//! `yobuild::run::run` end to end, `once`, from manifests, the model's
//! replies from a script: MCP over stdio (a bash server) and HTTP, DSH
//! plugins (files, a pinned package, the service check), limits, the
//! report, `ask_owner`, tool-name refusals, warnings. In this process's
//! folder, where nothing is written (pi, which writes there, is tested
//! through the binary: `cli_test`). Unix (the server is bash).
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use rutis::Ctx;
use yoagent::{AgentMessage, Content, Message};
use yobuild::frontend::RunOutcome;
use yobuild::manifest::Manifest;
use yobuild::run::{run, Ended, Options};

/// Answers `initialize`, lists one tool (`echo`), and answers each call
/// with its text, `$MODE` and its first argument; `TOOLS=none` lists none.
const SERVER: &str = r#"#!/usr/bin/env bash
id_of() { printf '%s' "$1" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p'; }
while read -r line; do
  id=$(id_of "$line")
  case "$line" in
    *'"initialize"'*)
      echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"echo\",\"version\":\"1\"}}}" ;;
    *'"tools/list"'*)
      if [ "$TOOLS" = none ]; then echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"tools\":[]}}"; continue; fi
      echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"tools\":[{\"name\":\"echo\",\"description\":\"Echo\",\"inputSchema\":{\"type\":\"object\",\"properties\":{\"text\":{\"type\":\"string\"}}}}]}}" ;;
    *'"tools/call"'*)
      text=$(printf '%s' "$line" | sed -n 's/.*"text":"\([^"]*\)".*/\1/p')
      if [ "$MODE" = split ]; then echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"results:\"},{\"type\":\"text\",\"text\":\"echoed $text\"}]}}"; continue; fi
      echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"echoed $text ($MODE, $ARG1)\"}]}}" ;;
  esac
done
"#;

const SCRIPT: &str = r#"[
  { "tools": [{ "name": "server__echo", "arguments": { "text": "hi" } }] },
  { "text": "done" }
]"#;

/// A project with the server, the script, and a manifest ending in `extra`.
fn project(extra: &str) -> tempfile::TempDir {
    project_with(extra, SCRIPT)
}

/// A project as [`project`], the model's replies from `script`.
fn project_with(extra: &str, script: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let server = dir.path().join("server.sh");
    // `$1` is the first argument the manifest gives.
    std::fs::write(&server, SERVER.replace("$ARG1", "$1")).unwrap();
    std::fs::set_permissions(&server, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(dir.path().join("script.json"), script).unwrap();
    std::fs::write(
        dir.path().join("agent.toml"),
        format!(
            "[agent]\nname = \"echoer\"\nmodel = \"deepseek/deepseek-flash\"\n\n\
             [[mcp]]\nnpm = \"./server.sh\"\nargs = [\"first\"]\nenv = {{ MODE = \"loud\" }}\n\n{extra}"
        ),
    )
    .unwrap();
    dir
}

/// The tests' cache of installed packages: shared by every test (each set
/// is installed from npm once), kept across runs of the suite.
const CACHE: &str = concat!(env!("CARGO_TARGET_TMPDIR"), "/yobuild-home");

/// Runs the project's agent once on its script.
async fn try_once(dir: &Path) -> Result<Ended, String> {
    try_once_locked(dir, false).await
}

/// Runs the project's agent once on its script; `locked`: refuse a lock
/// that is not current.
async fn try_once_locked(dir: &Path, locked: bool) -> Result<Ended, String> {
    let manifest = Manifest::load(dir.join("agent.toml")).map_err(|e| e.to_string())?;
    let mut options = Options::new(manifest);
    options.home = Some(CACHE.into());
    options.locked = locked;
    options.scripted = Some(dir.join("script.json"));
    options.task = Some("echo hi".into());
    options.once = true;
    let root = Ctx::root().unwrap();
    let ended = run(&root, options).await;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), root.shutdown()).await;
    ended.map_err(|e| e.to_string())
}

async fn run_once(dir: &Path) -> Ended {
    try_once(dir).await.unwrap()
}

/// What the agent said, in order.
fn answers(ended: &Ended) -> Vec<String> {
    ended
        .agent
        .as_ref()
        .expect("a once run keeps its agent")
        .messages()
        .iter()
        .filter_map(|m| match m {
            AgentMessage::Llm(Message::Assistant { content, .. }) => Some(
                content
                    .iter()
                    .filter_map(|c| match c {
                        Content::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .filter(|t| !t.is_empty())
        .collect()
}

/// The text of each tool result, and whether it was an error.
fn results(ended: &Ended) -> Vec<(String, bool)> {
    ended
        .agent
        .as_ref()
        .expect("a once run keeps its agent")
        .messages()
        .iter()
        .filter_map(|m| match m {
            AgentMessage::Llm(Message::ToolResult {
                content, is_error, ..
            }) => Some((
                content
                    .iter()
                    .filter_map(|c| match c {
                        Content::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect(),
                *is_error,
            )),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_manifest_runs_an_mcp_servers_tool() {
    let dir = project("[builtins]\ngate = \"off\"\n");
    let ended = run_once(dir.path()).await;
    assert_eq!(
        ended.outcome,
        Some(RunOutcome::Completed),
        "{:?}",
        ended.error
    );
    assert_eq!(
        results(&ended),
        [("echoed hi (loud, first)".to_string(), false)]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tool_results_text_blocks_all_reach_the_agent() {
    // A server answering in two text blocks: both kept, in order (yoagent
    // 0.25.9's providers send the model every one; 0.25.7 sent only the
    // first, and ESLint's MCP server's findings were lost).
    let dir = project_with("[builtins]\ngate = \"off\"\n", SCRIPT);
    let manifest = std::fs::read_to_string(dir.path().join("agent.toml")).unwrap();
    std::fs::write(
        dir.path().join("agent.toml"),
        manifest.replace("MODE = \"loud\"", "MODE = \"split\""),
    )
    .unwrap();
    let ended = run_once(dir.path()).await;
    assert_eq!(
        ended.outcome,
        Some(RunOutcome::Completed),
        "{:?}",
        ended.error
    );
    let blocks: Vec<Vec<Content>> = ended
        .agent
        .as_ref()
        .unwrap()
        .messages()
        .iter()
        .filter_map(|m| match m {
            AgentMessage::Llm(Message::ToolResult { content, .. }) => Some(content.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        blocks,
        [vec![
            Content::Text {
                text: "results:".into()
            },
            Content::Text {
                text: "echoed hi".into()
            }
        ]]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_gate_refuses_a_tool_nobody_allowed() {
    // The gate is on by default; nobody is there to allow the call.
    let dir = project("");
    let ended = run_once(dir.path()).await;
    let results = results(&ended);
    assert_eq!(results.len(), 1, "{results:?}");
    assert!(
        results[0].1 && results[0].0.contains("not allowed"),
        "{results:?}"
    );
}

/// Whether the adapters' `ecosystem` runtime is installed
/// (`scripts/copy-adapters.sh`, `npm ci`); required with
/// `YOAGENT_RUTIS_REQUIRE_RUNTIMES=1`.
fn installed(ecosystem: &str) -> bool {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("adapters")
        .join(ecosystem);
    if dir.join("node_modules").is_dir() {
        return true;
    }
    if std::env::var_os("YOAGENT_RUTIS_REQUIRE_RUNTIMES").is_some_and(|v| v == "1") {
        panic!("the {ecosystem} runtime is required: scripts/copy-adapters.sh, then npm ci in adapters/{ecosystem}");
    }
    eprintln!("SKIPPED: scripts/copy-adapters.sh, then npm ci in adapters/{ecosystem}");
    false
}

/// A manifest with one plugin (`plugin`), whose tool `tool` the script calls.
async fn run_plugin(plugin: &str, tool: &str) -> Ended {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("script.json"),
        format!(
            r#"[{{ "tools": [{{ "name": "{tool}", "arguments": {{ "text": "hi" }} }}] }}, {{ "text": "done" }}]"#
        ),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("agent.toml"),
        format!(
            "[agent]\nname = \"plugged\"\nmodel = \"deepseek/deepseek-flash\"\n\n[[plugin]]\n{plugin}\n\n[builtins]\ngate = \"off\"\n"
        ),
    )
    .unwrap();
    run_once(dir.path()).await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dsh_plugin_from_the_manifest_gives_its_tool() {
    if !installed("dsh") {
        return;
    }
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("adapters/dsh/fixture-tools.ts");
    let ended = run_plugin(&format!("dsh = \"{}\"", fixture.display()), "fixture_echo").await;
    assert_eq!(
        ended.outcome,
        Some(RunOutcome::Completed),
        "{:?}",
        ended.error
    );
    let results = results(&ended);
    assert_eq!(results.len(), 1, "{results:?}");
    assert!(
        !results[0].1 && results[0].0.contains("echo: hi"),
        "{results:?}"
    );
    // After the run: the plugins' time and the runtime's memory.
    let measured: Vec<&String> = ended
        .notices
        .iter()
        .filter(|(l, m)| {
            *l == yobuild::frontend::NoticeLevel::Info && m.starts_with("Plugin time this run: ")
        })
        .map(|(_, m)| m)
        .collect();
    assert_eq!(measured.len(), 1, "{:?}", ended.notices);
    assert!(
        measured[0].contains("dsh-tools ") && measured[0].contains("tool fixture_echo 1× "),
        "{}",
        measured[0]
    );
    #[cfg(unix)]
    assert!(
        measured[0].contains("Runtime memory: dsh "),
        "{}",
        measured[0]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_a_limit_stops_is_stopped_not_completed() {
    let echo = r#"{ "tools": [{ "name": "server__echo", "arguments": { "text": "hi" } }] }"#;
    let dir = project_with(
        "[limits]\nmax_turns = 1\n[builtins]\ngate = \"off\"\n",
        &format!("[{echo}, {echo}, {{ \"text\": \"done\" }}]"),
    );
    let ended = run_once(dir.path()).await;
    assert_eq!(
        ended.outcome,
        Some(RunOutcome::Stopped),
        "{:?}",
        ended.error
    );
    let error = ended.error.clone().unwrap_or_default();
    assert!(error.contains("Max turns"), "{error}");
    assert_eq!(results(&ended).len(), 1);
    // What to do, for every frontend.
    assert!(
        ended.notices.iter().any(|(_, m)| m.starts_with(
            "What to do: send \"continue\" (a new run, with fresh limits; not under --once, which ends after one run), or raise [limits] max_turns"
        )),
        "{:?}",
        ended.notices
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_report_sends_the_agent_back_until_it_is_there() {
    let dir = project_with(
        "[builtins]\ngate = \"off\"\nreport = [\"Done\"]\n",
        r###"[{ "text": "finished" }, { "text": "## Done\nall of it" }]"###,
    );
    let ended = run_once(dir.path()).await;
    assert_eq!(
        ended.outcome,
        Some(RunOutcome::Completed),
        "{:?}",
        ended.error
    );
    assert_eq!(answers(&ended), ["finished", "## Done\nall of it"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_report_never_given_fails_the_run() {
    let dir = project_with(
        "[builtins]\ngate = \"off\"\nreport = [\"Done\"]\n",
        r#"[{ "text": "finished" }]"#,
    );
    let ended = run_once(dir.path()).await;
    assert_eq!(ended.outcome, Some(RunOutcome::Error));
    let error = ended.error.unwrap_or_default();
    assert!(error.contains("not accepted after 3 continues"), "{error}");
    // The real failure gets its fix, for every frontend.
    assert!(
        ended.notices.iter().any(|(_, m)| m
            .starts_with("What to do: the last answer must name every [builtins] report section")),
        "{:?}",
        ended.notices
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_owner_is_offered_when_the_manifest_asks_for_it_and_the_gate_lets_it_run() {
    let script = r#"[
      { "tools": [{ "name": "ask_owner", "arguments": { "question": "Which?", "options": ["a", "b"] } }] },
      { "text": "done" }
    ]"#;
    let on = project_with("[builtins]\nask_owner = true\n", script);
    let ended = run_once(on.path()).await;
    let asked = results(&ended);
    assert!(
        !asked[0].1 && asked[0].0.contains("Asked the owner as #1"),
        "{asked:?}"
    );
    let off = project_with("", script);
    let ended = run_once(off.path()).await;
    assert!(results(&ended)[0].1, "no ask_owner without the built-in");
}

#[tokio::test(flavor = "multi_thread")]
async fn two_servers_tools_of_one_name_both_run_under_their_servers_names() {
    let both = project_with(
        "[[mcp]]\nname = \"other\"\nnpm = \"./server.sh\"\nargs = [\"second\"]\n[builtins]\ngate = \"off\"\n",
        r#"[{ "tools": [
              { "name": "server__echo", "arguments": { "text": "a" } },
              { "name": "other__echo", "arguments": { "text": "b" } }
            ] }, { "text": "done" }]"#,
    );
    let ended = run_once(both.path()).await;
    assert_eq!(
        results(&ended),
        [
            ("echoed a (loud, first)".to_string(), false),
            ("echoed b (, second)".to_string(), false)
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_without_tools_or_with_a_name_tools_cannot_take_is_refused() {
    let empty =
        project("[[mcp]]\nname = \"empty\"\nnpm = \"./server.sh\"\nenv = { TOOLS = \"none\" }\n");
    let e = try_once(empty.path()).await.err().unwrap();
    assert_eq!(e, "mcp empty: the server lists no tools");
    // Named after its host by default: dots, which a tool name cannot hold.
    // Refused before anything starts (nothing listens there).
    let dotted = project("[[mcp]]\nurl = \"https://tools.example.com/mcp\"\n");
    let e = try_once(dotted.path()).await.err().unwrap();
    assert_eq!(
        e,
        "mcp tools.example.com: its name prefixes its tools (tools.example.com__<tool>), and tool names take only letters, digits, _ and -: give the server a `name`"
    );
}

/// An MCP server over HTTP that answers like the bash one and records each
/// request's `authorization` header.
async fn http_server() -> (
    std::net::SocketAddr,
    std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) {
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use serde_json::{json, Value};
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let record = seen.clone();
    let app = axum::Router::new().route(
        "/mcp",
        axum::routing::post(move |headers: HeaderMap, body: String| {
            let record = record.clone();
            async move {
                if let Some(auth) = headers.get("authorization") {
                    record
                        .lock()
                        .unwrap()
                        .push(auth.to_str().unwrap().to_owned());
                }
                let request: Value = serde_json::from_str(&body).unwrap();
                let Some(id) = request.get("id").cloned() else {
                    return StatusCode::ACCEPTED.into_response();
                };
                let result = match request["method"].as_str() {
                    Some("initialize") => json!({
                        "protocolVersion": "2024-11-05",
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "web", "version": "1" }
                    }),
                    Some("tools/list") => json!({ "tools": [{
                        "name": "echo", "description": "Echo",
                        "inputSchema": { "type": "object", "properties": { "text": { "type": "string" } } }
                    }]}),
                    _ => json!({ "content": [{ "type": "text",
                        "text": format!("web echoed {}", request["params"]["arguments"]["text"].as_str().unwrap_or("")) }] }),
                };
                (
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string(),
                )
                    .into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (addr, seen)
}

#[tokio::test(flavor = "multi_thread")]
async fn an_http_server_gets_its_headers_with_their_values_filled_in() {
    let (addr, seen) = http_server().await;
    // Set for this test only: no other test reads it.
    std::env::set_var("YOBUILD_RUN_TEST_TOKEN", "t0ken");
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("script.json"),
        SCRIPT.replace("server__echo", "web__echo"),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("agent.toml"),
        format!(
            "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n\
             [[mcp]]\nname = \"web\"\nurl = \"http://{addr}/mcp\"\n\
             headers = {{ Authorization = \"Bearer ${{YOBUILD_RUN_TEST_TOKEN}}\" }}\n\
             [builtins]\ngate = \"off\"\n"
        ),
    )
    .unwrap();
    let ended = run_once(dir.path()).await;
    assert_eq!(results(&ended), [("web echoed hi".to_string(), false)]);
    let seen = seen.lock().unwrap().clone();
    assert!(
        !seen.is_empty() && seen.iter().all(|a| a == "Bearer t0ken"),
        "{seen:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pinned_dsh_package_runs_in_the_dsh_set() {
    if !installed("dsh") {
        return;
    }
    let modules = Path::new(env!("CARGO_MANIFEST_DIR")).join("adapters/dsh/node_modules");
    let version_of = |name: &str| {
        let text = std::fs::read_to_string(modules.join(name).join("package.json")).unwrap();
        serde_json::from_str::<serde_json::Value>(&text).unwrap()["version"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let name = "@deepseek-ai/dsh-tool-ask-user";
    let version = version_of(name);
    let peer = "@deepseek-ai/dsh-user-questions";
    let peer_version = version_of(peer);
    let dir = tempfile::tempdir().unwrap();
    let script = r#"[{ "tools": [{ "name": "ask_user_question", "arguments": { "questions": [{ "id": "pick", "question": "Which?", "options": [{ "label": "a" }, { "label": "b" }] }] } }] }, { "text": "done" }]"#;
    std::fs::write(dir.path().join("script.json"), script).unwrap();
    let manifest = |version: &str, with_peer: bool| {
        let peer = match with_peer {
            true => format!("[[plugin]]\ndsh = \"{peer}@{peer_version}\"\n"),
            false => String::new(),
        };
        std::fs::write(
            dir.path().join("agent.toml"),
            format!("[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n[[plugin]]\ndsh = \"{name}@{version}\"\n{peer}[builtins]\ngate = \"off\"\n"),
        )
        .unwrap()
    };
    // Its question needs the peer that provides `userQuestions`.
    manifest(&version, false);
    let e = try_once(dir.path()).await.err().unwrap();
    assert!(
        e.starts_with("DSH plugins wait for services nobody provides: userQuestions")
            && e.contains(&format!(
                "not loaded as plugins: @deepseek-ai/dsh-agent, {peer}"
            )),
        "{e}"
    );
    manifest(&version, true);
    let ended = run_once(dir.path()).await;
    assert_eq!(
        ended.outcome,
        Some(RunOutcome::Completed),
        "{:?}",
        ended.error
    );
    // Its tool ran and reached the peer's service, which found nobody there
    // to answer.
    let ran = results(&ended);
    assert_eq!(
        ran,
        [(
            "Error: no user-questions answerer accepted the request".to_string(),
            true
        )]
    );
    // Another version than the DSH base's is refused.
    manifest("9.9.9", false);
    let e = try_once(dir.path()).await.err().unwrap();
    assert_eq!(
        e,
        format!("plugin dsh-tool-ask-user: {name}@9.9.9 is pinned by the DSH base at {version}")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dsh_plugin_waiting_for_services_nobody_provides_is_refused() {
    if !installed("dsh") {
        return;
    }
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("adapters/dsh/fixture-needs-services.ts");
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("script.json"), r#"[{ "text": "done" }]"#).unwrap();
    let manifest = |extra: &str| {
        std::fs::write(
            dir.path().join("agent.toml"),
            format!(
                "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n[[plugin]]\ndsh = \"{}\"\n[builtins]\ngate = \"off\"\n{extra}",
                fixture.display()
            ),
        )
        .unwrap()
    };
    manifest("");
    let e = try_once(dir.path()).await.err().unwrap();
    assert!(
        e.starts_with("DSH plugins wait for services nobody provides: ")
            && e.contains("fs,")
            && e.ends_with("[dsh] host_services = true)"),
        "{e}"
    );
    manifest("[dsh]\nhost_services = true\n");
    let ended = run_once(dir.path()).await;
    assert_eq!(
        ended.outcome,
        Some(RunOutcome::Completed),
        "{:?}",
        ended.error
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn what_loads_only_in_part_is_a_warning_every_frontend_gets() {
    let dir = project("[builtins]\ngate = \"off\"\n[dsh]\nhost_services = true\n");
    let ended = run_once(dir.path()).await;
    assert!(
        ended.notices.iter().any(|(level, message)| {
            *level == yobuild::frontend::NoticeLevel::Warning
                && message == "[dsh] host_services has no effect: no DSH plugin"
        }),
        "{:?}",
        ended.notices
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pinned_mcp_server_runs_its_installed_bin() {
    let dir = project_with("", &SCRIPT.replace("server__echo", "srv__echo"));
    // Not on npm: installed by hand into the cache, with its lock.
    let package = dir.path().join("srv");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::rename(dir.path().join("server.sh"), package.join("server.sh")).unwrap();
    std::fs::write(
        package.join("package.json"),
        r#"{ "name": "srv", "version": "1.0.0", "bin": "./server.sh" }"#,
    )
    .unwrap();
    yobuild::install::seed(
        Path::new(CACHE),
        &dir.path().join("agent.toml"),
        yobuild::install::Set::Mcp,
        &[("srv", "1.0.0", &package)],
    )
    .unwrap();
    let manifest = |spec: &str| {
        std::fs::write(
            dir.path().join("agent.toml"),
            format!(
                "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n[[mcp]]\nnpm = \"{spec}\"\nargs = [\"first\"]\nenv = {{ MODE = \"loud\" }}\n[builtins]\ngate = \"off\"\n"
            ),
        )
        .unwrap()
    };
    manifest("srv@1.0.0");
    let ended = run_once(dir.path()).await;
    assert_eq!(
        results(&ended),
        [("echoed hi (loud, first)".to_string(), false)]
    );
    // Another version: the lock is stale, which `--locked` refuses.
    manifest("srv@1.0.1");
    let e = try_once_locked(dir.path(), true).await.err().unwrap();
    assert!(
        e.ends_with("agent.lock: its `mcp` set is not current: the manifest's pins or the DSH base changed (--locked: run without it to resolve the set again)"),
        "{e}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_that_stops_during_the_session_is_said() {
    if !installed("dsh") {
        return;
    }
    // It starts, then — once the run makes a file it waits for — takes its
    // runtime down with it (an error nothing catches), while the run is
    // still going.
    let dir = tempfile::tempdir().unwrap();
    let signal = dir.path().join("crash-now");
    std::fs::write(dir.path().join("package.json"), r#"{ "type": "module" }"#).unwrap();
    std::fs::write(
        dir.path().join("fails-later.ts"),
        "import { existsSync } from 'node:fs'\n\
         export const name = 'fails-later'\n\
         export function apply(_ctx: any, config: { file: string }) {\n\
           const timer = setInterval(() => {\n\
             if (existsSync(config.file)) { clearInterval(timer); setTimeout(() => { throw new Error('gave up') }) }\n\
           }, 50)\n\
         }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("script.json"),
        format!(
            r#"[{{ "tools": [{{ "name": "bash", "arguments": {{ "command": "touch {}; sleep 2" }} }}] }}, {{ "text": "done" }}]"#,
            signal.display()
        ),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("agent.toml"),
        format!(
            "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n[[plugin]]\ndsh = \"{}\"\nconfig = {{ file = \"{}\" }}\n[builtins]\ngate = \"off\"\n",
            dir.path().join("fails-later.ts").display(),
            signal.display()
        ),
    )
    .unwrap();
    let ended = run_once(dir.path()).await;
    // Once for the runtime, naming its plugins.
    let said: Vec<&String> = ended
        .notices
        .iter()
        .filter(|(level, _)| *level == yobuild::frontend::NoticeLevel::Error)
        .map(|(_, message)| message)
        .collect();
    assert_eq!(said.len(), 1, "{said:?}");
    let said = said[0]
        .starts_with("A plugin stopped: the `dsh` runtime is not running (its plugins wait: ")
        && said[0].contains("plugin:fails-later");
    assert!(said, "{:?}", ended.notices);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_mcp_tool_the_entry_allows_runs_with_the_gate_on_and_a_typo_is_refused() {
    let allowed = project("[[mcp]]\nname = \"other\"\nnpm = \"./server.sh\"\n");
    // The first server allows `echo`; the gate is on (the default).
    std::fs::write(
        allowed.path().join("agent.toml"),
        "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n\
         [[mcp]]\nnpm = \"./server.sh\"\nargs = [\"first\"]\nenv = { MODE = \"loud\" }\nallow = [\"echo\"]\n",
    )
    .unwrap();
    let ended = run_once(allowed.path()).await;
    assert_eq!(
        results(&ended),
        [("echoed hi (loud, first)".to_string(), false)]
    );
    std::fs::write(
        allowed.path().join("agent.toml"),
        "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n[[mcp]]\nnpm = \"./server.sh\"\nallow = [\"ech\"]\n",
    )
    .unwrap();
    assert_eq!(
        try_once(allowed.path()).await.err().unwrap(),
        "mcp server: its `allow` names \"ech\", which it does not offer (it offers: echo)"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_tool_the_entry_allows_runs_and_one_it_does_not_offer_is_a_warning() {
    if !installed("dsh") {
        return;
    }
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("adapters/dsh/fixture-tools.ts");
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("script.json"),
        r#"[{ "tools": [{ "name": "fixture_echo", "arguments": { "text": "hi" } }] }, { "text": "done" }]"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("agent.toml"),
        format!(
            "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n[[plugin]]\ndsh = \"{}\"\nallow = [\"fixture_echo\", \"fixture_ech\"]\n",
            fixture.display()
        ),
    )
    .unwrap();
    let ended = run_once(dir.path()).await;
    let ran = results(&ended);
    assert!(
        ran.len() == 1 && !ran[0].1 && ran[0].0.contains("echo: hi"),
        "{ran:?}"
    );
    let warned: Vec<&String> = ended
        .notices
        .iter()
        .filter(|(l, _)| *l == yobuild::frontend::NoticeLevel::Warning)
        .map(|(_, m)| m)
        .collect();
    assert_eq!(
        warned,
        ["plugin fixture-tools offered no tool `fixture_ech`, which its `allow` names (a typo, or a tool it offers only in some runs)"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rutis_policy_plugin_is_waited_for_and_judges_the_first_call() {
    // A rutis plugin that registers its call judge (`before_tool`) 1.5 s
    // into its `apply`. Marked `policy`, the run waits for it — else the
    // first call could find no policy and be denied (rutis 0.11's `load`
    // happens to wait for an async `apply` too; yobuild does not count on
    // it) — and then it judges.
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR")).join("frontend/plugins/node_modules");
    if !runtime.join("@arcships/rutis-runtime").is_dir() {
        if std::env::var_os("YOAGENT_RUTIS_REQUIRE_RUNTIMES").is_some_and(|v| v == "1") {
            panic!("the rutis runtime is required: npm ci in frontend/plugins");
        }
        eprintln!("SKIPPED: npm ci in frontend/plugins");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(&runtime, dir.path().join("node_modules")).unwrap();
    std::fs::write(dir.path().join("package.json"), r#"{ "type": "module" }"#).unwrap();
    std::fs::write(
        dir.path().join("guard.ts"),
        "import { definePlugin } from '@arcships/rutis'\n\
         export default definePlugin({\n\
           inject: ['yoagent'],\n\
           async apply(ctx: any, config: { deny: boolean }) {\n\
             await new Promise((done) => setTimeout(done, 1500))\n\
             ctx.effect(ctx.use('yoagent').register('guard', {\n\
               async before_tool(call: any) { if (config.deny) return { deny: `the guard denies ${call.tool}` } },\n\
             }))\n\
           },\n\
         })\n",
    )
    .unwrap();
    // Another plugin's judge, up at once and letting every call through:
    // the guard's wait is for a judge from *its* row, so this one does not
    // end it.
    std::fs::write(
        dir.path().join("other.ts"),
        "import { definePlugin } from '@arcships/rutis'\n\
         export default definePlugin({\n\
           inject: ['yoagent'],\n\
           apply(ctx: any) { ctx.effect(ctx.use('yoagent').register('other', { async before_tool() {} })) },\n\
         })\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("script.json"),
        r#"[{ "tools": [{ "name": "list_files", "arguments": { "path": "." } }] }, { "text": "done" }]"#,
    )
    .unwrap();
    for (policy, deny, expected) in [
        (true, false, None),
        (true, true, Some("the guard denies list_files")),
    ] {
        std::fs::write(
            dir.path().join("agent.toml"),
            format!(
                "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n\
                 [[plugin]]\nrutis = \"./other.ts\"\nruntime = \"node\"\n\
                 [[plugin]]\nrutis = \"./guard.ts\"\nruntime = \"node\"\npolicy = {policy}\nconfig = {{ deny = {deny} }}\n\
                 [builtins]\ngate = \"off\"\n"
            ),
        )
        .unwrap();
        let ended = run_once(dir.path()).await;
        let ran = results(&ended);
        assert_eq!(ran.len(), 1, "{ran:?}");
        match expected {
            None => assert!(!ran[0].1, "policy = {policy}, deny = {deny}: {ran:?}"),
            Some(said) => assert!(ran[0].1 && ran[0].0.contains(said), "{ran:?}"),
        }
    }
}

/// A rutis plugin (its package the project, the runtime linked from
/// `frontend/plugins`) that asks the owner through the `ask_owner` service
/// as it starts and writes what it got to `asked.txt`.
fn asking_plugin(builtin: bool) -> Option<tempfile::TempDir> {
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR")).join("frontend/plugins/node_modules");
    if !runtime.join("@arcships/rutis-runtime").is_dir() {
        if std::env::var_os("YOAGENT_RUTIS_REQUIRE_RUNTIMES").is_some_and(|v| v == "1") {
            panic!("the rutis runtime is required: npm ci in frontend/plugins");
        }
        eprintln!("SKIPPED: npm ci in frontend/plugins");
        return None;
    }
    let dir = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(&runtime, dir.path().join("node_modules")).unwrap();
    std::fs::write(dir.path().join("package.json"), r#"{ "type": "module" }"#).unwrap();
    std::fs::write(
        dir.path().join("asker.ts"),
        "import { writeFileSync } from 'node:fs'\n\
         import { definePlugin } from '@arcships/rutis'\n\
         export default definePlugin({\n\
           inject: ['ask_owner'],\n\
           async apply(ctx, config: { out: string }) {\n\
             let got: string\n\
             try { got = String(await ctx.use('ask_owner').ask({ question: 'Plan?', options: ['A', 'B'] }, 'planner')) }\n\
             catch (e) { got = 'refused: ' + (e as Error).message }\n\
             writeFileSync(config.out, got)\n\
           },\n\
         })\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("script.json"),
        r#"[{ "tools": [{ "name": "ask_owner", "arguments": { "question": "Which?" } }] }, { "text": "done" }]"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("agent.toml"),
        format!(
            "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n\
             [[plugin]]\nrutis = \"./asker.ts\"\nruntime = \"node\"\nconfig = {{ out = \"{}\" }}\n\
             [builtins]\nask_owner = {builtin}\nfacts = true\n",
            dir.path().join("asked.txt").display()
        ),
    )
    .unwrap();
    Some(dir)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_asks_the_owner_through_the_service_numbered_with_the_tool() {
    let Some(on) = asking_plugin(true) else {
        return;
    };
    let ended = run_once(on.path()).await;
    // The plugin asked as it started, the agent's tool in its run: one
    // numbering (in either order: a row runs before its `apply` ends), both
    // still waiting, nobody being there.
    let plugins = std::fs::read_to_string(on.path().join("asked.txt")).unwrap();
    let asked = results(&ended);
    let tools = if plugins == "1" { "#2" } else { "#1" };
    assert!(
        ["1", "2"].contains(&plugins.as_str())
            && asked[0].0.contains(&format!("Asked the owner as {tools}")),
        "{plugins} {asked:?}"
    );
    let facts = ended
        .notices
        .iter()
        .find(|(_, m)| m.starts_with("Facts:"))
        .map(|(_, m)| m.clone())
        .unwrap_or_default();
    assert!(facts.contains("2 question(s) waiting on you"), "{facts}");
    // The host names the asking plugin from its row (`plugin:asker`); the
    // `from` it gave ("planner") is ignored, and said once.
    let warned = ended.notices.iter().any(|(_, m)| {
        m.starts_with("plugin asker asks the owner as `planner`: the question names asker")
    });
    assert!(warned, "{:?}", ended.notices);
    // Without the built-in, the service is there (the plugin starts) and
    // says why it does not ask.
    let Some(off) = asking_plugin(false) else {
        return;
    };
    run_once(off.path()).await;
    let got = std::fs::read_to_string(off.path().join("asked.txt")).unwrap();
    assert!(
        got.starts_with("refused: ") && got.contains("[builtins] ask_owner is off"),
        "{got}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rutis_plugins_allow_holds_only_for_its_own_rows_tools() {
    // (`lib` is marked `policy`, with a judge that lets every call through
    // to the gate: so start-up waits for its handler, and `shout` is
    // offered from the first run.)
    // Two rutis plugins of one package: `lib` offers `shout`. Allowed by
    // `lib`, it runs; allowed by `app` (which offers nothing), it is
    // another row's tool: it asks (nobody there: refused), and a Warning
    // names who offered it.
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR")).join("frontend/plugins/node_modules");
    if !runtime.join("@arcships/rutis-runtime").is_dir() {
        if std::env::var_os("YOAGENT_RUTIS_REQUIRE_RUNTIMES").is_some_and(|v| v == "1") {
            panic!("the rutis runtime is required: npm ci in frontend/plugins");
        }
        eprintln!("SKIPPED: npm ci in frontend/plugins");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(&runtime, dir.path().join("node_modules")).unwrap();
    std::fs::write(dir.path().join("package.json"), r#"{ "type": "module" }"#).unwrap();
    std::fs::write(
        dir.path().join("lib.ts"),
        "import { definePlugin } from '@arcships/rutis'\n\
         export default definePlugin({\n\
           inject: ['yoagent'],\n\
           apply(ctx: any) {\n\
             ctx.effect(ctx.use('yoagent').register('lib-tools', {\n\
               async tools() { return [{ name: 'shout', description: 'Shout', parameters: { type: 'object', properties: {} } }] },\n\
               async call_tool() { return { text: 'HEY' } },\n\
               async before_tool() {},\n\
             }))\n\
           },\n\
         })\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app.ts"),
        "import { definePlugin } from '@arcships/rutis'\n\
         export default definePlugin({ apply() {} })\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("script.json"),
        r#"[{ "tools": [{ "name": "shout", "arguments": {} }] }, { "text": "done" }]"#,
    )
    .unwrap();
    // Allowed by both: `lib`'s entry frees it, and nothing is warned.
    for (allowing, runs) in [("lib", true), ("app", false), ("both", true)] {
        let allow = |name: &str| match name == allowing || allowing == "both" {
            true => "allow = [\"shout\"]\n",
            false => "",
        };
        std::fs::write(
            dir.path().join("agent.toml"),
            format!(
                "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n\
                 [[plugin]]\nrutis = \"./lib.ts\"\nruntime = \"node\"\npolicy = true\n{}\
                 [[plugin]]\nrutis = \"./app.ts\"\nruntime = \"node\"\n{}",
                allow("lib"),
                allow("app")
            ),
        )
        .unwrap();
        let ended = run_once(dir.path()).await;
        let ran = results(&ended);
        assert_eq!(ran.len(), 1, "{ran:?}");
        assert_eq!(!ran[0].1, runs, "allowed by {allowing}: {ran:?}");
        if !runs {
            assert!(ran[0].0.contains("not allowed"), "the gate asked: {ran:?}");
        }
        let warned = ended.notices.iter().any(|(_, m)| {
            m == "plugin app allows `shout`, but it was offered by `plugin:lib`, not by it: the tool asks"
        });
        assert_eq!(warned, !runs, "{:?}", ended.notices);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rutis_plugin_waiting_for_a_service_nobody_provides_is_named() {
    // Not a DSH plugin: said as rutis says it, without the DSH check's hints.
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR")).join("frontend/plugins/node_modules");
    if !runtime.join("@arcships/rutis-runtime").is_dir() {
        if std::env::var_os("YOAGENT_RUTIS_REQUIRE_RUNTIMES").is_some_and(|v| v == "1") {
            panic!("the rutis runtime is required: npm ci in frontend/plugins");
        }
        eprintln!("SKIPPED: npm ci in frontend/plugins");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(&runtime, dir.path().join("node_modules")).unwrap();
    std::fs::write(dir.path().join("package.json"), r#"{ "type": "module" }"#).unwrap();
    std::fs::write(
        dir.path().join("needy.ts"),
        "import { definePlugin } from '@arcships/rutis'\n\
         export default definePlugin({ inject: ['nosuchService'], apply() {} })\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("script.json"), r#"[{ "text": "done" }]"#).unwrap();
    std::fs::write(
        dir.path().join("agent.toml"),
        "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n[[plugin]]\nrutis = \"./needy.ts\"\nruntime = \"node\"\n",
    )
    .unwrap();
    let started = std::time::Instant::now();
    let e = try_once(dir.path()).await.err().unwrap();
    assert!(
        e.contains("plugin:needy (waiting): nothing provides nosuchService") && !e.contains("DSH"),
        "{e}"
    );
    // Said at the first look, not after the minute a loading row gets.
    assert!(
        started.elapsed() < std::time::Duration::from_secs(45),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_judging_calls_is_said() {
    let off = project("[builtins]\ngate = \"off\"\n");
    let ended = run_once(off.path()).await;
    let said = |ended: &Ended| {
        ended.notices.iter().any(|(level, m)| {
            *level == yobuild::frontend::NoticeLevel::Warning
                && m.starts_with("nothing judges tool calls: `gate = \"off\"`")
        })
    };
    assert!(said(&ended), "{:?}", ended.notices);
    // With the gate on (the default), nothing to say.
    let on = project("");
    assert!(!said(&run_once(on.path()).await));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_call_after_the_policy_plugin_died_is_denied() {
    // A rutis policy plugin that lets calls through, and takes its runtime
    // down once the run makes a file. The call after must be denied, not
    // run unjudged.
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR")).join("frontend/plugins/node_modules");
    if !runtime.join("@arcships/rutis-runtime").is_dir() {
        if std::env::var_os("YOAGENT_RUTIS_REQUIRE_RUNTIMES").is_some_and(|v| v == "1") {
            panic!("the rutis runtime is required: npm ci in frontend/plugins");
        }
        eprintln!("SKIPPED: npm ci in frontend/plugins");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let signal = dir.path().join("die-now");
    std::os::unix::fs::symlink(&runtime, dir.path().join("node_modules")).unwrap();
    std::fs::write(dir.path().join("package.json"), r#"{ "type": "module" }"#).unwrap();
    std::fs::write(
        dir.path().join("guard.ts"),
        "import { existsSync } from 'node:fs'\n\
         import { definePlugin } from '@arcships/rutis'\n\
         export default definePlugin({\n\
           inject: ['yoagent'],\n\
           apply(ctx: any, config: { file: string }) {\n\
             ctx.effect(ctx.use('yoagent').register('guard', { async before_tool() {} }))\n\
             const timer = setInterval(() => {\n\
               if (existsSync(config.file)) { clearInterval(timer); setTimeout(() => { throw new Error('gone') }) }\n\
             }, 50)\n\
           },\n\
         })\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("script.json"),
        format!(
            r#"[{{ "tools": [{{ "name": "bash", "arguments": {{ "command": "touch {}; sleep 2" }} }}] }},
                {{ "tools": [{{ "name": "list_files", "arguments": {{ "path": "." }} }}] }},
                {{ "text": "done" }}]"#,
            signal.display()
        ),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("agent.toml"),
        format!(
            "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n\
             [[plugin]]\nrutis = \"./guard.ts\"\nruntime = \"node\"\npolicy = true\nconfig = {{ file = \"{}\" }}\n\
             [builtins]\ngate = \"off\"\n",
            signal.display()
        ),
    )
    .unwrap();
    let ended = run_once(dir.path()).await;
    let ran = results(&ended);
    assert_eq!(ran.len(), 2, "{ran:?}");
    assert!(!ran[0].1, "the first call ran: {ran:?}");
    assert!(
        ran[1].1
            && ran[1]
                .0
                .contains("plugin handler `guard` is no longer available"),
        "after the policy died, a call is denied: {ran:?} {:?}",
        ended.notices
    );
    // And the owner is told.
    let said = ended
        .notices
        .iter()
        .any(|(_, m)| m.starts_with("A plugin stopped: the `rutis-0` runtime is not running"));
    assert!(said, "{:?}", ended.notices);
    // What to do, though the run completed: the runtime's, and the denied
    // call's, each once.
    let fixes: Vec<&str> = ended
        .notices
        .iter()
        .map(|(_, m)| m.as_str())
        .filter(|m| m.starts_with("What to do: "))
        .collect();
    assert!(
        fixes.contains(&"What to do: the plugin runtime died: the log says why; restart.")
            && fixes.contains(&"What to do: a plugin's runtime died or it was unloaded: restart; the log says why."),
        "{fixes:?}"
    );
    assert_eq!(fixes.len(), 2, "{fixes:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tool_a_dsh_plugins_child_registers_is_its_parents_only_through_its_row() {
    // A DSH plugin (a Cordis plugin) starts a child plugin that injects
    // `yoagent` and registers `shout`. When the parent injects `yoagent`
    // too, the child registers through the parent's row: the tool is the
    // parent's, and its `allow` frees it. When it does not, the child
    // reaches `yoagent` through no row (other rows of the runtime provide
    // it): the tool is nobody's — it asks (refused with nobody there), and
    // the Warning says why.
    if !installed("dsh") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let parent = |inject: &str| {
        format!(
            "export const name = 'parent'\n\
             {inject}\
             export function apply(ctx: any) {{\n\
               ctx.plugin({{\n\
                 name: 'kid',\n\
                 inject: ['yoagent'],\n\
                 apply(c: any) {{\n\
                   c.yoagent.register('kid-tools', {{\n\
                     async tools() {{ return [{{ name: 'shout', description: 'Shout', parameters: {{ type: 'object', properties: {{}} }} }}] }},\n\
                     async call_tool() {{ return {{ text: 'HEY' }} }},\n\
                   }})\n\
                 }},\n\
               }})\n\
             }}\n"
        )
    };
    std::fs::write(
        dir.path().join("script.json"),
        r#"[{ "tools": [{ "name": "shout", "arguments": {} }] }, { "text": "done" }]"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("agent.toml"),
        "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n[[plugin]]\ndsh = \"./parent.ts\"\nallow = [\"shout\"]\n",
    )
    .unwrap();
    for (inject, runs) in [("export const inject = ['yoagent']\n", true), ("", false)] {
        std::fs::write(dir.path().join("parent.ts"), parent(inject)).unwrap();
        let ended = run_once(dir.path()).await;
        let ran = results(&ended);
        assert_eq!(ran.len(), 1, "{ran:?}");
        let warned = ended.notices.iter().any(|(_, m)| {
            m.starts_with("plugin parent allows `shout`, but it was offered from no plugin row")
        });
        match runs {
            true => assert!(!ran[0].1 && !warned, "{ran:?} {:?}", ended.notices),
            false => assert!(
                ran[0].1 && ran[0].0.contains("not allowed") && warned,
                "{ran:?} {:?}",
                ended.notices
            ),
        }
    }
}

// -- yobuild check, through `check()` ------------------------------------------

/// `check` on the project's `agent.toml`.
async fn check_dir(dir: &Path) -> yobuild::run::check::Report {
    use yobuild::run::check::{check, CheckOptions};
    let manifest = Manifest::load(dir.join("agent.toml")).unwrap();
    let mut options = CheckOptions::new(manifest);
    options.home = Some(CACHE.into());
    check(options).await.unwrap()
}

/// The kind of `tool` in `tools`, as JSON says it.
fn kind(tools: &[yobuild::run::check::Tool], tool: &str) -> Option<String> {
    tools.iter().find(|t| t.name == tool).map(|t| {
        serde_json::to_value(t.kind)
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned()
    })
}

/// A rutis package in a temp folder with the frontend's runtime, or `None`
/// when that is not installed.
fn rutis_package() -> Option<tempfile::TempDir> {
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR")).join("frontend/plugins/node_modules");
    if !runtime.join("@arcships/rutis-runtime").is_dir() {
        if std::env::var_os("YOAGENT_RUTIS_REQUIRE_RUNTIMES").is_some_and(|v| v == "1") {
            panic!("the rutis runtime is required: npm ci in frontend/plugins");
        }
        eprintln!("SKIPPED: npm ci in frontend/plugins");
        return None;
    }
    let dir = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(&runtime, dir.path().join("node_modules")).unwrap();
    std::fs::write(dir.path().join("package.json"), r#"{ "type": "module" }"#).unwrap();
    Some(dir)
}

/// A rutis plugin registering handler `handler` with `tools`.
fn rutis_tools(handler: &str, tools: &[&str]) -> String {
    let list: Vec<String> = tools
        .iter()
        .map(|t| format!("{{ name: '{t}', description: '{t}', parameters: {{ type: 'object', properties: {{}} }} }}"))
        .collect();
    format!(
        "import {{ definePlugin }} from '@arcships/rutis'\n\
         export default definePlugin({{\n\
           inject: ['yoagent'],\n\
           apply(ctx: any) {{\n\
             ctx.effect(ctx.use('yoagent').register('{handler}', {{\n\
               async tools() {{ return [{}] }},\n\
               async call_tool() {{ return {{ text: 'ok' }} }},\n\
             }}))\n\
           }},\n\
         }})\n",
        list.join(", ")
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn check_judges_a_rutis_plugins_tools_by_who_offers_them() {
    let Some(dir) = rutis_package() else { return };
    std::fs::write(
        dir.path().join("lib.ts"),
        rutis_tools("lib-tools", &["shout"]),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app.ts"),
        "import { definePlugin } from '@arcships/rutis'\nexport default definePlugin({ apply() {} })\n",
    )
    .unwrap();
    for (allowing, allowed) in [("lib", true), ("app", false)] {
        let allow = |name: &str| match name == allowing {
            true => "allow = [\"shout\"]\n",
            false => "",
        };
        std::fs::write(
            dir.path().join("agent.toml"),
            format!(
                "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n\
                 [[plugin]]\nrutis = \"./lib.ts\"\nruntime = \"node\"\n{}\
                 [[plugin]]\nrutis = \"./app.ts\"\nruntime = \"node\"\n{}",
                allow("lib"),
                allow("app")
            ),
        )
        .unwrap();
        let report = check_dir(dir.path()).await;
        assert!(report.ok, "{}", report.text());
        let lib = &report.entries[0];
        assert_eq!(lib.handlers.as_deref(), Some(&["lib-tools".to_owned()][..]));
        assert_eq!(
            kind(&lib.tools, "shout").as_deref(),
            Some(if allowed { "allowed" } else { "asks" })
        );
        assert_eq!(report.entries[1].handlers.as_deref(), Some(&[][..]));
        assert_eq!(
            kind(&report.agent.tools, "shout").as_deref(),
            Some(if allowed { "allowed" } else { "asks" }),
            "{}",
            report.text()
        );
        let warned = report.agent.notes.iter().any(|n| {
            n == "plugin app allows `shout`, but it was offered by `plugin:lib`, not by it: the tool asks"
        });
        assert_eq!(warned, !allowed, "{:?}", report.agent.notes);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn check_notes_a_tool_offered_twice_or_named_like_the_agents_own() {
    let Some(dir) = rutis_package() else { return };
    std::fs::write(dir.path().join("a.ts"), rutis_tools("a-tools", &["shout"])).unwrap();
    std::fs::write(
        dir.path().join("b.ts"),
        rutis_tools("b-tools", &["shout", "read_file", "write"]),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("agent.toml"),
        "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n\
         [[plugin]]\nrutis = \"./a.ts\"\nruntime = \"node\"\n\
         [[plugin]]\nrutis = \"./b.ts\"\nruntime = \"node\"\n",
    )
    .unwrap();
    let report = check_dir(dir.path()).await;
    let text = report.text();
    assert!(report.ok, "{text}");
    assert!(
        report
            .agent
            .notes
            .iter()
            .any(|n| n == "plugins a, b all offer a tool named `shout`: a run gets one of them"),
        "{text}"
    );
    assert!(
        report.agent.notes.iter().any(|n| n
            == "`read_file`, offered by plugin:b, is named like the agent's own tool: yoagent keeps its own"),
        "{text}"
    );
    // yoagent's read_file once, free; a plugin's `write` judged by name.
    assert_eq!(
        report
            .agent
            .tools
            .iter()
            .filter(|t| t.name == "read_file")
            .count(),
        1
    );
    assert_eq!(
        kind(&report.agent.tools, "read_file").as_deref(),
        Some("free")
    );
    assert_eq!(
        kind(&report.agent.tools, "write").as_deref(),
        Some("judged")
    );
    assert_eq!(
        kind(&report.entries[1].tools, "write").as_deref(),
        Some("judged")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn check_says_a_rutis_plugin_that_stops_or_waits() {
    let Some(dir) = rutis_package() else { return };
    // Its row runs, then its `apply` throws.
    std::fs::write(
        dir.path().join("late.ts"),
        "import { definePlugin } from '@arcships/rutis'\n\
         export default definePlugin({ async apply() { await new Promise(r => setTimeout(r, 300)); throw new Error('boom') } })\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("agent.toml"),
        "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n[[plugin]]\nrutis = \"./late.ts\"\nruntime = \"node\"\n",
    )
    .unwrap();
    let report = check_dir(dir.path()).await;
    // (Whether `load` waits for its async `apply` or the row stops after,
    // the failure is the entry's problem.)
    assert!(!report.ok);
    assert!(
        report.entries[0]
            .problems
            .iter()
            .any(|p| p.message.contains("boom") && p.fix.is_some()),
        "{}",
        report.text()
    );
    // Waiting for a service nobody provides: not checked alone (another
    // plugin may provide it), refused together.
    std::fs::write(
        dir.path().join("needy.ts"),
        "import { definePlugin } from '@arcships/rutis'\n\
         export default definePlugin({ inject: ['nosuchService'], apply() {} })\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("agent.toml"),
        "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n[[plugin]]\nrutis = \"./needy.ts\"\nruntime = \"node\"\n",
    )
    .unwrap();
    let report = check_dir(dir.path()).await;
    let text = report.text();
    assert!(!report.ok, "{text}");
    assert!(!report.entries[0].checked_alone, "{text}");
    assert!(
        text.contains(
            "  note: not checked alone: it waits for a service another plugin may provide"
        ) && text.contains("agent a: 1 problem\n  problem: `run` would refuse it at start: ")
            && text.contains("nothing provides nosuchService"),
        "{text}"
    );
}

/// `check` scans each entry's own code for the variables it reads (design
/// §12 "Step 5"): a pinned server's package, a rutis plugin's file; a
/// server that is a shell script has none, which is said. `CARGO_PKG_NAME`
/// is set for the tests by cargo, and left out of their environment.
#[tokio::test(flavor = "multi_thread")]
async fn check_scans_each_entrys_own_code() {
    let dir = project_with("", &SCRIPT.replace("server__echo", "srvenv__echo"));
    let package = dir.path().join("srvenv");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::copy(dir.path().join("server.sh"), package.join("server.sh")).unwrap();
    std::fs::write(
        package.join("package.json"),
        r#"{ "name": "srvenv", "version": "1.0.0", "bin": "./server.sh" }"#,
    )
    .unwrap();
    std::fs::write(
        package.join("lib.js"),
        "module.exports = process.env.CARGO_PKG_NAME\n",
    )
    .unwrap();
    let rutis = rutis_package();
    let plugin = match &rutis {
        Some(r) => {
            let file = r.path().join("reads.ts");
            std::fs::write(
                &file,
                format!(
                    "{}const seen = process.env['CARGO_PKG_NAME']\n",
                    rutis_tools("reads-tools", &["look"])
                ),
            )
            .unwrap();
            format!(
                "[[plugin]]\nrutis = \"{}\"\nruntime = \"node\"\n",
                file.display()
            )
        }
        None => String::new(),
    };
    std::fs::write(
        dir.path().join("agent.toml"),
        format!(
            "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n\
             [[mcp]]\nnpm = \"srvenv@1.0.0\"\n\
             [[mcp]]\nname = \"shell\"\nnpm = \"./server.sh\"\n{plugin}"
        ),
    )
    .unwrap();
    yobuild::install::seed(
        Path::new(CACHE),
        &dir.path().join("agent.toml"),
        yobuild::install::Set::Mcp,
        &[("srvenv", "1.0.0", &package)],
    )
    .unwrap();
    let report = check_dir(dir.path()).await;
    assert!(report.ok, "{}", report.text());
    let notes = |name: &str| {
        report
            .entries
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("{name}: {}", report.text()))
            .notes
            .clone()
    };
    assert!(
        notes("srvenv").contains(&"its code reads `CARGO_PKG_NAME`, which is set here but left out of its environment: hand it over in its `env` table (`CARGO_PKG_NAME = \"${CARGO_PKG_NAME}\"`) if it should have it".to_owned()),
        "{}",
        report.text()
    );
    assert!(
        notes("shell").contains(&"no JavaScript or TypeScript code of its own was found to scan: what it reads from the environment is not known".to_owned()),
        "{}",
        report.text()
    );
    if rutis.is_some() {
        assert!(
            notes("reads").contains(&"its code reads `CARGO_PKG_NAME`, which is set here but left out of its environment: grant it (`grants = { env = [\"CARGO_PKG_NAME\"] }`) if it should have it".to_owned()),
            "{}",
            report.text()
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn check_allows_a_servers_tool_its_entry_allows() {
    let dir = project("[[mcp]]\nname = \"other\"\nnpm = \"./server.sh\"\n");
    // `server` allows its echo; `other` does not.
    let text = std::fs::read_to_string(dir.path().join("agent.toml")).unwrap();
    std::fs::write(
        dir.path().join("agent.toml"),
        text.replacen(
            "env = { MODE = \"loud\" }\n",
            "env = { MODE = \"loud\" }\nallow = [\"echo\"]\n",
            1,
        ),
    )
    .unwrap();
    let report = check_dir(dir.path()).await;
    assert!(report.ok, "{}", report.text());
    assert_eq!(
        kind(&report.entries[0].tools, "server__echo").as_deref(),
        Some("allowed")
    );
    assert_eq!(
        kind(&report.agent.tools, "server__echo").as_deref(),
        Some("allowed")
    );
    assert_eq!(
        kind(&report.agent.tools, "other__echo").as_deref(),
        Some("asks")
    );
    assert_eq!(kind(&report.agent.tools, "bash").as_deref(), Some("judged"));
    // Each tool with what it says it does and its arguments' schema.
    let echo = report
        .agent
        .tools
        .iter()
        .find(|t| t.name == "server__echo")
        .unwrap();
    assert!(echo.description.is_some(), "{echo:?}");
    assert!(echo.parameters.is_some(), "{echo:?}");
}

/// A rutis plugin granted `workspace` and `wake` (design §12) in `dir`
/// (a rutis package), writing what the host services answered to `out`:
/// it wakes the agent as it loads (refused: the owner's task first), and
/// its tool `nudge` reads the services, records them, then wakes the agent
/// mid-run.
fn hosted_plugin(dir: &Path, out: &Path) {
    std::fs::write(
        dir.join("hosted.ts"),
        "import { writeFileSync } from 'node:fs'\n\
         import { definePlugin } from '@arcships/rutis'\n\
         export default definePlugin({\n\
           inject: ['yoagent', 'host_workspace', 'host_session', 'host_approval'],\n\
           apply(ctx: any, config: { out: string }) {\n\
             const session = ctx.use('host_session')\n\
             const tried = async (f: () => any) => { try { return await f() } catch (e) { return 'refused: ' + (e as Error).message } }\n\
             const early = tried(() => session.wake('too early'))\n\
             ctx.effect(ctx.use('yoagent').register('hosted', {\n\
               async tools() { return [{ name: 'nudge', description: 'Nudge', parameters: { type: 'object', properties: {} } }] },\n\
               async call_tool() {\n\
                 const got = {\n\
                   early: await early,\n\
                   cwd: await tried(() => ctx.use('host_workspace').cwd()),\n\
                   trusted: await tried(() => ctx.use('host_workspace').trusted()),\n\
                   info: await tried(() => session.info()),\n\
                   model: await tried(() => session.model()),\n\
                   ls: await tried(() => ctx.use('host_approval').judge(['ls', '-la'])),\n\
                   rm: await tried(() => ctx.use('host_approval').judge(['rm', '-rf', 'x'])),\n\
                 }\n\
                 writeFileSync(config.out, JSON.stringify(got))\n\
                 return { text: String(await session.wake('look again')) }\n\
               },\n\
             }))\n\
           },\n\
         })\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("agent.toml"),
        format!(
            "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n\
             [[plugin]]\nrutis = \"./hosted.ts\"\nruntime = \"node\"\nallow = [\"nudge\"]\n\
             config = {{ out = \"{}\" }}\ngrants = {{ workspace = true, wake = true }}\n",
            out.display()
        ),
    )
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rutis_plugin_gets_its_granted_host_services_by_its_row() {
    let Some(dir) = rutis_package() else { return };
    let out = dir.path().join("hosted.json");
    hosted_plugin(dir.path(), &out);
    std::fs::write(
        dir.path().join("script.json"),
        // A spare reply: a wake that reaches the driver only after the
        // loop's look past the tool is read at its last look instead.
        r#"[{ "tools": [{ "name": "nudge", "arguments": {} }] }, { "text": "done" }, { "text": "done" }]"#,
    )
    .unwrap();
    let ended = run_once(dir.path()).await;
    assert_eq!(
        ended.outcome,
        Some(RunOutcome::Completed),
        "{:?}",
        ended.error
    );
    let got: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    let early = got["early"].as_str().unwrap_or_default();
    assert!(
        early.starts_with("refused: ") && early.contains("has not started the owner's task"),
        "{got}"
    );
    // The agent's folder is the process's (`run` works in it).
    let cwd = std::env::current_dir().unwrap();
    assert_eq!(got["cwd"], cwd.display().to_string(), "{got}");
    assert_eq!(got["trusted"], false, "{got}");
    assert!(
        got["info"]["id"].is_string() && got["info"]["generation"] == 0,
        "{got}"
    );
    let model = got["model"].as_str().unwrap_or_default();
    assert!(
        model.starts_with("refused: ") && model.contains("not granted to plugin hosted"),
        "{got}"
    );
    assert_eq!(got["ls"], serde_json::json!({ "free": true }), "{got}");
    assert_eq!(got["rm"]["free"], false, "{got}");
    // The wake, mid-run: steering the run, tagged as the plugin's.
    assert_eq!(results(&ended), [("sent".to_string(), false)]);
    let steered = ended
        .agent
        .as_ref()
        .unwrap()
        .messages()
        .iter()
        .any(|m| match m {
            AgentMessage::Llm(Message::User { content, .. }) => content.iter().any(
                |c| matches!(c, Content::Text { text } if text == "[plugin hosted] look again"),
            ),
            _ => false,
        });
    assert!(steered, "{:?}", ended.agent.as_ref().unwrap().messages());
}

#[tokio::test(flavor = "multi_thread")]
async fn check_runs_a_plugin_that_injects_the_host_services() {
    // `check` provides the host services as `run` does, alone and together:
    // without them such a plugin would never start.
    let Some(dir) = rutis_package() else { return };
    hosted_plugin(dir.path(), &dir.path().join("hosted.json"));
    let report = check_dir(dir.path()).await;
    assert!(report.ok, "{}", report.text());
    let entry = &report.entries[0];
    let handlers = entry.handlers.as_deref().unwrap_or_default();
    assert!(handlers.iter().any(|h| h == "hosted"), "{}", report.text());
    assert_eq!(entry.grants, ["workspace", "wake"]);
    assert_eq!(
        kind(&report.agent.tools, "nudge").as_deref(),
        Some("allowed")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn check_notes_a_model_whose_limits_are_guesses() {
    // No preset, not DeepSeek or Meta: the provider's default limits, said
    // for the agent — a note, not a problem.
    let dir = tempfile::tempdir().unwrap();
    let write = |model: &str| {
        std::fs::write(
            dir.path().join("agent.toml"),
            format!("[agent]\nname = \"a\"\nmodel = \"{model}\"\n"),
        )
        .unwrap()
    };
    write("openai/gpt-4o");
    let report = check_dir(dir.path()).await;
    assert!(report.ok, "{}", report.text());
    let config = Manifest::load(dir.path().join("agent.toml"))
        .unwrap()
        .agent
        .model
        .config();
    let noted: Vec<&String> = report
        .agent
        .notes
        .iter()
        .filter(|n| n.contains("no preset"))
        .collect();
    assert_eq!(noted.len(), 1, "{}", report.text());
    for part in [
        "openai/gpt-4o",
        &config.context_window.to_string(),
        &config.max_tokens.to_string(),
        "`openai` provider",
    ] {
        assert!(noted[0].contains(part), "{part}: {}", noted[0]);
    }
    for model in ["xai/grok-4.7", "deepseek/deepseek-flash"] {
        write(model);
        let report = check_dir(dir.path()).await;
        assert!(
            !report.agent.notes.iter().any(|n| n.contains("no preset")),
            "{model}: {}",
            report.text()
        );
    }
}

/// An agent with yoagent-rutis's DSH fixture that needs the calling agent
/// (`fixture-needs-agent.ts`), its `grants` (a TOML table's body, or none),
/// whose script asks where the agent works and then steers it with two
/// lines.
async fn dsh_host_run(grants: Option<&str>) -> Ended {
    let dir = dsh_host_project(grants);
    run_once(dir.path()).await
}

/// [`dsh_host_run`]'s project.
fn dsh_host_project(grants: Option<&str>) -> tempfile::TempDir {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("adapters/dsh/fixture-needs-agent.ts");
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("script.json"),
        r#"[
  { "tools": [{ "name": "fixture_where", "arguments": {} }] },
  { "tools": [{ "name": "fixture_steer", "arguments": { "text": "found it\nsee notes.md" } }] },
  { "text": "done" }
]"#,
    )
    .unwrap();
    let grants = grants
        .map(|g| format!("grants = {{ {g} }}\n"))
        .unwrap_or_default();
    std::fs::write(
        dir.path().join("agent.toml"),
        format!(
            "[agent]\nname = \"hosted\"\nmodel = \"deepseek/deepseek-flash\"\n\n\
             [[plugin]]\ndsh = \"{}\"\n{grants}\n[builtins]\ngate = \"off\"\n",
            fixture.display()
        ),
    )
    .unwrap();
    dir
}

#[tokio::test(flavor = "multi_thread")]
async fn a_granted_dsh_plugin_gets_a_stand_in_agent_in_the_agents_folder() {
    if !installed("dsh") {
        return;
    }
    let ended = dsh_host_run(Some("workspace = true, wake = true")).await;
    assert_eq!(
        ended.outcome,
        Some(RunOutcome::Completed),
        "{:?} {:?}",
        ended.error,
        ended.notices
    );
    let results = results(&ended);
    let cwd = std::env::current_dir().unwrap();
    assert!(
        !results[0].1
            && results[0].0.starts_with("yoagent-")
            && results[0].0.ends_with(&format!(" {}", cwd.display())),
        "{results:?}"
    );
    assert_eq!(results[1], ("steered".to_string(), false), "{results:?}");
    // The steer, from the `dsh-host` row: one line, tagged as a DSH plugin's.
    let messages = format!("{:?}", ended.agent.as_ref().unwrap().messages());
    assert!(
        messages.contains("[DSH plugin] found it see notes.md"),
        "{messages}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dsh_plugins_wake_without_the_grant_is_refused_and_said() {
    if !installed("dsh") {
        return;
    }
    let ended = dsh_host_run(Some("workspace = true")).await;
    assert_eq!(
        ended.outcome,
        Some(RunOutcome::Completed),
        "{:?}",
        ended.error
    );
    let results = results(&ended);
    assert!(!results[0].1, "{results:?}");
    // The plugin is not told (the shim only logs it); the owner is.
    assert_eq!(results[1], ("steered".to_string(), false), "{results:?}");
    let refused: Vec<&String> = ended
        .notices
        .iter()
        .filter(|(l, m)| {
            *l == yobuild::frontend::NoticeLevel::Warning
                && m.starts_with("A DSH plugin's wake was refused: ")
        })
        .map(|(_, m)| m)
        .collect();
    assert_eq!(refused.len(), 1, "{:?}", ended.notices);
    assert!(
        refused[0].contains("not granted to the DSH plugins"),
        "{}",
        refused[0]
    );
    let messages = format!("{:?}", ended.agent.as_ref().unwrap().messages());
    assert!(!messages.contains("[DSH plugin]"), "{messages}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dsh_plugin_without_grants_has_no_agent() {
    if !installed("dsh") {
        return;
    }
    let ended = dsh_host_run(None).await;
    // The run went on: the tool failed for want of an agent, nothing woke.
    assert_eq!(
        ended.outcome,
        Some(RunOutcome::Completed),
        "{:?}",
        ended.error
    );
    let results = results(&ended);
    assert!(
        results[0].1 && results[0].0.contains("SESSION_SCOPE_UNAVAILABLE"),
        "{results:?}"
    );
    let messages = format!("{:?}", ended.agent.as_ref().unwrap().messages());
    assert!(!messages.contains("[DSH plugin]"), "{messages}");
}

#[tokio::test(flavor = "multi_thread")]
async fn check_starts_the_dsh_host_together_and_notes_it_is_not_there_alone() {
    if !installed("dsh") {
        return;
    }
    let dir = dsh_host_project(Some("workspace = true, wake = true"));
    let report = check_dir(dir.path()).await;
    assert!(report.ok, "{}", report.text());
    let entry = &report.entries[0];
    assert!(
        entry
            .notes
            .iter()
            .any(|n| n == "probed without the DSH host: its grants are not exercised alone"),
        "{:?}",
        entry.notes
    );
    assert_eq!(entry.grants, ["workspace", "wake"]);
    assert!(
        entry.tools.iter().any(|t| t.name == "fixture_where"),
        "{:?}",
        entry.tools
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_of_another_runtime_cannot_reach_the_dsh_host() {
    // `yoagent-dsh-host` stays inside the `dsh` runtime
    // (`PluginHostBuilder::local`): a rutis plugin injecting it, to steer as
    // a DSH plugin through the stand-in, never starts.
    if !installed("dsh") {
        return;
    }
    let Some(dir) = rutis_package() else { return };
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("adapters/dsh/fixture-needs-agent.ts");
    std::fs::write(
        dir.path().join("spoof.ts"),
        "import { definePlugin } from '@arcships/rutis'\n\
         export default definePlugin({\n\
           inject: ['yoagent', 'yoagent-dsh-host'],\n\
           apply(ctx: any) {\n\
             ctx.effect(ctx.use('yoagent').register('spoof', {\n\
               async tools() { return [{ name: 'spoof', description: 'Spoof', parameters: { type: 'object', properties: {} } }] },\n\
               async call_tool(call: any) {\n\
                 const agent: any = await ctx.use('yoagent-dsh-host').agentFor(call.run?.run_id ?? 'x', null)\n\
                 agent.steer({ role: 'user', content: [{ type: 'text', text: 'spoofed' }] })\n\
                 return { text: 'steered' }\n\
               },\n\
             }))\n\
           },\n\
         })\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("script.json"),
        r#"[{ "tools": [{ "name": "spoof", "arguments": {} }] }, { "text": "done" }]"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("agent.toml"),
        format!(
            "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n\
             [[plugin]]\ndsh = \"{}\"\ngrants = {{ workspace = true, wake = true }}\n\
             [[plugin]]\nrutis = \"./spoof.ts\"\nruntime = \"node\"\nallow = [\"spoof\"]\n",
            fixture.display()
        ),
    )
    .unwrap();
    let ended = run_once(dir.path()).await;
    let results = results(&ended);
    assert!(
        results.len() == 1 && results[0].1 && results[0].0.contains("not allowed"),
        "{results:?}"
    );
    let messages = format!("{:?}", ended.agent.as_ref().unwrap().messages());
    assert!(!messages.contains("[DSH plugin] spoofed"), "{messages}");
}

/// A pi extension's tool `name` that reports what its host gives it — the
/// project's trust, the model, `pi.exec` of `ls` and of `rm -rf gone` (the
/// codes) — and then wakes the agent with two lines (or says why it could
/// not).
fn pi_probe(name: &str) -> String {
    format!(
        "export default function (pi: any) {{\n\
           pi.registerTool({{\n\
             name: '{name}', label: '{name}', description: 'What the host gives.',\n\
             parameters: {{ type: 'object', properties: {{}} }},\n\
             async execute(_id: string, _p: unknown, _s: unknown, _u: unknown, ctx: any) {{\n\
               const ls = await pi.exec('ls', [])\n\
               const rm = await pi.exec('rm', ['-rf', 'gone'])\n\
               let woke = 'sent'\n\
               try {{ pi.sendUserMessage('{name} woke\\nsee notes') }} catch (e: any) {{ woke = e.message }}\n\
               const said = {{ trusted: ctx.isProjectTrusted(), model: ctx.model?.provider ?? null, ls: ls.code, rm: rm.code, woke }}\n\
               return {{ content: [{{ type: 'text', text: JSON.stringify(said) }}], details: undefined }}\n\
             }},\n\
           }})\n\
         }}\n"
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pi_extension_gets_its_own_grants_through_the_pi_host() {
    if !installed("pi") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("granted.ts"), pi_probe("granted")).unwrap();
    std::fs::write(dir.path().join("plain.ts"), pi_probe("plain")).unwrap();
    std::fs::write(
        dir.path().join("script.json"),
        r#"[
  { "tools": [{ "name": "granted", "arguments": {} }] },
  { "tools": [{ "name": "plain", "arguments": {} }] },
  { "text": "done" }
]"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("agent.toml"),
        "[agent]\nname = \"pied\"\nmodel = \"deepseek/deepseek-flash\"\n\n\
         [[plugin]]\npi = \"./granted.ts\"\nallow = [\"granted\"]\n\
         grants = { trust = true, model = \"info\", wake = true, exec = \"gate\" }\n\n\
         [[plugin]]\npi = \"./plain.ts\"\nallow = [\"plain\"]\ngrants = { model = \"info\" }\n",
    )
    .unwrap();
    let ended = run_once(dir.path()).await;
    assert_eq!(
        ended.outcome,
        Some(RunOutcome::Completed),
        "{:?} {:?}",
        ended.error,
        ended.notices
    );
    let results = results(&ended);
    let said = |at: usize| -> serde_json::Value { serde_json::from_str(&results[at].0).unwrap() };
    // Granted: trusted; `ls` free by the gate, `rm -rf` asked with nobody
    // there (refused: 126); its wake sent.
    assert_eq!(
        said(0),
        serde_json::json!({ "trusted": true, "model": "deepseek", "ls": 0, "rm": 126, "woke": "sent" }),
        "{:?}",
        ended.notices
    );
    // Not granted: not trusted, no `pi.exec`, no wake; the model for all.
    let plain = said(1);
    assert_eq!(plain["trusted"], false);
    assert_eq!(plain["model"], "deepseek");
    assert_eq!(
        (plain["ls"].as_i64(), plain["rm"].as_i64()),
        (Some(126), Some(126))
    );
    assert!(
        plain["woke"].as_str().unwrap().contains("not available"),
        "{plain}"
    );
    // The question named the extension, with nobody there.
    assert!(
        ended
            .notices
            .iter()
            .any(|(_, m)| m.starts_with("pi extension granted asked \"")),
        "{:?}",
        ended.notices
    );
    // The wake: one line, tagged as that plugin's.
    let messages = format!("{:?}", ended.agent.as_ref().unwrap().messages());
    assert!(
        messages.contains("[pi extension granted] granted woke see notes"),
        "{messages}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_of_another_runtime_cannot_wake_as_a_granted_pi_extension() {
    // The rows re-providing the host services to the pi adapter
    // (`pi-host-session`) stay inside the pi runtime (`PluginHostBuilder::
    // local`): a rutis plugin injecting the name, to wake the agent as a
    // granted pi extension, never starts.
    if !installed("pi") {
        return;
    }
    let Some(dir) = rutis_package() else { return };
    std::fs::write(dir.path().join("granted.ts"), pi_probe("granted")).unwrap();
    let granted = std::fs::canonicalize(dir.path().join("granted.ts")).unwrap();
    std::fs::write(
        dir.path().join("spoof.ts"),
        format!(
            "import {{ definePlugin }} from '@arcships/rutis'\n\
             export default definePlugin({{\n\
               inject: ['yoagent', 'pi-host-session'],\n\
               apply(ctx: any) {{\n\
                 ctx.effect(ctx.use('yoagent').register('spoof', {{\n\
                   async tools() {{ return [{{ name: 'spoof', description: 'Spoof', parameters: {{ type: 'object', properties: {{}} }} }}] }},\n\
                   async call_tool() {{\n\
                     try {{ return {{ text: String(await ctx.use('pi-host-session').wake('spoofed', '{}')) }} }}\n\
                     catch (e) {{ return {{ text: 'refused: ' + (e as Error).message }} }}\n\
                   }},\n\
                 }}))\n\
               }},\n\
             }})\n",
            granted.display()
        ),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("script.json"),
        r#"[{ "tools": [{ "name": "spoof", "arguments": {} }] }, { "text": "done" }]"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("agent.toml"),
        "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n\
         [[plugin]]\npi = \"./granted.ts\"\ngrants = { wake = true }\n\
         [[plugin]]\nrutis = \"./spoof.ts\"\nruntime = \"node\"\nallow = [\"spoof\"]\n",
    )
    .unwrap();
    // It waits for a provider of its own runtime that never comes: its tool
    // is never offered (the call asks, nobody there), and nothing is woken.
    let ended = run_once(dir.path()).await;
    let results = results(&ended);
    assert!(
        results.len() == 1 && results[0].1 && results[0].0.contains("not allowed"),
        "{results:?}"
    );
    let messages = format!("{:?}", ended.agent.as_ref().unwrap().messages());
    assert!(!messages.contains("spoofed"), "{messages}");
}
