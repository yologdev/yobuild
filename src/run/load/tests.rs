use std::path::Path;

use serde_json::json;
use yoagent::mcp::McpToolInfo;
use yoagent::{AgentTool, ToolContext, ToolError};

use super::*;
use crate::builtins::{Candidate, FindPlugin, Sources};
use crate::frontend::{Driver, Session};
use crate::manifest::Manifest;

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

#[test]
fn the_listed_tools_are_judged_as_a_manifests_servers_are() {
    assert_eq!(
        tools_problem("eslint", &[info("lint-files"), info("fix")]),
        Ok(vec!["lint-files".to_owned(), "fix".to_owned()])
    );
    assert_eq!(
        tools_problem("eslint", &[]),
        Err("it lists no tools".to_owned())
    );
    let long = "t".repeat(60);
    assert!(tools_problem("eslint", &[info(&long)])
        .unwrap_err()
        .contains("load it under a shorter `name`"));
    assert!(tools_problem("eslint", &[info("read file")])
        .unwrap_err()
        .contains("cannot be offered to a model"));
    assert!(tools_problem("eslint", &[info("a"), info("a")])
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

/// A transport whose server is gone.
struct Gone;

#[async_trait::async_trait]
impl McpTransport for Gone {
    async fn send(&self, _: JsonRpcRequest) -> Result<JsonRpcResponse, McpError> {
        Err(McpError::ConnectionClosed)
    }
    async fn close(&self) -> Result<(), McpError> {
        Ok(())
    }
}

fn server(name: &str, closed: Option<&str>) -> Server {
    let client = Arc::new(tokio::sync::Mutex::new(McpClient::from_transport(
        Box::new(Gone),
    )));
    let tool: Arc<dyn AgentTool> =
        Arc::new(McpToolAdapter::new(client.clone(), info("lint-files")).with_prefix(name));
    Server {
        name: name.into(),
        tools: vec![tool],
        client,
        closed: Arc::new(Mutex::new(closed.map(String::from))),
    }
}

fn add(loaded: &Loaded, name: &str, closed: Option<&str>) {
    loaded.add(kept(name, &[]), server(name, closed));
}

#[tokio::test]
async fn loaded_servers_are_a_tool_source_that_moves_on_each_change() {
    let loaded = Loaded::default();
    assert_eq!(loaded.generation(), 0);
    assert!(loaded.tools().await.is_empty());
    add(&loaded, "eslint", None);
    add(&loaded, "gone", Some("exited with 1"));
    assert_eq!(loaded.generation(), 2);
    let names: Vec<String> = loaded
        .tools()
        .await
        .iter()
        .map(|t| t.name().to_owned())
        .collect();
    // One found gone is offered no more, before anyone looks.
    assert_eq!(names, ["eslint__lint-files"]);
    // Asked by membership: a loaded tool, not a name that merely looks so.
    assert!(loaded.asks("eslint__lint-files"));
    assert!(!loaded.asks("eslint__other"));
    assert!(!loaded.asks("lint-files"));
    // The one found gone is said once, then kept marked stopped; still
    // asked.
    assert_eq!(
        loaded.stopped(),
        [("gone".to_owned(), "exited with 1".to_owned())]
    );
    assert!(loaded.stopped().is_empty());
    assert_eq!(loaded.generation(), 2, "the list did not change again");
    assert_eq!(loaded.tools().await.len(), 1);
    assert!(loaded.asks("gone__lint-files"));
    let kept = loaded.kept();
    assert_eq!(kept.len(), 2);
    assert_eq!(kept[1].stopped.as_deref(), Some("exited with 1"));
    assert_eq!(kept[0].stopped, None);
    loaded.close().await;
    assert!(loaded.tools().await.is_empty());
    assert_eq!(loaded.generation(), 3, "closing moves it");
}

/// A call through the transport that finds the server gone moves the
/// generation then, once: its tools leave at the next turn whoever looks.
#[tokio::test]
async fn the_transport_moves_the_generation_when_it_finds_the_server_gone() {
    let generation = Arc::new(AtomicU64::new(5));
    let closed: Arc<Mutex<Option<String>>> = Arc::default();
    let see = |out: Result<(), McpError>| {
        let why = gone(&out);
        let mut c = locked(&closed);
        if let (Some(why), true) = (why, c.is_none()) {
            *c = Some(why);
            generation.fetch_add(1, Ordering::SeqCst);
        }
    };
    see(Err(McpError::Protocol("odd".into())));
    assert_eq!(generation.load(Ordering::SeqCst), 5);
    see(Err(McpError::Transport(
        "Write error: Broken pipe (os error 32)".into(),
    )));
    see(Err(McpError::ConnectionClosed));
    assert_eq!(generation.load(Ordering::SeqCst), 6, "once");
    assert!(locked(&closed)
        .as_deref()
        .unwrap()
        .contains("it had exited"));
}

#[test]
fn a_call_that_finds_the_server_gone_says_why() {
    assert_eq!(gone(&Err::<(), _>(McpError::Protocol("odd".into()))), None);
    assert_eq!(gone(&Ok::<(), McpError>(())), None);
    let why = gone(&Err::<(), _>(McpError::Transport(
        "Connection closed: MCP server 'x' exited with exit status: 1; last stderr:\nboom".into(),
    )))
    .unwrap();
    assert_eq!(why, "it exited with exit status: 1; last stderr: boom");
    assert_eq!(
        gone(&Err::<(), _>(McpError::ConnectionClosed)).as_deref(),
        Some("its connection closed")
    );
    // Dead while idle: the call's write fails before anything is read.
    assert_eq!(
        gone(&Err::<(), _>(McpError::Transport(
            "Write error: Broken pipe (os error 32)".into()
        )))
        .as_deref(),
        Some(
            "it no longer reads its input: it had exited (Write error: Broken pipe (os error 32))"
        )
    );
    assert!(gone(&Err::<(), _>(McpError::Transport("Flush error: x".into()))).is_some());
    assert_eq!(
        gone(&Err::<(), _>(McpError::Transport("Read error: x".into()))),
        None
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
    find.remember(&[
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
    add(&load.loaded, "other", Some("exited"));
    load.loaded.stopped();
    let why = refusal(&load, json!({ "package": "lint-mcp", "version": "1.2.0" })).await;
    assert!(
        why.contains("this session has loaded 1 servers, as many as it may ([extend] max_loads 1)"),
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
    load.loaded.add(lint, server("lint", None));
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
    *locked(&locked(&load.loaded.0.lists).servers[0].closed) = Some("it exited".into());
    load.loaded.stopped();
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
