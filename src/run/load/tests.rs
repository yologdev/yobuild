use std::path::Path;

use serde_json::json;
use yoagent::mcp::types::{JsonRpcRequest, JsonRpcResponse};
use yoagent::mcp::{McpError, McpToolInfo};
use yoagent::{AgentTool, ToolContext, ToolError};

use super::*;
use crate::builtins::{Candidate, FindPlugin, Sources};
use crate::frontend::{Driver, NoticeLevel, Session};
use crate::manifest::{ExtendKind, Manifest};

#[test]
fn a_package_is_named_without_its_scope_and_mcp_words() {
    for (package, name) in [
        ("@eslint/mcp", "eslint"),
        ("@modelcontextprotocol/server-filesystem", "filesystem"),
        ("mcp-server-git", "git"),
        ("lint-mcp", "lint"),
        ("xlsx-mcp-server", "xlsx"),
        ("@acme/mcp-server", "acme"),
        ("plain", "plain"),
        ("mcp", "mcp"),
        ("@a/server-x-mcp", "x"),
    ] {
        assert_eq!(default_name(package), name, "{package}");
    }
}

#[test]
fn a_name_must_prefix_tools_a_model_takes() {
    assert_eq!(name_problem("eslint"), None);
    assert_eq!(name_problem("a-b_c9"), None);
    assert!(name_problem("").is_some());
    assert!(name_problem("files.js").unwrap().contains("only letters"));
    assert!(name_problem("a__b").unwrap().contains("`__`"));
    assert_eq!(name_problem(&"x".repeat(61)), None);
    assert!(name_problem(&"x".repeat(62)).unwrap().contains("too long"));
}

fn info(name: &str) -> McpToolInfo {
    McpToolInfo {
        name: name.into(),
        description: Some(format!("does {name}")),
        input_schema: json!({ "type": "object" }),
    }
}

fn names(tools: &[&str]) -> Vec<String> {
    tools.iter().map(|t| t.to_string()).collect()
}

#[test]
fn the_listed_tools_are_judged_as_a_manifests_servers_are() {
    assert_eq!(
        tools_problem("eslint", &names(&["lint-files", "fix"])),
        Ok(vec!["lint-files".to_owned(), "fix".to_owned()])
    );
    assert_eq!(
        tools_problem("eslint", &[]),
        Err("it lists no tools".to_owned())
    );
    let long = "t".repeat(60);
    assert!(tools_problem("eslint", &names(&[&long]))
        .unwrap_err()
        .contains("load it under a shorter `name`"));
    assert!(tools_problem("eslint", &names(&["read file"]))
        .unwrap_err()
        .contains("cannot be offered to a model"));
    assert!(tools_problem("eslint", &names(&["a", "a"]))
        .unwrap_err()
        .contains("twice"));
}

#[test]
fn an_argument_naming_a_path_outside_the_folder_is_marked() {
    let cwd = Path::new("/work/project");
    for (arg, out) in [
        (".", false),
        ("src", false),
        ("/work/project/src", false),
        ("--root=/work/project", false),
        ("/etc", true),
        ("~/notes", true),
        ("../other", true),
        ("src/../../x", true),
        ("src/../lib", false),
        ("--root=/", true),
        ("--verbose", false),
    ] {
        assert_eq!(outside(arg, cwd), out, "{arg}");
    }
}

fn kept(name: &str, args: &[&str]) -> Kept {
    Kept {
        kind: ExtendKind::Mcp,
        name: name.into(),
        package: "@eslint/mcp".into(),
        version: "0.3.15".into(),
        args: args.iter().map(|a| a.to_string()).collect(),
        tools: vec!["lint-files".into()],
        stopped: None,
    }
}

fn files() -> Files {
    Files {
        manifest: "handoff.toml".into(),
        lock: "handoff.lock".into(),
        shared: Vec::new(),
    }
}

#[test]
fn the_end_says_each_server_and_the_block_that_keeps_it() {
    assert_eq!(
        kept("eslint", &[]).block(),
        "[[mcp]]\nname = \"eslint\"\nnpm  = \"@eslint/mcp@0.3.15\"\n# allow = [\"lint-files\"]   # tools that would run without asking\n"
    );
    let text = kept_text(&[kept("eslint", &["--dir", "a \"b\""])], &files());
    assert!(
        text.starts_with(
            "Loaded this session: @eslint/mcp@0.3.15 as eslint (tools: lint-files). To keep it, add to handoff.toml:\n\n[[mcp]]\n"
        ),
        "{text}"
    );
    assert!(
        text.ends_with("writes handoff.lock, which --locked needs."),
        "{text}"
    );
    assert!(
        text.contains("args = [\"--dir\", \"a \\\"b\\\"\"]\n"),
        "{text}"
    );
    assert!(text.ends_with("which --locked needs."), "{text}");
    let mut stopped = kept("b", &[]);
    stopped.stopped = Some("it exited with 1".into());
    let both = kept_text(&[kept("a", &[]), stopped], &files());
    assert!(both.contains(
        "as a (tools: lint-files); @eslint/mcp@0.3.15 as b (tools: lint-files, stopped this session: it exited with 1)"
    ));
    assert!(both.contains("To keep them, add"));
    assert!(both.contains("resolves their dependencies"));
}

/// A pi or DSH package's block is a `[[plugin]]`, said with how it would
/// run in the manifest and the grants it would share; the sets named.
#[test]
fn the_end_says_a_package_as_a_plugin_block_and_what_it_would_share() {
    let pi = Kept {
        kind: ExtendKind::Pi,
        name: "memory".into(),
        package: "pi-memory".into(),
        version: "0.4.2".into(),
        args: Vec::new(),
        tools: vec!["memory_add".into(), "scratchpad".into()],
        stopped: None,
    };
    assert_eq!(
        pi.block(),
        "[[plugin]]\nname = \"memory\"\npi   = \"pi-memory@0.4.2\"\n# allow = [\"memory_add\", \"scratchpad\"]   # tools that would run without asking\n"
    );
    let dsh = Kept {
        kind: ExtendKind::Dsh,
        name: "office".into(),
        package: "dsh-univer-office".into(),
        ..pi.clone()
    };
    assert!(dsh
        .block()
        .contains("\ndsh  = \"dsh-univer-office@0.4.2\"\n"));
    let text = kept_text(&[kept("eslint", &[]), pi.clone()], &files());
    assert!(
        text.contains("\nIn handoff.toml a pi package runs with the other pi plugins, in the `pi` runtime, its hooks too, its tools under their own names.\n"),
        "{text}"
    );
    assert!(!text.contains("DSH package runs"), "{text}");
    assert!(
        text.ends_with("(the lock's `mcp` and `pi` sets, without the age limit) and writes handoff.lock, which --locked needs."),
        "{text}"
    );
    let sharing = Files {
        shared: vec![(ExtendKind::Pi, "the pi plugins' `model`".into())],
        ..files()
    };
    let text = kept_text(&[pi], &sharing);
    assert!(
        text.contains("its tools under their own names: give it the pi plugins' `model`, as they share them.\n"),
        "{text}"
    );

    // What a manifest's plugins would have it share.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("e.ts"), "").unwrap();
    std::fs::write(dir.path().join("d.ts"), "").unwrap();
    let text = "[agent]\nname = \"t\"\nmodel = \"deepseek/deepseek-flash\"\n\n[[plugin]]\npi = \"./e.ts\"\ngrants = { model = \"info\", env = [\"LANG\"] }\n\n[[plugin]]\ndsh = \"./d.ts\"\ngrants = { workspace = true }\n";
    let m = Manifest::parse(text, &dir.path().join("agent.toml"), dir.path()).unwrap();
    assert_eq!(
        shared(&m),
        [
            (
                ExtendKind::Pi,
                "the pi plugins' `model` and `env` (LANG)".to_owned()
            ),
            (
                ExtendKind::Dsh,
                "the DSH plugins' grants (workspace)".to_owned()
            ),
        ]
    );
}

/// The block reads back as the manifest server it says: quotes,
/// backslashes and other scripts in its arguments too.
#[test]
fn the_block_that_keeps_a_server_is_a_manifest_entry() {
    let dir = tempfile::tempdir().unwrap();
    let args = ["--dir", "a \"b\" \\ c", "日本語", "x=${NOT}"];
    let block = kept("eslint", &args[..3]).block();
    let text = format!("[agent]\nname = \"t\"\nmodel = \"deepseek/deepseek-flash\"\n\n{block}");
    let m = Manifest::parse(&text, &dir.path().join("agent.toml"), dir.path()).unwrap();
    let [server] = &m.mcp[..] else {
        panic!("{:?}", m.mcp)
    };
    assert_eq!(server.name, "eslint");
    assert!(server.allow.is_empty(), "the allow line is a comment");
    match &server.transport {
        crate::manifest::Transport::Stdio {
            source, args: read, ..
        } => {
            assert!(
                matches!(source, crate::manifest::Source::Package(p) if p.spec() == "@eslint/mcp@0.3.15")
            );
            let read: Vec<&str> = read.iter().map(|v| v.as_str()).collect();
            assert_eq!(read, args[..3]);
        }
        other => panic!("{other:?}"),
    }
    // What would not read back is refused when the agent passes it.
    assert!(crate::manifest::loaded_arguments(&["a\u{7f}".into()])
        .unwrap_err()
        .contains("control character"));
    assert!(crate::manifest::loaded_arguments(&[args[3].into()]).is_err());
}

/// A transport that answers nothing (its calls are never made here).
struct Mute;

#[async_trait::async_trait]
impl McpTransport for Mute {
    async fn send(&self, _: JsonRpcRequest) -> Result<JsonRpcResponse, McpError> {
        Err(McpError::ConnectionClosed)
    }
    async fn close(&self) -> Result<(), McpError> {
        Ok(())
    }
}

/// What a stand-in exit watch answers: set it to say the server has gone.
type Exit = Arc<Mutex<Option<String>>>;

fn server(name: &str, exit: &Exit) -> Server {
    let client = Arc::new(tokio::sync::Mutex::new(McpClient::from_transport(
        Box::new(Mute),
    )));
    let tool: Arc<dyn AgentTool> =
        Arc::new(McpToolAdapter::new(client.clone(), info("lint-files")).with_prefix(name));
    let exit = exit.clone();
    Server::new(
        name.into(),
        vec![tool],
        client,
        ExitWatch::new(move || locked(&exit).clone()),
    )
}

/// `exit` says its server has gone with `why`, said stopped once settled.
async fn stop(loaded: &Loaded, exit: &Exit, why: &str) {
    *locked(exit) = Some(why.into());
    loaded.stopped();
    tokio::time::sleep(SETTLE).await;
    assert_eq!(loaded.stopped().len(), 1);
}

fn add(loaded: &Loaded, name: &str) -> Exit {
    let exit = Exit::default();
    loaded.add(kept(name, &[]), server(name, &exit));
    exit
}

#[tokio::test]
async fn loaded_servers_are_a_tool_source_that_moves_on_each_change() {
    let loaded = Loaded::default();
    assert_eq!(loaded.generation(), 0);
    assert!(loaded.tools().await.is_empty());
    add(&loaded, "eslint");
    let gone = add(&loaded, "gone");
    assert_eq!(loaded.generation(), 2);
    assert_eq!(loaded.tools().await.len(), 2);
    // Its watch says it has gone (yoagent's words, its wrapper named): the
    // generation moves once, at the first look, and its tools leave.
    *locked(&gone) = Some("MCP server '/usr/bin/env' exited with exit status: 1".into());
    assert_eq!(loaded.generation(), 3);
    assert_eq!(loaded.generation(), 3, "once");
    let names: Vec<String> = loaded
        .tools()
        .await
        .iter()
        .map(|t| t.name().to_owned())
        .collect();
    assert_eq!(names, ["eslint__lint-files"]);
    // Asked by membership: a loaded tool, not a name that merely looks so.
    assert!(loaded.asks("eslint__lint-files"));
    assert!(!loaded.asks("eslint__other"));
    assert!(!loaded.asks("lint-files"));
    // Said stopped only once its reason has settled, with the text as it
    // is then; once, then kept marked stopped; still asked.
    assert!(loaded.stopped().is_empty(), "not settled yet");
    *locked(&gone) =
        Some("MCP server '/usr/bin/env' exited with exit status: 1; last stderr:\nboom".into());
    tokio::time::sleep(SETTLE).await;
    let why = "it exited with exit status: 1; last stderr: boom";
    assert_eq!(loaded.stopped(), [("gone".to_owned(), why.to_owned())]);
    assert!(loaded.stopped().is_empty());
    assert_eq!(loaded.generation(), 3, "the list did not change again");
    assert_eq!(loaded.tools().await.len(), 1);
    assert!(loaded.asks("gone__lint-files"));
    let kept = loaded.kept();
    assert_eq!(kept.len(), 2);
    assert_eq!(kept[1].stopped.as_deref(), Some(why));
    assert_eq!(kept[0].stopped, None);
    loaded.close().await;
    assert!(loaded.tools().await.is_empty());
    assert_eq!(loaded.generation(), 4, "closing moves it");
}

/// A real stdio server (`/bin/sh`) on yoagent's transport, with one tool.
async fn real(loaded: &Loaded, name: &str, script: &str) {
    let inner = yoagent::mcp::StdioTransport::new("/bin/sh", &["-c", script], None)
        .await
        .unwrap();
    let exit = inner.exit_watch().expect("stdio gives a watch");
    let client = Arc::new(tokio::sync::Mutex::new(McpClient::from_transport(
        Box::new(inner),
    )));
    let tool: Arc<dyn AgentTool> =
        Arc::new(McpToolAdapter::new(client.clone(), info("echo")).with_prefix(name));
    loaded.add(
        kept(name, &[]),
        Server::new(name.into(), vec![tool], client, exit),
    );
}

/// A real stdio server that dies while idle: its watch says so with no
/// call made, its tools leave at the first look, and it is said once its
/// reason settled, the stderr tail in it.
#[tokio::test]
async fn a_server_that_dies_while_idle_is_seen_without_a_call() {
    let loaded = Loaded::default();
    real(&loaded, "sh", "sleep 0.3; echo bye >&2; exit 3").await;
    assert_eq!(loaded.tools().await.len(), 1, "running");
    let start = Instant::now();
    let mut stopped = Vec::new();
    let mut gone_seen = None;
    while start.elapsed() < Duration::from_secs(4) && stopped.is_empty() {
        if gone_seen.is_none() && loaded.tools().await.is_empty() {
            gone_seen = Some(Instant::now());
        }
        stopped = loaded.stopped();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let gone_seen = gone_seen.expect("its tools left");
    assert!(
        gone_seen.elapsed() >= SETTLE - Duration::from_millis(60),
        "said only once settled"
    );
    assert_eq!(
        stopped,
        [(
            "sh".to_owned(),
            "it exited with exit status: 3; last stderr: bye".to_owned()
        )]
    );
    assert_eq!(loaded.generation(), 2, "added, then gone");
}

/// The servers yobuild closes are never said stopped.
#[tokio::test]
async fn a_server_yobuild_closes_is_not_said_stopped() {
    let loaded = Loaded::default();
    real(&loaded, "sh", "sleep 30").await;
    loaded.close().await;
    tokio::time::sleep(SETTLE).await;
    assert!(loaded.stopped().is_empty());
    assert!(loaded.stopped_at_end(None).await.is_empty());
    assert_eq!(loaded.kept()[0].stopped, None);
}

/// At the session's end a server just seen gone is waited for and marked
/// stopped in the block; after a Ctrl+C, one first seen after it or
/// ended by its SIGINT is not.
#[tokio::test]
async fn the_end_marks_a_server_just_gone_but_not_one_the_ctrl_c_stopped() {
    let loaded = Loaded::default();
    let early = add(&loaded, "early");
    let sigint = add(&loaded, "sigint");
    let late = add(&loaded, "late");
    *locked(&early) = Some("MCP server '/usr/bin/env' exited with exit status: 1".into());
    *locked(&sigint) = Some("MCP server '/usr/bin/env' exited with signal: 2 (SIGINT)".into());
    loaded.generation();
    let ctrl_c = Instant::now();
    *locked(&late) = Some("MCP server '/usr/bin/env' exited with exit status: 0".into());
    let said = loaded.stopped_at_end(Some(ctrl_c)).await;
    assert_eq!(
        said,
        [(
            "early".to_owned(),
            "it exited with exit status: 1".to_owned()
        )]
    );
    let text = kept_text(&loaded.kept(), &files());
    assert!(
        text.contains("stopped this session: it exited with exit status: 1"),
        "{text}"
    );
    assert_eq!(text.matches("stopped this session").count(), 1, "{text}");

    // Without a Ctrl+C: one first seen at the end is waited for, and said.
    let loaded = Loaded::default();
    let gone = add(&loaded, "gone");
    *locked(&gone) = Some("MCP server '/usr/bin/env' exited with exit status: 1".into());
    let start = Instant::now();
    assert_eq!(loaded.stopped_at_end(None).await.len(), 1);
    assert!(start.elapsed() >= SETTLE, "its reason left to settle");
    assert!(loaded.kept()[0].stopped.is_some());
}

/// The longest reason read is kept: yoagent answers without the stderr
/// tail while a call holds the child.
#[tokio::test]
async fn the_reason_said_is_the_longest_read() {
    let loaded = Loaded::default();
    let exit = add(&loaded, "x");
    *locked(&exit) = Some("MCP server 'sh' exited with exit status: 1; last stderr:\nboom".into());
    loaded.generation();
    *locked(&exit) = Some("MCP server 'sh' exited with exit status: 1".into());
    tokio::time::sleep(SETTLE).await;
    assert_eq!(
        loaded.stopped()[0].1,
        "it exited with exit status: 1; last stderr: boom"
    );
}

#[test]
fn a_start_that_fails_names_the_server_not_its_wrapper() {
    assert_eq!(
        said("Transport error: Connection closed: MCP server '/usr/bin/env' exited with exit status: 3; last stderr:\nCannot find module\n  at x"),
        "it exited with exit status: 3; last stderr: Cannot find module at x"
    );
    assert_eq!(
        said("Transport error: Failed to spawn '/usr/bin/env': No such file or directory"),
        "it could not be started: No such file or directory"
    );
    assert_eq!(
        said("no answer to `initialize` within 30 s"),
        "no answer to `initialize` within 30 s"
    );
    // The exit watch's words, each.
    for (watch, ours) in [
        (
            "MCP server '/usr/bin/env' closed its output (its process is still running)",
            "it closed its output (its process is still running)",
        ),
        (
            "MCP server '/usr/bin/env' has gone (exit status unknown: ECHILD)",
            "it has gone (exit status unknown: ECHILD)",
        ),
        (
            "MCP server '/usr/bin/env' was closed (`close`)",
            "it was closed (`close`)",
        ),
        (
            "MCP server '/bin/sh' was closed: its client was dropped",
            "it was closed: its client was dropped",
        ),
    ] {
        assert_eq!(said(watch), ours);
    }
    // A long stderr tail keeps its end (where a crash is).
    let tail: String = (0..200).map(|i| format!("line{i}\n")).collect();
    let long = said(&format!(
        "MCP server '/usr/bin/env' exited with exit status: 1; last stderr:\n{tail}"
    ));
    assert!(
        long.starts_with("it exited with exit status: 1; last stderr: …"),
        "{long}"
    );
    assert!(long.ends_with("line198 line199"), "{long}");
    assert!(
        long.chars().count() <= 400 + "; last stderr: ".len(),
        "{long}"
    );
}

#[test]
fn a_package_with_several_programs_starts_the_one_named_like_it() {
    let dir = tempfile::tempdir().unwrap();
    let write = |bin: serde_json::Value| {
        std::fs::write(
            dir.path().join("package.json"),
            json!({ "name": "@a/tool", "bin": bin }).to_string(),
        )
        .unwrap()
    };
    write(json!({ "tool": "a.js", "other": "b.js" }));
    assert_eq!(bin(dir.path(), "@a/tool").unwrap(), dir.path().join("a.js"));
    write(json!({ "x": "a.js", "y": "b.js" }));
    let e = bin(dir.path(), "@a/tool").unwrap_err();
    assert!(
        e.starts_with("@a/tool has several programs (its `bin`) and yobuild cannot tell which to start: @a/tool has no bin \"tool\""),
        "{e}"
    );
    write(json!("only.js"));
    assert_eq!(
        bin(dir.path(), "@a/tool").unwrap(),
        dir.path().join("only.js")
    );
}

#[test]
fn what_its_code_reads_and_what_the_scan_missed_are_said() {
    let dir = tempfile::tempdir().unwrap();
    // No code: said, not taken for "reads nothing".
    let none = reads(dir.path(), &[]);
    assert_eq!(none.names, Vec::<String>::new());
    assert!(none
        .missed
        .unwrap()
        .contains("no JavaScript or TypeScript code"));
    // A name set here (this process has `PATH` and the test's own) that
    // the kept set keeps is not said; one withheld is.
    std::fs::write(
        dir.path().join("index.js"),
        "process.env.PATH; process.env.CARGO_PKG_NAME;",
    )
    .unwrap();
    let r = reads(dir.path(), &["CARGO_PKG_NAME".into()]);
    assert_eq!(r.names, ["CARGO_PKG_NAME"]);
    assert_eq!(r.missed, None);
    let details = first_details(
        &candidate("x", Ok("1.0.0")),
        "1.0.0",
        "x",
        &[],
        dir.path(),
        &[],
        &Reads {
            names: vec![],
            missed: Some("1 link(s) not followed".into()),
        },
        &[],
    );
    assert!(
        details.contains("Its code was scanned in part for what it reads (1 link(s) not followed): there may be more"),
        "{details}"
    );
}

fn manifest(dir: &Path) -> Manifest {
    let text = r#"
[agent]
name   = "t"
model  = "deepseek/deepseek-flash"

[[mcp]]
name = "taken"
npm  = "./server.js"

[extend]
max_loads    = 1
max_attempts = 2
"#;
    Manifest::parse(text, &dir.join("agent.toml"), dir).unwrap()
}

fn candidate(package: &str, version: Result<&str, &str>) -> Candidate {
    Candidate {
        need: "lint".into(),
        kind: ExtendKind::Mcp,
        package: package.into(),
        version: version.map(String::from).map_err(String::from),
        registry_name: None,
        from_npm: true,
        description: String::new(),
        licence: None,
        repository: None,
        published: None,
        downloads: Err(String::new()),
        provenance: None,
        required_env: None,
        required_args: None,
    }
}

fn tool(once: bool) -> (LoadPlugin, Driver, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let manifest = manifest(dir.path());
    let (session, driver) = Session::new(false);
    let sources = Sources::at("http://127.0.0.1:9").unwrap();
    let extend = manifest.extend.clone().unwrap();
    let find = FindPlugin::new(session.clone(), extend.clone(), sources);
    let mut needs = candidate("needs-env", Ok("1.0.0"));
    needs.required_env = Some(vec!["API_TOKEN".into()]);
    let mut pi = candidate("pi-lint", Ok("1.0.0"));
    pi.kind = ExtendKind::Pi;
    // Found as a pi package first, then as the MCP server it also is:
    // loaded as the server (its version, 1.2.0, below).
    let mut lint_as_pi = candidate("lint-mcp", Ok("9.9.9"));
    lint_as_pi.kind = ExtendKind::Pi;
    let mut dsh = candidate("dsh-lint", Ok("1.0.0"));
    dsh.kind = ExtendKind::Dsh;
    find.remember(&[lint_as_pi]);
    find.remember(&[
        pi,
        dsh,
        candidate("lint-mcp", Ok("1.2.0")),
        candidate("young", Err("no version was published 7 or more days ago")),
        candidate("taken-mcp", Ok("1.0.0")),
        needs,
    ]);
    let load = LoadPlugin::new(
        session,
        files(),
        &manifest,
        extend,
        find,
        Loaded::default(),
        once,
        Some(dir.path().join("home")),
        dir.path(),
        None,
    );
    (load, driver, dir)
}

async fn refusal(load: &LoadPlugin, args: serde_json::Value) -> String {
    match load
        .execute(args, ToolContext::new("1", LoadPlugin::TOOL))
        .await
    {
        Err(ToolError::Failed(why)) | Err(ToolError::InvalidArgs(why)) => why,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn what_the_limits_refuse_before_anything_is_installed() {
    let (load, _driver, _dir) = tool(false);
    let cases = [
        (json!("x"), "the arguments must be an object"),
        (
            json!({ "package": "lint-mcp", "version": "1.2.0", "env": {} }),
            "unknown key `env`",
        ),
        (json!({ "version": "1.2.0" }), "`package` is required"),
        (
            json!({ "package": "lint-mcp", "version": 1 }),
            "`version` must be a string, not 1",
        ),
        (
            json!({ "package": "lint-mcp", "version": "1.2.0", "args": "x" }),
            "`args` must be a list of strings",
        ),
        (
            json!({ "package": "lodash", "version": "1.0.0" }),
            "lodash is not among this session's candidates",
        ),
        (
            json!({ "package": "young", "version": "1.0.0" }),
            "young is not loadable: no version was published 7 or more days ago",
        ),
        (
            json!({ "package": "young" }),
            "young is not loadable: no version was published 7 or more days ago",
        ),
        (
            json!({ "package": "lint-mcp", "version": "1.3.0" }),
            "lint-mcp@1.3.0 is not the version find_plugin offered: lint-mcp@1.2.0 (or give no `version`)",
        ),
    ];
    for (args, said) in cases {
        let why = refusal(&load, args.clone()).await;
        assert!(why.contains(said), "{args}: {why}");
    }
    assert_eq!(locked(&load.state).attempts, 0, "none of these counted");
    assert_eq!(load.parameters_schema()["required"], json!(["package"]));
    // Counted from here: a name in use (a `null` version is none: the
    // offered one), then the attempts spent.
    let why = refusal(&load, json!({ "package": "taken-mcp", "version": null })).await;
    assert!(
        why.contains(
            "the name \"taken\" is in use by another plugin or server: pass another `name`"
        ),
        "{why}"
    );
    let why = refusal(
        &load,
        json!({ "package": "needs-env", "version": "1.0.0", "name": "n" }),
    )
    .await;
    assert!(
        why.contains("environment variables it requires (API_TOKEN)"),
        "{why}"
    );
    let why = refusal(&load, json!({ "package": "lint-mcp", "version": "1.2.0" })).await;
    assert!(
        why.contains("calls are spent ([extend] max_attempts 2)"),
        "{why}"
    );
    assert!(!locked(&load.state).busy, "nothing left in progress");
}

/// A package takes no `args`; without a plugin host (`check`'s tool) it is
/// refused before anything is installed. Both counted.
#[tokio::test]
async fn a_package_takes_no_arguments_and_needs_a_plugin_host() {
    let (load, _driver, _dir) = tool(false);
    let why = refusal(&load, json!({ "package": "pi-lint", "args": ["-v"] })).await;
    assert!(
        why.contains("a pi package takes no `args` (they are an MCP server's)"),
        "{why}"
    );
    let why = refusal(&load, json!({ "package": "dsh-lint" })).await;
    assert!(
        why.contains("this session runs no plugin host to load it in"),
        "{why}"
    );
    assert_eq!(locked(&load.state).attempts, 2);
    assert!(!locked(&load.state).busy);
}

#[tokio::test]
async fn names_and_arguments_are_refused_before_the_install() {
    let (load, _driver, _dir) = tool(false);
    let why = refusal(
        &load,
        json!({ "package": "lint-mcp", "version": "1.2.0", "name": "a.b" }),
    )
    .await;
    assert!(why.contains("takes only letters, digits"), "{why}");
    let why = refusal(
        &load,
        json!({ "package": "lint-mcp", "version": "1.2.0", "args": ["--token", "abc"] }),
    )
    .await;
    assert!(why.contains("looks like a secret"), "{why}");
    // Two attempts spent: the third is refused whatever it asks.
    let why = refusal(
        &load,
        json!({ "package": "lint-mcp", "version": "1.2.0", "args": ["${HOME}"] }),
    )
    .await;
    assert!(why.contains("max_attempts"), "{why}");
}

#[tokio::test]
async fn args_with_a_reference_are_refused() {
    let (load, _driver, _dir) = tool(false);
    let why = refusal(
        &load,
        json!({ "package": "lint-mcp", "version": "1.2.0", "args": ["--dir", "${HOME}"] }),
    )
    .await;
    assert!(
        why.contains("refers to a variable: nothing fills one in for a loaded server"),
        "{why}"
    );
}

#[tokio::test]
async fn nothing_is_loaded_under_once_and_nothing_counted() {
    let (load, _driver, _dir) = tool(true);
    let why = refusal(&load, json!({ "package": "lint-mcp", "version": "1.2.0" })).await;
    assert!(why.contains("not under --once"), "{why}");
    assert_eq!(locked(&load.state).attempts, 0);
}

#[tokio::test]
async fn a_second_load_at_once_and_one_past_max_loads_are_refused() {
    let (load, _driver, _dir) = tool(false);
    locked(&load.state).busy = true;
    let why = refusal(&load, json!({ "package": "lint-mcp", "version": "1.2.0" })).await;
    assert!(why.contains("another load is in progress"), "{why}");
    locked(&load.state).busy = false;
    // One kept, stopped or not, is as many as `max_loads = 1` allows.
    stop(
        &load.loaded,
        &add(&load.loaded, "other"),
        "MCP server 'x' exited",
    )
    .await;
    let why = refusal(&load, json!({ "package": "lint-mcp", "version": "1.2.0" })).await;
    assert!(
        why.contains("this session has loaded 1 plugins, as many as it may ([extend] max_loads 1)"),
        "{why}"
    );
    assert_eq!(locked(&load.state).attempts, 1, "counted before max_loads");
}

#[tokio::test]
async fn a_package_loaded_or_stopped_is_not_loaded_again_and_its_name_is_taken() {
    let (mut load, _driver, _dir) = tool(false);
    load.extend.max_loads = 5;
    let mut lint = kept("lint", &[]);
    lint.package = "lint-mcp".into();
    let exit = Exit::default();
    load.loaded.add(lint, server("lint", &exit));
    let why = refusal(
        &load,
        json!({ "package": "lint-mcp", "version": "1.2.0", "name": "other" }),
    )
    .await;
    assert!(
        why.contains("lint-mcp is loaded already this session, as lint"),
        "{why}"
    );
    // A loaded server's name is in use, as a manifest entry's is.
    let why = refusal(
        &load,
        json!({ "package": "taken-mcp", "version": "1.0.0", "name": "lint" }),
    )
    .await;
    assert!(
        why.contains("the name \"lint\" is in use by another plugin or server"),
        "{why}"
    );
    // Once it stopped: said so, not "loaded already".
    stop(&load.loaded, &exit, "MCP server 'x' exited").await;
    let why = refusal(&load, json!({ "package": "lint-mcp", "version": "1.2.0" })).await;
    assert!(
        why.contains("lint-mcp was loaded this session and stopped (it exited): it is not loaded again this session"),
        "{why}"
    );
}

#[test]
fn a_name_may_not_end_in_an_underscore() {
    assert!(name_problem("a_").unwrap().contains("ends in `_`"));
    assert_eq!(name_problem("_a"), None);
}

/// A DSH package's start, as `load_plugin` makes it (a fixture file for
/// the package's row: the stub registry has none of the DSH base's
/// packages): a restricted runtime of its own on the DSH base, the base's
/// rows, the package's and the adapter's, its handler registered tools
/// only; one waiting for DSH's host services refused, naming them. Needs
/// the checkout's `adapters/dsh` (required with
/// `YOAGENT_RUTIS_REQUIRE_RUNTIMES=1`).
#[tokio::test(flavor = "multi_thread")]
async fn a_dsh_package_starts_restricted_on_the_base_and_one_lacking_services_is_refused() {
    let dsh = Path::new(env!("CARGO_MANIFEST_DIR")).join("adapters/dsh");
    if !dsh.join("node_modules").is_dir() {
        assert!(
            std::env::var_os("YOAGENT_RUTIS_REQUIRE_RUNTIMES").is_none(),
            "adapters/dsh/node_modules is missing"
        );
        return;
    }
    let root = rutis::Ctx::root().unwrap();
    let bridge = yoagent_rutis::RutisBridge::install(&root).unwrap();
    let host = crate::frontend::host::PluginHost::builder()
        .share("yoagent")
        .start(&root)
        .await
        .unwrap();
    let on = Plugins {
        host: Arc::new(tokio::sync::Mutex::new(host)),
        bridge: bridge.clone(),
        adapters: dsh.parent().unwrap().to_path_buf(),
    };
    let (load, _driver, _dir) = tool(false);
    let start = |runtime: &'static str, name: &'static str, fixture: &str| {
        let file = dsh.join(fixture).display().to_string();
        let row =
            crate::frontend::host::Row::new(format!("loaded:{name}:target"), &file).runtime("dsh");
        let handler = format!("loaded-{name}-1");
        let (load, on, dsh) = (load.clone(), on.clone(), dsh.clone());
        async move {
            let _loading = load.loaded.loading(&handler, name);
            let mut remove = package::RemoveOnDrop::new(on.host.clone(), load.session.clone());
            let found = package::Found::Dsh {
                rows: vec![row],
                modules: vec![file],
            };
            let started = load
                .start_package(
                    &on,
                    runtime,
                    &handler,
                    &format!("loaded:{name}"),
                    &dsh,
                    found,
                    name,
                    &mut remove,
                )
                .await;
            (started, remove, handler)
        }
    };
    let (started, remove, handler) = start("loaded-1", "echo", "fixture-tools.ts").await;
    assert!(started.is_ok(), "{}", started.err().unwrap().message);
    let info = bridge
        .registry()
        .handlers()
        .into_iter()
        .find(|h| h.name() == handler)
        .expect("its adapter's handler");
    assert!(info.restricted());
    assert!(
        !info.hooks().contains(&"before_model"),
        "{:?}",
        info.hooks()
    );
    assert_eq!(info.owner(), "loaded:echo");
    remove.now().await;
    // Removed: its handler gone with its runtime.
    assert!(
        !bridge
            .registry()
            .handlers()
            .iter()
            .any(|h| h.name() == handler),
        "its handler outlived its runtime"
    );

    let (started, _remove, handler) = start("loaded-2", "needy", "fixture-needs-services.ts").await;
    let refusal = started.expect_err("refused");
    assert!(matches!(refusal.level, NoticeLevel::Error));
    // The runtime removed before the refusal returned.
    assert!(!bridge
        .registry()
        .handlers()
        .iter()
        .any(|h| h.name() == handler));
    let message = refusal.message;
    assert!(
        message.starts_with("needy did not start: it waits for services nobody provides here (")
            && message.contains("credentials")
            && message.ends_with("a loaded DSH package gets none of DSH's host services"),
        "{message}"
    );
    root.shutdown().await.unwrap();
}

/// A name a package load started a runtime under is not given again this
/// session (the late source keeps a run's tools by name): counted.
#[tokio::test]
async fn a_name_an_earlier_package_load_used_is_not_given_again() {
    let (load, _driver, _dir) = tool(false);
    locked(&load.state).tried.insert("lint".into());
    let why = refusal(&load, json!({ "package": "lint-mcp" })).await;
    assert!(
        why.contains(
            "the name \"lint\" was used by an earlier load this session: pass another `name`"
        ),
        "{why}"
    );
    assert_eq!(locked(&load.state).attempts, 1);
}

/// A stopped package's tools are refused without asking, naming why; a
/// running one's and a stopped server's are not (a server's leave at the
/// next turn). The naming the bridge keeps holds `Loaded` weakly.
#[test]
fn a_stopped_packages_tools_are_refused_and_the_naming_holds_loaded_weakly() {
    let loaded = Loaded::default();
    let package = |kind, name: &str, stopped: Option<&str>| Kept {
        kind,
        name: name.into(),
        package: format!("{name}-pkg"),
        version: "1.0.0".into(),
        args: Vec::new(),
        tools: vec!["shout".into()],
        stopped: stopped.map(str::to_owned),
    };
    locked(&loaded.0.lists).kept.extend([
        package(ExtendKind::Pi, "loud", Some("its runtime stopped: exit 7")),
        package(ExtendKind::Dsh, "calm", None),
        package(ExtendKind::Mcp, "srv", Some("it exited")),
    ]);
    assert_eq!(
        loaded.refused("loud__shout").as_deref(),
        Some("`loud__shout` is a tool of the pi package `loud`, which stopped this session (its runtime stopped: exit 7): it is not run")
    );
    assert_eq!(loaded.refused("loud__other"), None);
    assert_eq!(loaded.refused("calm__shout"), None);
    assert_eq!(loaded.refused("srv__shout"), None);

    // A load in progress names its handler's tools; a kept running one
    // only those the owner saw; anything else nothing.
    let naming = loaded.naming();
    let guard = loaded.loading("h-1", "fresh");
    assert_eq!(naming("h-1", "any").as_deref(), Some("fresh__any"));
    drop(guard);
    assert_eq!(naming("h-1", "any"), None);
    locked(&loaded.0.lists).packages.push(Running {
        name: "calm".into(),
        runtime: "loaded-2".into(),
        handler: "h-2".into(),
        tools: vec!["shout".into()],
    });
    assert_eq!(naming("h-2", "shout").as_deref(), Some("calm__shout"));
    assert_eq!(naming("h-2", "later"), None);
    drop(loaded);
    assert_eq!(
        naming("h-2", "shout"),
        None,
        "Loaded kept alive by its naming"
    );
}
