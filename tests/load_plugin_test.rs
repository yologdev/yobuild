//! `load_plugin` (`[extend]`, design §14) through the binary and its web
//! page: a stub npm registry (and an empty MCP registry) on one axum
//! server serves MCP server packages packed here with `npm pack`, so the
//! install is the real `npm install --before` and `npm ci`, and no test
//! reaches the network. The owner's questions are answered over the
//! page's WebSocket. Unix, as `cli_test`.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::Path as UrlPath;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use futures::StreamExt;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

const DAY: u64 = 24 * 60 * 60;

/// `value` as a JSON response (axum's `json` feature is off).
#[allow(non_snake_case)]
fn Json(value: Value) -> Response {
    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        value.to_string(),
    )
        .into_response()
}

/// An ISO time `days` ago, as npm writes them.
fn ago(days: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let t = now - days * DAY;
    let z = (t / DAY) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let s = t % DAY;
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.000Z",
        s / 3600,
        s / 60 % 60,
        s % 60
    )
}

/// An MCP stdio server in Node offering `tools`: `echo` answers
/// `echoed <text>`, and exits when the text is `die`.
fn server_js(tools: &Value) -> String {
    format!(
        r#"#!/usr/bin/env node
const tools = {tools};
const say = (m) => process.stdout.write(JSON.stringify(m) + "\n");
require("readline").createInterface({{ input: process.stdin }}).on("line", (line) => {{
  const m = JSON.parse(line);
  if (m.id === undefined) return;
  if (m.method === "initialize") {{
    say({{ jsonrpc: "2.0", id: m.id, result: {{ protocolVersion: "2024-11-05",
      capabilities: {{ tools: {{}} }}, serverInfo: {{ name: "echo", version: "1" }} }} }});
  }} else if (m.method === "tools/list") {{
    say({{ jsonrpc: "2.0", id: m.id, result: {{ tools }} }});
  }} else if (m.method === "tools/call") {{
    const text = m.params.arguments.text;
    if (text === "die") {{ process.stderr.write("dying as asked\n"); process.exit(3); }}
    say({{ jsonrpc: "2.0", id: m.id, result: {{ content: [{{ type: "text", text: "echoed " + text }}] }} }});
  }} else {{
    say({{ jsonrpc: "2.0", id: m.id, error: {{ code: -32601, message: "no such method" }} }});
  }}
}});
"#
    )
}

/// One version of a package, packed: what its packument says of it, its
/// tarball, and when it was published.
struct Packed {
    version: String,
    days: u64,
    /// Its `package.json`.
    manifest: Value,
    tarball: Vec<u8>,
    integrity: String,
    shasum: String,
}

/// `name@version`, published `days` ago, packed with `npm pack`: an MCP
/// server offering `tools`, or with none a library; `dependencies` as
/// npm writes them.
fn pack(name: &str, version: &str, days: u64, tools: Option<Value>, dependencies: Value) -> Packed {
    let dir = tempfile::tempdir().unwrap();
    let package = dir.path().join("package");
    std::fs::create_dir(&package).unwrap();
    let mut manifest = json!({ "name": name, "version": version,
        "description": format!("{name} echoes"), "license": "MIT", "dependencies": dependencies });
    match &tools {
        Some(tools) => {
            manifest["bin"] = json!({ name: "server.js" });
            std::fs::write(package.join("server.js"), server_js(tools)).unwrap();
        }
        None => std::fs::write(package.join("index.js"), "module.exports = 1;\n").unwrap(),
    }
    std::fs::write(package.join("package.json"), manifest.to_string()).unwrap();
    let out = Command::new("npm")
        .args(["pack", "--json", "--pack-destination"])
        .arg(dir.path())
        .current_dir(&package)
        .env("HOME", dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "npm pack: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let said: Value = serde_json::from_slice(&out.stdout).unwrap();
    let said = &said[0];
    Packed {
        version: version.to_owned(),
        days,
        manifest,
        tarball: std::fs::read(dir.path().join(said["filename"].as_str().unwrap())).unwrap(),
        integrity: said["integrity"].as_str().unwrap().to_owned(),
        shasum: said["shasum"].as_str().unwrap().to_owned(),
    }
}

/// A server package at 1.0.0, 60 days old, with no dependencies.
fn server(name: &str, tools: Value) -> (String, Vec<Packed>) {
    (
        name.to_owned(),
        vec![pack(name, "1.0.0", 60, Some(tools), json!({}))],
    )
}

/// The packages the stub serves, by name: their versions, the last the
/// latest.
type Packages = Arc<BTreeMap<String, Vec<Packed>>>;

fn echo_tools() -> Value {
    json!([{ "name": "echo", "description": "Echoes its text",
             "inputSchema": { "type": "object", "properties": { "text": { "type": "string" } } } }])
}

/// The stub registries on a port of their own, serving until the process
/// ends.
fn stub(packages: Packages) -> u16 {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async move {
            let servers = || async { Json(json!({ "servers": [], "metadata": { "count": 0 } })) };
            let names: Vec<String> = packages.keys().cloned().collect();
            let search = move || {
                let names = names.clone();
                async move {
                    let objects: Vec<Value> = names
                        .iter()
                        .map(|name| {
                            json!({ "package": { "name": name, "version": "1.0.0",
                                "description": format!("{name} echoes"), "date": ago(60),
                                "links": {}, "maintainers": [] }, "score": { "final": 1.0 }, "searchScore": 1.0 })
                        })
                        .collect();
                    Json(json!({ "objects": objects, "total": names.len(), "time": ago(0) }))
                }
            };
            let port = Arc::new(std::sync::OnceLock::<u16>::new());
            let (at, served) = (port.clone(), packages.clone());
            let package = move |UrlPath(name): UrlPath<String>| async move {
                let Some(all) = served.get(&name) else {
                    return StatusCode::NOT_FOUND.into_response();
                };
                let port = at.get().unwrap();
                let mut versions = serde_json::Map::new();
                let mut time = serde_json::Map::new();
                for packed in all {
                    let v = &packed.version;
                    let mut entry = packed.manifest.clone();
                    entry["dist"] = json!({
                        "tarball": format!("http://127.0.0.1:{port}/{name}/-/{name}-{v}.tgz"),
                        "integrity": packed.integrity, "shasum": packed.shasum
                    });
                    versions.insert(v.clone(), entry);
                    time.insert(v.clone(), json!(ago(packed.days)));
                }
                let oldest = all.iter().map(|p| p.days).max().unwrap();
                time.insert("created".into(), json!(ago(oldest)));
                time.insert("modified".into(), json!(ago(all.last().unwrap().days)));
                Json(json!({
                    "name": name, "description": format!("{name} echoes"),
                    "dist-tags": { "latest": all.last().unwrap().version },
                    "versions": versions, "time": time
                }))
            };
            let served = packages.clone();
            let tarball = move |UrlPath((name, file)): UrlPath<(String, String)>| async move {
                let packed = served.get(&name).and_then(|all| {
                    all.iter()
                        .find(|p| file == format!("{name}-{}.tgz", p.version))
                });
                match packed {
                    Some(packed) => (
                        [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
                        packed.tarball.clone(),
                    )
                        .into_response(),
                    None => StatusCode::NOT_FOUND.into_response(),
                }
            };
            let app = Router::new()
                .route("/v0/servers", get(servers))
                .route("/-/v1/search", get(search))
                .route("/{name}", get(package))
                .route("/{name}/-/{file}", get(tarball));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let bound = listener.local_addr().unwrap().port();
            port.set(bound).unwrap();
            tx.send(bound).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });
    rx.recv().unwrap()
}

fn project(script: &Value) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("agent.toml"),
        "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n\n[frontend]\nweb = true\n\n[extend]\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("script.json"), script.to_string()).unwrap();
    std::fs::create_dir(dir.path().join("home")).unwrap();
    dir
}

/// The web page's session: `yobuild run` with the stubs and a home of its
/// own, connected over its WebSocket.
struct Page {
    child: Child,
    socket: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
}

impl Drop for Page {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Page {
    async fn open(dir: &Path, port: u16) -> Page {
        let mut command = Command::new(env!("CARGO_BIN_EXE_yobuild"));
        for key in yobuild::manifest::KEY_VARIABLES {
            command.env_remove(key);
        }
        let mut child = command
            .args([
                "run", "agent.toml", "--scripted", "script.json", "--web", "127.0.0.1:0", "go",
            ])
            .current_dir(dir)
            .env_remove("RUST_LOG")
            .env("HOME", dir.join("home"))
            .env("YOBUILD_HOME", dir.join("home/.yobuild"))
            .env("YOBUILD_MCP_REGISTRY", format!("http://127.0.0.1:{port}"))
            .env("npm_config_registry", format!("http://127.0.0.1:{port}/"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        out.read_line(&mut line).unwrap();
        std::thread::spawn(move || std::io::copy(&mut out, &mut std::io::sink()));
        let url = line.split_whitespace().nth(1).expect("the URL");
        let ws = url.replace("http://", "ws://").replace("/?t=", "/ws?t=");
        let (socket, _) = tokio_tungstenite::connect_async(ws).await.unwrap();
        Page { child, socket }
    }

    /// The next message, or `None` after `wait`.
    async fn next_within(&mut self, wait: Duration) -> Option<Value> {
        let next = async {
            while let Some(Ok(message)) = self.socket.next().await {
                if let Message::Text(text) = message {
                    return serde_json::from_str(&text).unwrap();
                }
            }
            panic!("the page closed")
        };
        tokio::time::timeout(wait, next).await.ok()
    }

    async fn send(&mut self, message: Value) {
        use futures::SinkExt;
        self.socket
            .send(Message::Text(message.to_string().into()))
            .await
            .unwrap();
    }

    /// Answers each question with the first of `answers` (title prefix →
    /// value) in order until the run ends, then reads on for `after` (a
    /// notice the watcher's tick gives). The questions' titles, the
    /// notices, and the tool results (`name ✓|✗ text`).
    async fn drive(&mut self, answers: &[(&str, Value)], after: Duration) -> Seen {
        let mut seen = Seen::default();
        let mut answers = answers.iter();
        let mut ended = false;
        loop {
            let wait = match ended {
                true => after,
                false => Duration::from_secs(120),
            };
            let Some(m) = self.next_within(wait).await else {
                assert!(ended, "no run ended in 120 s: {seen:#?}");
                return seen;
            };
            match m["type"].as_str() {
                Some("uiRequest") => {
                    let title = m["request"]["title"].as_str().unwrap().to_owned();
                    let (prefix, value) = answers
                        .next()
                        .unwrap_or_else(|| panic!("an unexpected question: {title}"));
                    assert!(title.starts_with(prefix), "{title:?} is not {prefix:?}");
                    seen.questions
                        .push((title, m["request"]["message"].as_str().unwrap().to_owned()));
                    self.send(json!({ "type": "uiResponse", "id": m["id"], "value": value }))
                        .await;
                }
                Some("notice") => seen.notices.push(format!(
                    "{}: {}",
                    m["level"].as_str().unwrap(),
                    m["message"].as_str().unwrap()
                )),
                Some("event") if m["event"]["type"] == "toolExecutionEnd" => {
                    let result = &m["event"]["result"];
                    let text: Vec<&str> = result["content"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|c| c["text"].as_str())
                        .collect();
                    seen.calls.push(format!(
                        "{} {} {}",
                        m["event"]["toolName"].as_str().unwrap(),
                        match m["event"]["isError"] == true {
                            true => "✗",
                            false => "✓",
                        },
                        text.join("\n")
                    ));
                }
                Some("runEnd") => {
                    assert_eq!(m["outcome"], "completed", "{m}");
                    ended = true;
                }
                _ => {}
            }
        }
    }

    /// Ctrl+C, then its exit code and stderr.
    fn interrupt(mut self) -> (Option<i32>, String) {
        use std::io::Read;
        let mut err = self.child.stderr.take().expect("piped");
        let reading = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = err.read_to_string(&mut text);
            text
        });
        let pid = self.child.id().to_string();
        Command::new("kill").args(["-INT", &pid]).status().unwrap();
        let status = self.child.wait().unwrap();
        (status.code(), reading.join().unwrap_or_default())
    }
}

#[derive(Debug, Default)]
struct Seen {
    questions: Vec<(String, String)>,
    notices: Vec<String>,
    calls: Vec<String>,
}

fn call(name: &str, arguments: Value) -> Value {
    json!({ "tools": [{ "name": name, "arguments": arguments }] })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_allowed_twice_is_offered_from_the_next_turn_each_call_asking() {
    // Its dependency's newest version is a day old: `--before` keeps the
    // tree to what is `min_age_days` old, not only the server itself.
    let packages: Packages = Arc::new(BTreeMap::from([
        (
            "echo-mcp".to_owned(),
            vec![pack(
                "echo-mcp",
                "1.0.0",
                60,
                Some(echo_tools()),
                json!({ "echo-dep": "^1.0.0" }),
            )],
        ),
        (
            "echo-dep".to_owned(),
            vec![
                pack("echo-dep", "1.0.0", 60, None, json!({})),
                pack("echo-dep", "1.1.0", 1, None, json!({})),
            ],
        ),
    ]));
    let port = stub(packages);
    let dir = project(&json!([
        call("find_plugin", json!({ "need": "echo text" })),
        call("load_plugin", json!({ "package": "echo-mcp", "version": "1.0.0", "args": ["../elsewhere"] })),
        call("echo__echo", json!({ "text": "hi" })),
        call("echo__echo", json!({ "text": "die" })),
        { "text": "done" }
    ]));
    let mut page = Page::open(dir.path(), port).await;
    let seen = page
        .drive(
            &[
                (
                    "Start the MCP server echo-mcp@1.0.0 as echo? (asked by the agent)",
                    json!("Allow"),
                ),
                ("Keep the MCP server echo? It offers: echo", json!("Allow")),
                ("Run `echo__echo`?", json!("Allow")),
                ("Run `echo__echo`?", json!("Allow")),
            ],
            Duration::from_secs(3),
        )
        .await;
    let (_, first) = &seen.questions[0];
    for said in [
        "The agent needs: echo text",
        "echo-mcp@1.0.0, its tools as echo__<tool>.",
        "licence: MIT",
        "its needs: not listed (found by npm only)",
        "Arguments: \"../elsewhere\" (outside the agent's folder)",
        "Install scripts: none (none are run)",
        "you will be asked again once they are.",
        "this is not a sandbox.",
    ] {
        assert!(first.contains(said), "{said:?} not in:\n{first}");
    }
    assert!(
        seen.questions[1].1.contains("echo__echo: Echoes its text"),
        "{seen:#?}"
    );
    let calls = &seen.calls;
    assert!(calls[0].starts_with("find_plugin ✓ "), "{calls:#?}");
    assert!(
        calls[1].starts_with("load_plugin ✓ Loaded echo-mcp@1.0.0 as echo, for this session. Its tools are yours from your next step: echo__echo."),
        "{calls:#?}"
    );
    assert_eq!(calls[2], "echo__echo ✓ echoed hi", "{calls:#?}");
    assert!(calls[3].starts_with("echo__echo ✗ "), "{calls:#?}");
    let notices = seen.notices.join("\n");
    assert!(
        notices.contains("info: The agent loaded echo-mcp@1.0.0 as echo, for this session. Loaded this session: echo-mcp@1.0.0 as echo (tools: echo). To keep it, add to agent.toml:"),
        "{notices}"
    );
    assert!(
        notices.contains("error: The loaded MCP server `echo` stopped: it exited with exit status: 3; last stderr: dying as asked"),
        "{notices}"
    );
    let (_, stderr) = page.interrupt();
    assert!(
        stderr.contains("Loaded this session: echo-mcp@1.0.0 as echo (tools: echo). To keep it, add to agent.toml:\n\n[[mcp]]\nname = \"echo\"\nnpm  = \"echo-mcp@1.0.0\"\nargs = [\"../elsewhere\"]\n# allow = [\"echo\"]"),
        "{stderr}"
    );
    // Nothing written beside the manifest.
    assert!(!dir.path().join("agent.lock").exists());
    // The dependency as it was a week ago, not its newest.
    let sets = dir.path().join("home/.yobuild/plugins/mcp");
    let installed: Vec<String> = std::fs::read_dir(&sets)
        .unwrap()
        .map(|e| e.unwrap().path().join("node_modules/echo-dep/package.json"))
        .filter(|p| p.exists())
        .map(|p| {
            let package: Value = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
            package["version"].as_str().unwrap().to_owned()
        })
        .collect();
    assert_eq!(installed, ["1.0.0"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn unanswered_is_not_remembered_block_is_and_a_server_with_no_tools_never_offered() {
    let packages: Packages = Arc::new(BTreeMap::from([
        server("echo-mcp", echo_tools()),
        server("empty-mcp", json!([])),
    ]));
    let port = stub(packages);
    let echo = json!({ "package": "echo-mcp", "version": "1.0.0" });
    let dir = project(&json!([
        call("find_plugin", json!({ "need": "echo" })),
        call("load_plugin", echo.clone()),
        call("load_plugin", echo.clone()),
        call("load_plugin", echo.clone()),
        call("load_plugin", json!({ "package": "empty-mcp", "version": "1.0.0" })),
        { "text": "done" }
    ]));
    let mut page = Page::open(dir.path(), port).await;
    let start = "Start the MCP server echo-mcp@1.0.0 as echo?";
    let seen = page
        .drive(
            &[
                (start, Value::Null),
                (start, json!("Block")),
                (
                    "Start the MCP server empty-mcp@1.0.0 as empty?",
                    json!("Allow"),
                ),
            ],
            Duration::from_millis(500),
        )
        .await;
    let calls = &seen.calls;
    assert!(
        calls[1].starts_with(
            "load_plugin ✗ load_plugin refused it: nobody answered whether to start it"
        ),
        "{calls:#?}"
    );
    assert!(
        calls[2].contains(
            "the owner blocked it (start it): do not propose echo-mcp again this session"
        ),
        "{calls:#?}"
    );
    assert!(
        calls[3].contains("the owner blocked echo-mcp earlier this session"),
        "{calls:#?}"
    );
    assert!(
        calls[4].contains("load_plugin refused it: empty-mcp: it lists no tools"),
        "{calls:#?}"
    );
    let notices = seen.notices.join("\n");
    assert!(
        notices.contains("warning: The agent's plugin `echo-mcp` was refused: nobody answered"),
        "{notices}"
    );
    assert!(
        notices.contains(
            "error: The agent's plugin `empty-mcp` was refused: empty-mcp: it lists no tools"
        ),
        "{notices}"
    );
    let (_, stderr) = page.interrupt();
    assert!(!stderr.contains("Loaded this session"), "{stderr}");
}
