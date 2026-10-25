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
    }
}

#[test]
fn the_end_says_each_server_and_the_block_that_keeps_it() {
    assert_eq!(
        kept("eslint", &[]).block(),
        "[[mcp]]\nname = \"eslint\"\nnpm  = \"@eslint/mcp@0.3.15\"\n# allow = [\"lint-files\"]   # tools that would run without asking\n"
    );
    let text = kept_text(&[kept("eslint", &["--dir", "a \"b\""])]);
    assert!(
        text.starts_with(
            "Loaded this session: @eslint/mcp@0.3.15 as eslint (tools: lint-files). To keep it, add to agent.toml:\n\n[[mcp]]\n"
        ),
        "{text}"
    );
    assert!(
        text.contains("args = [\"--dir\", \"a \\\"b\\\"\"]\n"),
        "{text}"
    );
    assert!(text.ends_with("which --locked needs."), "{text}");
    let both = kept_text(&[kept("a", &[]), kept("b", &[])]);
    assert!(both.contains("as a (tools: lint-files); @eslint/mcp@0.3.15 as b"));
    assert!(both.contains("To keep them, add"));
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
        kept: kept(name, &[]),
        tools: vec![tool],
        client,
        closed: Arc::new(Mutex::new(closed.map(String::from))),
    }
}

#[tokio::test]
async fn loaded_servers_are_a_tool_source_that_moves_on_each_change() {
    let loaded = Loaded::default();
    assert_eq!(loaded.generation(), 0);
    assert!(loaded.tools().await.is_empty());
    loaded.add(server("eslint", None));
    loaded.add(server("gone", Some("exited with 1")));
    assert_eq!(loaded.generation(), 2);
    let names: Vec<String> = loaded
        .tools()
        .await
        .iter()
        .map(|t| t.name().to_owned())
        .collect();
    assert_eq!(names, ["eslint__lint-files", "gone__lint-files"]);
    // Asked by membership: a loaded tool, not a name that merely looks so.
    assert!(loaded.asks("eslint__lint-files"));
    assert!(!loaded.asks("eslint__other"));
    assert!(!loaded.asks("lint-files"));
    // The one found gone leaves, said once; it is still asked and kept.
    assert_eq!(
        loaded.stopped(),
        [("gone".to_owned(), "exited with 1".to_owned())]
    );
    assert_eq!(loaded.generation(), 3);
    assert!(loaded.stopped().is_empty());
    assert_eq!(loaded.generation(), 3, "no change, no move");
    assert_eq!(loaded.tools().await.len(), 1);
    assert!(loaded.asks("gone__lint-files"));
    assert_eq!(loaded.kept().len(), 2);
    loaded.close().await;
    assert!(loaded.tools().await.is_empty());
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
    assert_eq!(
        gone(&Err::<(), _>(McpError::Transport("Write error: x".into()))),
        None
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
            "young is not loadable: no version was published 7 or more days ago (min_age_days is 7)",
        ),
        (
            json!({ "package": "lint-mcp", "version": "1.3.0" }),
            "lint-mcp@1.3.0 is not the version find_plugin offered: lint-mcp@1.2.0",
        ),
    ];
    for (args, said) in cases {
        let why = refusal(&load, args.clone()).await;
        assert!(why.contains(said), "{args}: {why}");
    }
    assert_eq!(locked(&load.state).attempts, 0, "none of these counted");
    // Counted from here: a name in use, then the attempts spent.
    let why = refusal(&load, json!({ "package": "taken-mcp", "version": "1.0.0" })).await;
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
    load.loaded.add(server("other", Some("exited")));
    load.loaded.stopped();
    let why = refusal(&load, json!({ "package": "lint-mcp", "version": "1.2.0" })).await;
    assert!(
        why.contains("this session has loaded 1 servers, as many as it may ([extend] max_loads 1)"),
        "{why}"
    );
    assert_eq!(locked(&load.state).attempts, 1, "counted before max_loads");
}
