//! `load_plugin` (`[extend]`, design §14) through the binary and its web
//! page: a stub npm registry (and an empty MCP registry) on one axum
//! server serves MCP server and pi packages packed here with `npm pack`, so
//! the install is the real `npm install --before` and `npm ci`, and no test
//! reaches the network. The owner's questions are answered over the
//! page's WebSocket. A pi package runs on the checkout's pi adapter
//! (`YOBUILD_RUNTIMES=checkout`; skipped without its `node_modules` unless
//! `YOAGENT_RUTIS_REQUIRE_RUNTIMES=1`). Unix, as `cli_test`.
#![cfg(unix)]

mod common;

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

use common::registry::{self, stub, Packages, Packed};

/// An MCP stdio server in Node offering `tools`: `echo` answers
/// `echoed <text>`, and exits when the text is `die` (at once) or
/// `die-soon` (0.3 s after answering, idle). As it starts it
/// leaves `echo-<pid>.pid` in its home, so a test can tell it stopped.
fn server_js(tools: &Value) -> String {
    format!(
        r#"#!/usr/bin/env node
const tools = {tools};
require("fs").writeFileSync(require("path").join(require("os").homedir(), "echo-" + process.pid + ".pid"), "");
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
    if (text === "die-soon") setTimeout(() => {{ process.stderr.write("dying idle\n"); process.exit(4); }}, 300);
    say({{ jsonrpc: "2.0", id: m.id, result: {{ content: [{{ type: "text", text: "echoed " + text }}] }} }});
  }} else {{
    say({{ jsonrpc: "2.0", id: m.id, error: {{ code: -32601, message: "no such method" }} }});
  }}
}});
"#
    )
}

/// A `postinstall` that would leave `postinstall-ran` in the home: the
/// install must never run it.
const POSTINSTALL: &str =
    "node -e \"require('fs').writeFileSync(require('os').homedir()+'/postinstall-ran','')\"";

/// `name@version`, published `days` ago: an MCP server offering `tools`,
/// or with none a library; `extra` merged into its `package.json`
/// (`dependencies`, `scripts`).
fn pack(name: &str, version: &str, days: u64, tools: Option<Value>, extra: Value) -> Packed {
    let mut extra = extra;
    let files = match &tools {
        Some(tools) => {
            extra["bin"] = json!({ name: "server.js" });
            vec![("server.js", server_js(tools))]
        }
        None => vec![("index.js", "module.exports = 1;\n".to_owned())],
    };
    registry::pack(name, version, days, &files, extra)
}

/// A server package at 1.0.0, 60 days old.
fn server(name: &str, tools: Value) -> (String, Vec<Packed>) {
    (
        name.to_owned(),
        vec![pack(name, "1.0.0", 60, Some(tools), json!({}))],
    )
}

fn echo_tools() -> Value {
    json!([{ "name": "echo", "description": "Echoes its text",
             "inputSchema": { "type": "object", "properties": { "text": { "type": "string" } } } }])
}
fn project(extend: &str, script: &Value) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("agent.toml"),
        format!("[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n\n[frontend]\nweb = true\n\n[extend]\n{extend}"),
    )
    .unwrap();
    std::fs::write(dir.path().join("script.json"), script.to_string()).unwrap();
    std::fs::create_dir(dir.path().join("home")).unwrap();
    dir
}

/// The servers that started (each left its pid in the home).
fn pids(home: &Path) -> Vec<u32> {
    std::fs::read_dir(home)
        .unwrap()
        .filter_map(|e| {
            let name = e.unwrap().file_name().into_string().unwrap();
            name.strip_prefix("echo-")?
                .strip_suffix(".pid")?
                .parse()
                .ok()
        })
        .collect()
}

/// Whether `pid` still runs (a zombie, killed but not yet reaped, does
/// not).
fn running(pid: u32) -> bool {
    let out = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let stat = String::from_utf8_lossy(&out.stdout);
    !stat.trim().is_empty() && !stat.trim().starts_with('Z')
}

/// Every server that started has stopped, within 5 s.
fn all_stopped(home: &Path) {
    for _ in 0..50 {
        if !pids(home).into_iter().any(running) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("still running: {:?}", pids(home));
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

#[derive(Debug, Default)]
struct Seen {
    questions: Vec<(String, String)>,
    notices: Vec<String>,
    calls: Vec<String>,
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
            // The checkout's adapters: the cache's would be installed from
            // the stub registry, which has none of their packages.
            .env("YOBUILD_RUNTIMES", "checkout")
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

    /// Answers each question with the next of `answers` (title prefix →
    /// value), in order, until a run ended `completed` and a notice holding
    /// each of `until` came (the watcher's tick gives some; 10 s at most),
    /// then reads on for `after`. Every answer must be used, and no other
    /// question come. The questions (title and details), the notices
    /// (`level: text`), and the tool results (`name ✓|✗ text`).
    async fn drive(&mut self, answers: &[(&str, Value)], until: &[&str], after: Duration) -> Seen {
        let mut seen = Seen::default();
        let mut left = answers.iter();
        let mut ended = false;
        loop {
            let waiting = until
                .iter()
                .any(|u| !seen.notices.iter().any(|n| n.contains(u)));
            let wait = match (ended, waiting) {
                (true, false) => after,
                (true, true) => Duration::from_secs(10),
                (false, _) => Duration::from_secs(120),
            };
            let Some(m) = self.next_within(wait).await else {
                assert!(ended, "no run ended in 120 s: {seen:#?}");
                assert!(!waiting, "a notice of {until:?} never came: {seen:#?}");
                break;
            };
            match m["type"].as_str() {
                Some("uiRequest") => {
                    let title = m["request"]["title"].as_str().unwrap().to_owned();
                    let (prefix, value) = left
                        .next()
                        .unwrap_or_else(|| panic!("an unexpected question: {title}"));
                    assert!(title.starts_with(prefix), "{title:?} is not {prefix:?}");
                    seen.questions
                        .push((title, m["request"]["message"].as_str().unwrap().to_owned()));
                    self.send(json!({ "type": "uiResponse", "id": m["id"], "value": value }))
                        .await;
                }
                Some("notice") => seen.notices.push(notice(&m)),
                Some("event") if m["event"]["type"] == "toolExecutionEnd" => {
                    seen.calls.push(call_said(&m))
                }
                Some("runEnd") => {
                    assert_eq!(m["outcome"], "completed", "{m}");
                    ended = true;
                }
                _ => {}
            }
        }
        assert_eq!(
            seen.questions.len(),
            answers.len(),
            "answers left unused: {seen:#?}"
        );
        seen
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

fn notice(m: &Value) -> String {
    format!(
        "{}: {}",
        m["level"].as_str().unwrap(),
        m["message"].as_str().unwrap()
    )
}

/// A tool's end as `name ✓|✗ text`.
fn call_said(m: &Value) -> String {
    let text: Vec<&str> = m["event"]["result"]["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["text"].as_str())
        .collect();
    format!(
        "{} {} {}",
        m["event"]["toolName"].as_str().unwrap(),
        match m["event"]["isError"] == true {
            true => "✗",
            false => "✓",
        },
        text.join("\n")
    )
}

fn call(name: &str, arguments: Value) -> Value {
    json!({ "tools": [{ "name": name, "arguments": arguments }] })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_allowed_twice_is_offered_from_the_next_turn_each_call_asking() {
    // Its dependency's newest version is a day old: `--before` keeps the
    // tree to what is `min_age_days` old, not only the server itself. Its
    // own `postinstall` would leave a file: never run.
    let packages: Packages = Arc::new(BTreeMap::from([
        (
            "echo-mcp".to_owned(),
            vec![pack(
                "echo-mcp",
                "1.0.0",
                60,
                Some(echo_tools()),
                json!({ "dependencies": { "echo-dep": "^1.0.0" },
                        "scripts": { "postinstall": POSTINSTALL } }),
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
    let port = stub(packages, &[]).port;
    let dir = project(
        "",
        &json!([
            call("find_plugin", json!({ "need": "echo text" })),
            // No `version`: the one find_plugin offered.
            call("load_plugin", json!({ "package": "echo-mcp",
                                        "name": "talk", "args": ["../elsewhere"] })),
            call("talk__echo", json!({ "text": "hi" })),
            call("talk__echo", json!({ "text": "die" })),
            call("talk__echo", json!({ "text": "again" })),
            { "text": "done" }
        ]),
    );
    let home = dir.path().join("home");
    let mut page = Page::open(dir.path(), port).await;
    let asked = "Run `talk__echo`, a tool of a server loaded this session?";
    let seen = page
        .drive(
            &[
                (
                    "Start the MCP server echo-mcp@1.0.0 as talk? (asked by the agent)",
                    json!("Allow"),
                ),
                ("Keep the MCP server talk? It offers: echo", json!("Allow")),
                (asked, json!("Allow")),
                (asked, json!("Allow")),
                // After it stopped: still asked by its name (were it freed,
                // a plugin's tool of that name could run unasked), then
                // not found.
                (asked, json!("Allow")),
            ],
            &["error: The loaded MCP server `talk` stopped"],
            Duration::from_millis(300),
        )
        .await;
    let (_, first) = &seen.questions[0];
    for said in [
        "The agent needs: echo text",
        "echo-mcp@1.0.0, its tools as talk__<tool>.",
        "licence: MIT",
        "its needs: not listed (found by npm only)",
        "Arguments: \"../elsewhere\" (outside the agent's folder)",
        "Install scripts it would have run (not run): echo-mcp",
        "you will be asked again once they are.",
        "this is not a sandbox.",
    ] {
        assert!(first.contains(said), "{said:?} not in:\n{first}");
    }
    assert!(!home.join("postinstall-ran").exists(), "a script ran");
    assert!(
        seen.questions[1].1.contains("talk__echo: Echoes its text"),
        "{seen:#?}"
    );
    // Every call asked, with the loaded tool's own question.
    for (title, _) in &seen.questions[2..] {
        assert_eq!(title, asked);
    }
    let calls = &seen.calls;
    assert!(calls[0].starts_with("find_plugin ✓ "), "{calls:#?}");
    assert!(
        calls[1].starts_with("load_plugin ✓ Loaded echo-mcp@1.0.0 as talk, for this session. Its tools are yours from your next step: talk__echo."),
        "{calls:#?}"
    );
    assert_eq!(calls[2], "talk__echo ✓ echoed hi", "{calls:#?}");
    assert!(calls[3].starts_with("talk__echo ✗ "), "{calls:#?}");
    // Its tools are gone at the next turn: not offered, so not found.
    assert_eq!(
        calls[4], "talk__echo ✗ Tool talk__echo not found",
        "{calls:#?}"
    );
    let notices = seen.notices.join("\n");
    assert!(
        notices.contains("info: The agent loaded echo-mcp@1.0.0 as talk, for this session. Loaded this session: echo-mcp@1.0.0 as talk (tools: echo). To keep it, add to agent.toml:"),
        "{notices}"
    );
    assert!(
        notices.contains("error: The loaded MCP server `talk` stopped: it exited with exit status: 3; last stderr: dying as asked"),
        "{notices}"
    );
    let (_, stderr) = page.interrupt();
    assert!(
        stderr.contains("Loaded this session: echo-mcp@1.0.0 as talk (tools: echo, stopped this session: it exited with exit status: 3; last stderr: dying as asked). To keep it, add to agent.toml:\n\n[[mcp]]\nname = \"talk\"\nnpm  = \"echo-mcp@1.0.0\"\nargs = [\"../elsewhere\"]\n# allow = [\"echo\"]"),
        "{stderr}"
    );
    assert!(
        stderr.contains("and writes agent.lock, which --locked needs."),
        "{stderr}"
    );
    // Nothing written beside the manifest; no server left running.
    assert!(!dir.path().join("agent.lock").exists());
    all_stopped(&home);
    // The dependency as it was a week ago, not its newest.
    let sets = home.join(".yobuild/plugins/mcp");
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

/// A loaded server that dies while idle, between runs: said by the
/// watcher's tick with no call made, its tool gone from the next run (still
/// asked by its name, then not found), and the block at the end marking it.
#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_dies_while_idle_is_said_with_no_call_made() {
    let packages: Packages = Arc::new(BTreeMap::from([(
        "echo-mcp".to_owned(),
        vec![pack("echo-mcp", "1.0.0", 60, Some(echo_tools()), json!({}))],
    )]));
    let port = stub(packages, &[]).port;
    let dir = project(
        "",
        &json!([
            call("find_plugin", json!({ "need": "echo text" })),
            call("load_plugin", json!({ "package": "echo-mcp", "name": "talk" })),
            call("talk__echo", json!({ "text": "die-soon" })),
            { "text": "loaded" },
            call("talk__echo", json!({ "text": "again" })),
            { "text": "done" }
        ]),
    );
    let home = dir.path().join("home");
    let mut page = Page::open(dir.path(), port).await;
    let asked = "Run `talk__echo`, a tool of a server loaded this session?";
    let died = "error: The loaded MCP server `talk` stopped: it exited with exit status: 4; last stderr: dying idle";
    let first = page
        .drive(
            &[
                (
                    "Start the MCP server echo-mcp@1.0.0 as talk?",
                    json!("Allow"),
                ),
                ("Keep the MCP server talk?", json!("Allow")),
                (asked, json!("Allow")),
            ],
            &[died],
            Duration::from_millis(300),
        )
        .await;
    assert_eq!(first.calls[2], "talk__echo ✓ echoed die-soon", "{first:#?}");
    page.send(json!({ "type": "prompt", "text": "again" }))
        .await;
    let second = page
        .drive(&[(asked, json!("Allow"))], &[], Duration::from_millis(300))
        .await;
    assert_eq!(
        second.calls,
        ["talk__echo ✗ Tool talk__echo not found"],
        "{second:#?}"
    );
    let (_, stderr) = page.interrupt();
    assert!(
        stderr.contains("as talk (tools: echo, stopped this session: it exited with exit status: 4; last stderr: dying idle)"),
        "{stderr}"
    );
    all_stopped(&home);
}

#[tokio::test(flavor = "multi_thread")]
async fn unanswered_is_not_remembered_block_is_at_either_question_and_no_tools_never_offered() {
    let packages: Packages = Arc::new(BTreeMap::from([
        server("echo-mcp", echo_tools()),
        server("empty-mcp", json!([])),
    ]));
    let port = stub(packages, &[]).port;
    let echo = json!({ "package": "echo-mcp", "version": "1.0.0" });
    let dir = project(
        "max_attempts = 10\n",
        &json!([
            call("find_plugin", json!({ "need": "echo" })),
            call("load_plugin", echo.clone()),
            call("load_plugin", echo.clone()),
            call("load_plugin", echo.clone()),
            call("load_plugin", echo.clone()),
            call("load_plugin", json!({ "package": "empty-mcp", "version": "1.0.0" })),
            { "text": "done" }
        ]),
    );
    let home = dir.path().join("home");
    let mut page = Page::open(dir.path(), port).await;
    let start = "Start the MCP server echo-mcp@1.0.0 as echo?";
    let keep = "Keep the MCP server echo? It offers: echo";
    let seen = page
        .drive(
            &[
                (start, Value::Null),
                (start, json!("Allow")),
                (keep, Value::Null),
                (start, json!("Allow")),
                (keep, json!("Block")),
                (
                    "Start the MCP server empty-mcp@1.0.0 as empty?",
                    json!("Allow"),
                ),
            ],
            &[],
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
        calls[2].contains("nobody answered whether to keep it"),
        "{calls:#?}"
    );
    assert!(
        calls[3]
            .contains("the owner blocked it (keep it): do not propose echo-mcp again this session"),
        "{calls:#?}"
    );
    assert!(
        calls[4].contains("the owner blocked echo-mcp earlier this session"),
        "{calls:#?}"
    );
    assert!(
        calls[5].contains("load_plugin refused it: empty-mcp: it lists no tools"),
        "{calls:#?}"
    );
    let notices = seen.notices.join("\n");
    assert!(
        notices.contains("warning: The agent's plugin `echo-mcp` was refused: nobody answered whether to start it"),
        "{notices}"
    );
    assert!(
        notices.contains(
            "warning: The agent's plugin `echo-mcp` was refused: the owner blocked it (keep it)"
        ),
        "{notices}"
    );
    assert!(
        notices.contains(
            "error: The agent's plugin `empty-mcp` was refused: empty-mcp: it lists no tools"
        ),
        "{notices}"
    );
    // The three that started (two not kept, one without tools) stopped.
    assert_eq!(pids(&home).len(), 3);
    all_stopped(&home);
    let (_, stderr) = page.interrupt();
    assert!(!stderr.contains("Loaded this session"), "{stderr}");
}

/// A run aborted while the server installs stops npm and leaves no
/// partial folder; one aborted while the owner is asked withdraws the
/// question; neither leaves a load "in progress".
#[tokio::test(flavor = "multi_thread")]
async fn a_run_aborted_mid_load_stops_it_where_it_is() {
    let packages: Packages = Arc::new(BTreeMap::from([
        server("echo-mcp", echo_tools()),
        server("slow-mcp", echo_tools()),
    ]));
    let stub = stub(packages, &["slow-mcp"]);
    let (port, fetched) = (stub.port, stub.tarballs);
    let echo = json!({ "package": "echo-mcp", "version": "1.0.0" });
    let dir = project(
        "",
        &json!([
            call("find_plugin", json!({ "need": "echo" })),
            call("load_plugin", json!({ "package": "slow-mcp", "version": "1.0.0" })),
            call("load_plugin", echo.clone()),
            call("load_plugin", echo.clone()),
            { "text": "done" }
        ]),
    );
    let sets = dir.path().join("home/.yobuild/plugins/mcp");
    let mut page = Page::open(dir.path(), port).await;
    let mut runs = Vec::new();
    let mut calls = Vec::new();
    let mut withdrawn = false;
    let mut asked = None;
    while runs.len() < 3 {
        let Some(m) = page.next_within(Duration::from_millis(200)).await else {
            // Aborted while npm fetches the slow tarball.
            if runs.is_empty() && fetched.lock().unwrap().iter().any(|n| n == "slow-mcp") {
                page.send(json!({ "type": "abort" })).await;
                fetched.lock().unwrap().clear();
            }
            continue;
        };
        match m["type"].as_str() {
            Some("uiRequest") => {
                let title = m["request"]["title"].as_str().unwrap();
                assert!(title.starts_with("Start the MCP server echo-mcp"), "{m}");
                match runs.len() {
                    // Aborted while the owner is asked.
                    1 => {
                        asked = Some(m["id"].clone());
                        page.send(json!({ "type": "abort" })).await;
                    }
                    _ => {
                        page.send(json!({ "type": "uiResponse", "id": m["id"], "value": "Block" }))
                            .await
                    }
                }
            }
            Some("uiResolved") if Some(&m["id"]) == asked.as_ref() => {
                assert_eq!(m["reason"], "withdrawn", "{m}");
                withdrawn = true;
            }
            Some("event") if m["event"]["type"] == "toolExecutionEnd" => calls.push(call_said(&m)),
            Some("runEnd") => {
                runs.push(m["outcome"].as_str().unwrap().to_owned());
                if runs.len() == 1 {
                    // npm stopped, its partial folder gone (within 5 s).
                    let mut left = Vec::new();
                    for _ in 0..50 {
                        left = std::fs::read_dir(&sets)
                            .map(|d| {
                                d.map(|e| e.unwrap().file_name().into_string().unwrap())
                                    .filter(|n| n.contains(".partial-"))
                                    .collect()
                            })
                            .unwrap_or_default();
                        if left.is_empty() {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    assert!(left.is_empty(), "left behind: {left:?}");
                }
                if runs.len() < 3 {
                    page.send(json!({ "type": "prompt", "text": "again" }))
                        .await;
                }
            }
            _ => {}
        }
    }
    assert_eq!(runs, ["aborted", "aborted", "completed"], "{calls:#?}");
    assert!(withdrawn, "the question was not withdrawn");
    // The third load was asked, not refused as "in progress".
    let last = calls.last().unwrap();
    assert!(
        last.contains("the owner blocked it (start it)"),
        "{calls:#?}"
    );
    assert!(
        !calls
            .iter()
            .any(|c| c.contains("another load is in progress")),
        "{calls:#?}"
    );
    drop(page);
}

/// A pi package: `shout` answers its text in capitals and, for `die`,
/// ends its runtime; its own `tool_call` policy blocks `no` — and every
/// other tool, were it called for them — and its `tool_result` rewrites
/// every result, were it called. `typebox` is pi's own (a peer).
fn pi_shout(name: &str) -> (String, Vec<Packed>) {
    let ext = r#"import { Type } from 'typebox'
export default function (pi) {
  pi.registerTool({
    name: 'shout', label: 'Shout', description: 'Says the text in capitals.',
    parameters: Type.Object({ text: Type.String() }),
    async execute(_id, params) {
      if (params.text === 'die') setTimeout(() => process.exit(7), 50)
      return { content: [{ type: 'text', text: params.text.toUpperCase() }], details: {} }
    },
  })
  pi.on('tool_call', async (event) =>
    event.toolName !== 'shout' || event.input.text === 'no'
      ? { block: true, reason: 'pi-shout blocks it' }
      : undefined)
  pi.on('tool_result', async () => ({ content: [{ type: 'text', text: 'rewritten by pi-shout' }] }))
}
"#;
    (
        name.to_owned(),
        vec![registry::pack(
            name,
            "1.0.0",
            60,
            &[("ext.js", ext.to_owned())],
            json!({ "type": "module", "keywords": ["pi-package"],
                    "pi": { "extensions": ["./ext.js"] },
                    "peerDependencies": { "typebox": "*" } }),
        )],
    )
}

/// Whether the checkout's pi adapter can run (CI requires it).
fn pi_adapter() -> bool {
    let there = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("adapters/pi/node_modules")
        .is_dir();
    if !there && std::env::var_os("YOAGENT_RUTIS_REQUIRE_RUNTIMES").is_some() {
        panic!("adapters/pi/node_modules is missing: run scripts/try-coding-agent.sh --setup-only");
    }
    there
}

/// A pi package loaded mid-run in a restricted runtime of its own: its
/// tool offered from the next turn as `<name>__<tool>`, each call asking;
/// its own policy reached for its own calls only, its `tool_result` never;
/// offered again in the next run; its runtime dying said, its tool gone.
#[tokio::test(flavor = "multi_thread")]
async fn a_pi_package_runs_restricted_with_its_tools_only() {
    if !pi_adapter() {
        return;
    }
    let packages: Packages = Arc::new(BTreeMap::from([pi_shout("pi-shout")]));
    let port = stub(packages, &[]).port;
    let dir = project(
        "kinds = [\"pi\"]\n",
        &json!([
            call("find_plugin", json!({ "need": "shout" })),
            call("load_plugin", json!({ "package": "pi-shout", "name": "loud" })),
            call("loud__shout", json!({ "text": "hi" })),
            call("loud__shout", json!({ "text": "no" })),
            call("list_files", json!({})),
            { "text": "first" },
            call("loud__shout", json!({ "text": "again" })),
            call("loud__shout", json!({ "text": "die" })),
            { "text": "second" },
            call("loud__shout", json!({ "text": "gone" })),
            { "text": "third" }
        ]),
    );
    let mut page = Page::open(dir.path(), port).await;
    let asked = "Run `loud__shout`, a tool of a server loaded this session?";
    let first = page
        .drive(
            &[
                (
                    "Start the pi package pi-shout@1.0.0 as loud? (asked by the agent)",
                    json!("Allow"),
                ),
                ("Keep the pi package loud? It offers: shout", json!("Allow")),
                (asked, json!("Allow")),
                (asked, json!("Allow")),
            ],
            &[],
            Duration::from_millis(300),
        )
        .await;
    let (_, details) = &first.questions[0];
    assert!(
        details.contains("It runs in a runtime of its own with its tools only: its hooks are not called, and it gets no grants, config or host services."),
        "{details}"
    );
    assert!(
        first.questions[1]
            .1
            .contains("loud__shout: Says the text in capitals."),
        "{first:#?}"
    );
    let calls = &first.calls;
    assert!(calls[0].starts_with("find_plugin ✓ "), "{calls:#?}");
    assert!(
        calls[1].starts_with("load_plugin ✓ Loaded pi-shout@1.0.0 as loud, for this session. Its tools are yours from your next step: loud__shout."),
        "{calls:#?}"
    );
    // Its result as it made it: its `tool_result` is not called.
    assert_eq!(calls[2], "loud__shout ✓ HI", "{calls:#?}");
    // Its own policy judges its own call...
    assert!(
        calls[3].starts_with("loud__shout ✗ ") && calls[3].contains("pi-shout blocks it"),
        "{calls:#?}"
    );
    // ...and no other.
    assert!(calls[4].starts_with("list_files ✓ "), "{calls:#?}");
    let notices = first.notices.join("\n");
    assert!(
        notices.contains("info: The agent loaded pi-shout@1.0.0 as loud, for this session. Loaded this session: pi-shout@1.0.0 as loud (tools: shout). To keep it, add to agent.toml:\n\n[[plugin]]\nname = \"loud\"\npi   = \"pi-shout@1.0.0\"\n"),
        "{notices}"
    );

    // The next run offers it again; then its runtime dies, said.
    page.send(json!({ "type": "prompt", "text": "again" }))
        .await;
    let second = page
        .drive(
            &[(asked, json!("Allow")), (asked, json!("Allow"))],
            &["error: The loaded pi package `loud` stopped: "],
            Duration::from_millis(300),
        )
        .await;
    assert_eq!(second.calls[0], "loud__shout ✓ AGAIN", "{second:#?}");
    // Gone from the run after.
    page.send(json!({ "type": "prompt", "text": "gone" })).await;
    let third = page
        .drive(&[(asked, json!("Allow"))], &[], Duration::from_millis(300))
        .await;
    assert_eq!(
        third.calls,
        ["loud__shout ✗ Tool loud__shout not found"],
        "{third:#?}"
    );
    let (_, stderr) = page.interrupt();
    assert!(
        stderr.contains(
            "Loaded this session: pi-shout@1.0.0 as loud (tools: shout, stopped this session: "
        ),
        "{stderr}"
    );
    assert!(
        stderr.contains("In agent.toml a pi package runs with the other pi plugins, in the `pi` runtime, its hooks too, its tools under their own names.\n"),
        "{stderr}"
    );
    assert!(
        stderr.contains("(the lock's `pi` set, without the age limit)"),
        "{stderr}"
    );
    assert!(!dir.path().join("agent.lock").exists());
}

/// A pi package blocked at the second question: its runtime removed, its
/// tool never offered (the late source holds it for the run; `Loaded`
/// passes on only a kept package's).
#[tokio::test(flavor = "multi_thread")]
async fn a_pi_package_blocked_with_its_tools_in_front_of_the_owner_is_never_offered() {
    if !pi_adapter() {
        return;
    }
    let packages: Packages = Arc::new(BTreeMap::from([pi_shout("pi-shout")]));
    let port = stub(packages, &[]).port;
    let dir = project(
        "kinds = [\"pi\"]\n",
        &json!([
            call("find_plugin", json!({ "need": "shout" })),
            call("load_plugin", json!({ "package": "pi-shout" })),
            call("pi-shout__shout", json!({ "text": "hi" })),
            { "text": "done" }
        ]),
    );
    let mut page = Page::open(dir.path(), port).await;
    let seen = page
        .drive(
            &[
                (
                    "Start the pi package pi-shout@1.0.0 as pi-shout?",
                    json!("Allow"),
                ),
                (
                    "Keep the pi package pi-shout? It offers: shout",
                    json!("Block"),
                ),
                // Any tool asks; this one, then, is not found.
                ("Run `pi-shout__shout`?", json!("Allow")),
            ],
            &[],
            Duration::from_millis(300),
        )
        .await;
    assert!(
        seen.calls[1].contains("the owner blocked it (keep it)"),
        "{seen:#?}"
    );
    assert_eq!(
        seen.calls[2], "pi-shout__shout ✗ Tool pi-shout__shout not found",
        "{seen:#?}"
    );
    let (_, stderr) = page.interrupt();
    assert!(!stderr.contains("Loaded this session"), "{stderr}");
}
