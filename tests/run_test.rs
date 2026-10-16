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
      echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"echoed $text ($MODE, $ARG1)\"}]}}" ;;
  esac
done
"#;

const SCRIPT: &str = r#"[
  { "tools": [{ "name": "echo", "arguments": { "text": "hi" } }] },
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

/// Runs the project's agent once on its script.
async fn try_once(dir: &Path) -> Result<Ended, String> {
    let manifest = Manifest::load(dir.join("agent.toml")).map_err(|e| e.to_string())?;
    let mut options = Options::new(manifest);
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
    let dir = project("[parts]\ngate = \"off\"\n");
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
            "[agent]\nname = \"plugged\"\nmodel = \"deepseek/deepseek-flash\"\n\n[[plugin]]\n{plugin}\n\n[parts]\ngate = \"off\"\n"
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
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_a_limit_stops_is_stopped_not_completed() {
    let echo = r#"{ "tools": [{ "name": "echo", "arguments": { "text": "hi" } }] }"#;
    let dir = project_with(
        "[limits]\nmax_turns = 1\n[parts]\ngate = \"off\"\n",
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
}

#[tokio::test(flavor = "multi_thread")]
async fn the_report_sends_the_agent_back_until_it_is_there() {
    let dir = project_with(
        "[parts]\ngate = \"off\"\nreport = [\"Done\"]\n",
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
        "[parts]\ngate = \"off\"\nreport = [\"Done\"]\n",
        r#"[{ "text": "finished" }]"#,
    );
    let ended = run_once(dir.path()).await;
    assert_eq!(ended.outcome, Some(RunOutcome::Error));
    let error = ended.error.unwrap_or_default();
    assert!(error.contains("not accepted after 3 continues"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_owner_is_offered_when_the_manifest_asks_for_it_and_the_gate_lets_it_run() {
    let script = r#"[
      { "tools": [{ "name": "ask_owner", "arguments": { "question": "Which?", "options": ["a", "b"] } }] },
      { "text": "done" }
    ]"#;
    let on = project_with("[parts]\nask_owner = true\n", script);
    let ended = run_once(on.path()).await;
    let asked = results(&ended);
    assert!(
        !asked[0].1 && asked[0].0.contains("Asked the owner as #1"),
        "{asked:?}"
    );
    let off = project_with("", script);
    let ended = run_once(off.path()).await;
    assert!(results(&ended)[0].1, "no ask_owner without the part");
}

#[tokio::test(flavor = "multi_thread")]
async fn two_tools_of_one_name_and_a_server_without_tools_are_refused() {
    let twice =
        project("[[mcp]]\nname = \"other\"\nnpm = \"./server.sh\"\n[parts]\ngate = \"off\"\n");
    let e = try_once(twice.path()).await.err().unwrap();
    assert_eq!(e, "two tools named echo: from mcp server and mcp other");
    let empty =
        project("[[mcp]]\nname = \"empty\"\nnpm = \"./server.sh\"\nenv = { TOOLS = \"none\" }\n");
    let e = try_once(empty.path()).await.err().unwrap();
    assert_eq!(e, "mcp empty: the server lists no tools");
    let bash = project_with("", SCRIPT);
    std::fs::write(
        bash.path().join("agent.toml"),
        "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n[[mcp]]\nnpm = \"./server.sh\"\nenv = { TOOLS = \"none\" }\n",
    )
    .unwrap();
    assert_eq!(
        try_once(bash.path()).await.err().unwrap(),
        "mcp server: the server lists no tools"
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
    std::fs::write(dir.path().join("script.json"), SCRIPT).unwrap();
    std::fs::write(
        dir.path().join("agent.toml"),
        format!(
            "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n\
             [[mcp]]\nname = \"web\"\nurl = \"http://{addr}/mcp\"\n\
             headers = {{ Authorization = \"Bearer ${{YOBUILD_RUN_TEST_TOKEN}}\" }}\n\
             [parts]\ngate = \"off\"\n"
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
async fn a_pinned_dsh_package_is_found_in_the_dsh_runtime() {
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
            format!("[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n[[plugin]]\ndsh = \"{name}@{version}\"\n{peer}[parts]\ngate = \"off\"\n"),
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
    manifest("9.9.9", false);
    let e = try_once(dir.path()).await.err().unwrap();
    assert!(
        e.contains(&format!("is installed at {version}")) && e.contains("--no-save"),
        "{e}"
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
                "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n[[plugin]]\ndsh = \"{}\"\n[parts]\ngate = \"off\"\n{extra}",
                fixture.display()
            ),
        )
        .unwrap()
    };
    manifest("");
    let e = try_once(dir.path()).await.err().unwrap();
    assert!(
        e.starts_with("DSH plugins wait for services nobody provides: fs,")
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
    let dir = project("[parts]\ngate = \"off\"\n[dsh]\nhost_services = true\n");
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
    let dir = project_with("", SCRIPT);
    let package = dir.path().join("node_modules/srv");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::rename(dir.path().join("server.sh"), package.join("server.sh")).unwrap();
    std::fs::write(
        package.join("package.json"),
        r#"{ "name": "srv", "version": "1.0.0", "bin": "./server.sh" }"#,
    )
    .unwrap();
    let manifest = |spec: &str| {
        std::fs::write(
            dir.path().join("agent.toml"),
            format!(
                "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n[[mcp]]\nnpm = \"{spec}\"\nargs = [\"first\"]\nenv = {{ MODE = \"loud\" }}\n[parts]\ngate = \"off\"\n"
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
    manifest("srv@1.0.1");
    let e = try_once(dir.path()).await.err().unwrap();
    assert!(
        e.starts_with("mcp srv: srv is installed at 1.0.0") && e.contains("not 1.0.1"),
        "{e}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_that_stops_during_the_session_is_said() {
    if !installed("dsh") {
        return;
    }
    // It starts, then a while later takes its runtime down with it (an
    // error nothing catches), while the run is still going.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), r#"{ "type": "module" }"#).unwrap();
    std::fs::write(
        dir.path().join("fails-later.ts"),
        "export const name = 'fails-later'\n\
         export function apply(ctx: any) {\n\
           setTimeout(() => { throw new Error('gave up') }, 4000)\n\
         }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("script.json"),
        r#"[{ "tools": [{ "name": "bash", "arguments": { "command": "sleep 7" } }] }, { "text": "done" }]"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("agent.toml"),
        format!(
            "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n[[plugin]]\ndsh = \"{}\"\n[parts]\ngate = \"off\"\n",
            dir.path().join("fails-later.ts").display()
        ),
    )
    .unwrap();
    let ended = run_once(dir.path()).await;
    let said = ended.notices.iter().any(|(level, message)| {
        *level == yobuild::frontend::NoticeLevel::Error
            && message.starts_with("A plugin stopped: plugin plugin:fails-later (")
            && message.ends_with(") is not running")
    });
    assert!(said, "{:?}", ended.notices);
}
