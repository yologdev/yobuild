//! `yobuild run` end to end: an agent from a manifest, its model's replies
//! from a script, an MCP server over stdio (a bash script, as in yoagent's
//! own tests), run `once` without a frontend. Unix (the server is bash).
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use rutis::Ctx;
use yoagent::{AgentMessage, Content, Message};
use yobuild::frontend::RunOutcome;
use yobuild::manifest::Manifest;
use yobuild::run::{run, Ended, Options};

/// Answers `initialize`, lists one tool (`echo`), and answers each call
/// with its text, `$MODE` and its first argument.
const SERVER: &str = r#"#!/usr/bin/env bash
id_of() { printf '%s' "$1" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p'; }
while read -r line; do
  id=$(id_of "$line")
  case "$line" in
    *'"initialize"'*)
      echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"echo\",\"version\":\"1\"}}}" ;;
    *'"tools/list"'*)
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
    let dir = tempfile::tempdir().unwrap();
    let server = dir.path().join("server.sh");
    // `$1` is the first argument the manifest gives.
    std::fs::write(&server, SERVER.replace("$ARG1", "$1")).unwrap();
    std::fs::set_permissions(&server, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(dir.path().join("script.json"), SCRIPT).unwrap();
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

async fn run_once(dir: &Path) -> Ended {
    let manifest = Manifest::load(dir.join("agent.toml")).unwrap();
    let mut options = Options::new(manifest, dir);
    options.scripted = Some(dir.join("script.json"));
    options.task = Some("echo hi".into());
    options.once = true;
    let root = Ctx::root().unwrap();
    let ended = run(&root, options).await.unwrap().expect("a once run ends");
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), root.shutdown()).await;
    ended
}

/// The text of each tool result, and whether it was an error.
fn results(ended: &Ended) -> Vec<(String, bool)> {
    ended
        .agent
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
    assert_eq!(ended.outcome, RunOutcome::Completed, "{:?}", ended.error);
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

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_key_stops_the_run_before_it_starts() {
    let dir = project("");
    let manifest = Manifest::load(dir.path().join("agent.toml")).unwrap();
    // No script: the provider needs its key, and the test's environment has none.
    if manifest.agent.model.api_key().is_some() {
        return;
    }
    let root = Ctx::root().unwrap();
    let Err(e) = run(&root, Options::new(manifest, dir.path())).await else {
        panic!("it started without a key")
    };
    assert_eq!(
        e.to_string(),
        "deepseek/deepseek-flash needs its key in DEEPSEEK_API_KEY"
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
async fn a_pi_extension_from_the_manifest_gives_its_tool() {
    if !installed("pi") {
        return;
    }
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("adapters/pi/fixture-extension.ts");
    let ended = run_plugin(&format!("pi = \"{}\"", fixture.display()), "pi_echo").await;
    assert_eq!(ended.outcome, RunOutcome::Completed, "{:?}", ended.error);
    assert_eq!(results(&ended), [("pi echo: hi".to_string(), false)]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dsh_plugin_from_the_manifest_gives_its_tool() {
    if !installed("dsh") {
        return;
    }
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("adapters/dsh/fixture-tools.ts");
    let ended = run_plugin(&format!("dsh = \"{}\"", fixture.display()), "fixture_echo").await;
    assert_eq!(ended.outcome, RunOutcome::Completed, "{:?}", ended.error);
    let results = results(&ended);
    assert_eq!(results.len(), 1, "{results:?}");
    assert!(
        !results[0].1 && results[0].0.contains("echo: hi"),
        "{results:?}"
    );
}
