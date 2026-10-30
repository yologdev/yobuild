use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value as Json};
use yoagent::tool_source::ToolSource;
use yoagent::{AgentTool, Content, ToolContext, ToolError, ToolResult};

use super::super::{Files, LoadPlugin, Loaded, SETTLE};
use super::{credential_problem, default_name};
use crate::builtins::find_plugin::{Remote, RemoteHeader, NO_VERSION};
use crate::builtins::{Candidate, FindPlugin, Sources};
use crate::frontend::{ClientMessage, Driver, NoticeLevel, ServerMessage, Session, UiRequest};
use crate::manifest::{ExtendKind, Manifest};

/// The credential the tests name; set for these tests only.
const KEY: &str = "YB_REMOTE_TEST_KEY";
const SECRET: &str = "sk-remote-test-123456";

#[test]
fn a_remote_is_named_after_its_registry_name() {
    for (registry, name) in [
        ("io.github.upstash/context7", "context7"),
        ("com.notion/mcp", "notion"),
        ("app.linear/linear", "linear"),
        ("io.x/docs-mcp-server", "docs"),
        ("io.x/server", "x"),
        ("io.x/my.docs", "my-docs"),
    ] {
        assert_eq!(default_name(registry), name, "{registry}");
    }
}

#[test]
fn a_credential_is_a_plain_name_set_here_and_not_one_yoagent_reads() {
    let keys = ["DEEPSEEK_API_KEY".to_owned()];
    let set = |n: &str| n == "MINE" || n == "DEEPSEEK_API_KEY";
    assert_eq!(credential_problem("MINE", &keys, &set), None);
    assert_eq!(
        credential_problem("DEEPSEEK_API_KEY", &keys, &set).as_deref(),
        Some("`DEEPSEEK_API_KEY` is a credential yoagent reads: hand it over in agent.toml if you mean to")
    );
    assert_eq!(
        credential_problem("UNSET", &keys, &set).as_deref(),
        Some("`UNSET` is not set here, or is empty")
    );
    assert_eq!(
        credential_problem("sk-live-1234", &keys, &set).as_deref(),
        Some("`sk-live-1234` is not a variable's name (letters, digits and `_`)")
    );
}

/// How the stub server answers.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Ok,
    /// 401 at `initialize`.
    Unauthorized,
    /// A 307 to another path of its own: followed by nothing.
    Redirect,
    /// Holds a session, then answers a tool call 404: the session expired.
    Expire,
}

/// An MCP server over streamable HTTP on this machine, recording each
/// request (`<method> <JSON-RPC method> auth=<authorization>`).
async fn stub(mode: Mode) -> (String, Arc<Mutex<Vec<String>>>) {
    use axum::http::{HeaderMap, Method, StatusCode};
    use axum::response::IntoResponse;
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let record = seen.clone();
    let elsewhere = seen.clone();
    let app = axum::Router::new()
        .route(
            "/mcp",
            axum::routing::any(move |method: Method, headers: HeaderMap, body: String| {
                let record = record.clone();
                async move {
                    let request: Json = serde_json::from_str(&body).unwrap_or(Json::Null);
                    let auth = headers
                        .get("authorization")
                        .and_then(|a| a.to_str().ok())
                        .unwrap_or("-");
                    let what = request["method"].as_str().unwrap_or("-").to_owned();
                    record
                        .lock()
                        .unwrap()
                        .push(format!("{method} {what} auth={auth}"));
                    if method == Method::DELETE {
                        return StatusCode::OK.into_response();
                    }
                    match (mode, what.as_str()) {
                        (Mode::Unauthorized, _) => {
                            return (StatusCode::UNAUTHORIZED, "sign in first").into_response()
                        }
                        (Mode::Redirect, _) => {
                            return (StatusCode::TEMPORARY_REDIRECT, [("location", "/elsewhere")])
                                .into_response()
                        }
                        (Mode::Expire, "tools/call") => {
                            return StatusCode::NOT_FOUND.into_response()
                        }
                        _ => {}
                    }
                    let Some(id) = request.get("id").cloned() else {
                        return StatusCode::ACCEPTED.into_response();
                    };
                    let result = match what.as_str() {
                        "initialize" => json!({
                            "protocolVersion": "2025-03-26",
                            "capabilities": { "tools": {} },
                            "serverInfo": { "name": "docs", "version": "1" }
                        }),
                        "tools/list" => json!({ "tools": [{
                            "name": "echo", "description": "Echo",
                            "inputSchema": { "type": "object", "properties": { "text": { "type": "string" } } }
                        }]}),
                        _ => json!({ "content": [{ "type": "text",
                            "text": format!("echoed {}", request["params"]["arguments"]["text"].as_str().unwrap_or("")) }] }),
                    };
                    (
                        [
                            ("content-type", "application/json"),
                            ("mcp-session-id", "s-1"),
                        ],
                        json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string(),
                    )
                        .into_response()
                }
            }),
        )
        .route(
            "/elsewhere",
            axum::routing::any(move || {
                let elsewhere = elsewhere.clone();
                async move {
                    elsewhere.lock().unwrap().push("ELSEWHERE".into());
                    StatusCode::OK
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    (url, seen)
}

fn authorization(required: bool) -> RemoteHeader {
    RemoteHeader {
        name: "Authorization".into(),
        required,
        secret: true,
        description: "Your API key".into(),
        template: Some("Bearer {key}".into()),
    }
}

/// What a frontend saw: each question (title, message) and each notice.
#[derive(Default)]
struct Seen {
    questions: Vec<(String, String)>,
    notices: Vec<(NoticeLevel, String)>,
}

/// A frontend answering each question in turn with `answers`.
fn answering(session: &Session, answers: Vec<Json>) -> Arc<Mutex<Seen>> {
    let seen: Arc<Mutex<Seen>> = Arc::default();
    let mut connection = session.connect();
    let (session, record) = (session.clone(), seen.clone());
    tokio::spawn(async move {
        let mut answers = answers.into_iter();
        while let Some(message) = connection.messages.recv().await {
            match message {
                ServerMessage::UiRequest { id, request } => {
                    let (title, message) = match &request {
                        UiRequest::Select { title, message, .. }
                        | UiRequest::Input { title, message, .. } => {
                            (title.clone(), message.clone())
                        }
                        other => (format!("{other:?}"), String::new()),
                    };
                    record.lock().unwrap().questions.push((title, message));
                    let value = answers.next().unwrap_or(Json::Null);
                    session.send(ClientMessage::UiResponse { id, value });
                }
                ServerMessage::Notice { level, message } => {
                    record.lock().unwrap().notices.push((level, message))
                }
                _ => {}
            }
        }
    });
    seen
}

struct Loading {
    load: LoadPlugin,
    loaded: Loaded,
    seen: Arc<Mutex<Seen>>,
    _driver: Driver,
    _dir: tempfile::TempDir,
}

fn manifest(dir: &Path) -> Manifest {
    let text = "[agent]\nname = \"t\"\nmodel = \"deepseek/deepseek-flash\"\n[extend]\nkinds = [\"remote\"]\n";
    Manifest::parse(text, &dir.join("agent.toml"), dir).unwrap()
}

/// `load_plugin` with one remote candidate, `io.x/docs` at `url`, and a
/// frontend answering `answers`.
fn loading(url: &str, headers: Vec<RemoteHeader>, answers: Vec<Json>) -> Loading {
    std::env::set_var(KEY, SECRET);
    let dir = tempfile::tempdir().unwrap();
    let manifest = manifest(dir.path());
    let (session, driver) = Session::new(false);
    let extend = manifest.extend.clone().unwrap();
    let find = FindPlugin::new(
        session.clone(),
        extend.clone(),
        Sources::at("http://127.0.0.1:9").unwrap(),
    );
    find.remember(&[Candidate {
        need: "library docs".into(),
        kind: ExtendKind::Remote,
        package: "io.x/docs".into(),
        version: Err(NO_VERSION.into()),
        registry_name: Some("io.x/docs".into()),
        from_npm: false,
        description: "Library docs".into(),
        licence: None,
        repository: None,
        published: Some("2026-09-30".into()),
        downloads: Err(String::new()),
        provenance: None,
        required_env: None,
        required_args: None,
        remote: Some(Remote::unchecked(url, headers)),
    }]);
    let seen = answering(&session, answers);
    let loaded = Loaded::default();
    let load = LoadPlugin::new(
        session,
        Files {
            manifest: "agent.toml".into(),
            lock: "agent.lock".into(),
            shared: Vec::new(),
        },
        &manifest,
        extend,
        find,
        loaded.clone(),
        false,
        None,
        dir.path(),
        None,
    );
    Loading {
        load,
        loaded,
        seen,
        _driver: driver,
        _dir: dir,
    }
}

impl Loading {
    async fn call(&self) -> Result<ToolResult, ToolError> {
        self.load
            .execute(
                json!({ "package": "io.x/docs" }),
                ToolContext::new("1", LoadPlugin::TOOL),
            )
            .await
    }

    fn questions(&self) -> Vec<(String, String)> {
        self.seen.lock().unwrap().questions.clone()
    }

    /// The notices, once the last has had a moment to arrive.
    async fn notices(&self) -> Vec<(NoticeLevel, String)> {
        tokio::time::sleep(Duration::from_millis(100)).await;
        self.seen.lock().unwrap().notices.clone()
    }
}

fn text(result: &Result<ToolResult, ToolError>) -> String {
    match result {
        Ok(r) => r
            .content
            .iter()
            .map(|c| match c {
                Content::Text { text } => text.clone(),
                _ => String::new(),
            })
            .collect(),
        Err(ToolError::Failed(why)) => why.clone(),
        Err(other) => format!("{other:?}"),
    }
}

#[tokio::test]
async fn a_remote_is_connected_with_the_credential_the_owner_names() {
    let (url, seen) = stub(Mode::Ok).await;
    let l = loading(
        &url,
        vec![authorization(true)],
        vec![json!("Allow"), json!(KEY), json!("Allow")],
    );
    let result = l.call().await;
    let said = text(&result);
    assert!(result.is_ok(), "{said}");
    assert!(
        said.starts_with("Connected to io.x/docs at 127.0.0.1 as docs, for this session. Its tools are yours from your next step: docs__echo."),
        "{said}"
    );
    let questions = l.questions();
    let titles: Vec<&str> = questions.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(
        titles,
        [
            "Connect to the remote MCP server io.x/docs at 127.0.0.1 as docs? (asked by the agent)",
            "Which environment variable holds the `Authorization` header for docs? (sent to 127.0.0.1 with every request)",
            "Keep the remote MCP server docs? It offers: echo",
        ]
    );
    let first = &questions[0].1;
    assert!(
        first.contains("Header `Authorization` (required, secret: you will be asked which variable holds it) — Your API key")
            && first.contains("Nothing of it runs here. Each call of its tools sends its arguments to 127.0.0.1"),
        "{first}"
    );
    let credential = &questions[1].1;
    assert!(
        credential.contains("Its value: Bearer {key}")
            && credential.ends_with("Without it, it is not connected."),
        "{credential}"
    );
    // Sent with each request; said nowhere.
    let requests = seen.lock().unwrap().clone();
    assert!(
        requests
            .iter()
            .any(|r| r == &format!("POST initialize auth=Bearer {SECRET}")),
        "{requests:?}"
    );
    let notices = l.notices().await;
    let info = notices
        .iter()
        .find(|(level, m)| *level == NoticeLevel::Info && m.starts_with("The agent connected"))
        .map(|(_, m)| m.clone())
        .expect("the connection said");
    assert!(
        info.contains("[[mcp]]\nname = \"docs\"\nurl  = \"")
            && info.contains(&format!(
                "headers = {{ Authorization = \"Bearer ${{{KEY}}}\" }}\n# allow = [\"echo\"]"
            )),
        "{info}"
    );
    for text in notices.iter().map(|(_, m)| m).chain([&said]) {
        assert!(!text.contains(SECRET), "{text}");
    }
    // Its tool, asked of the server; the block read back.
    let tools = l.loaded.tools().await;
    let [echo] = tools.as_slice() else {
        panic!("{}", tools.len())
    };
    assert_eq!(echo.name(), "docs__echo");
    let echoed = echo
        .execute(json!({ "text": "hi" }), ToolContext::new("2", "docs__echo"))
        .await;
    assert_eq!(text(&echoed), "echoed hi");
    let kept = l.loaded.kept();
    let block = kept[0].block();
    let manifest = format!(
        "[agent]\nname = \"t\"\nmodel = \"deepseek/deepseek-flash\"\n{}",
        block.replace(&url, "https://docs.example.com/mcp")
    );
    let dir = tempfile::tempdir().unwrap();
    let read = Manifest::parse(&manifest, &dir.path().join("a.toml"), dir.path()).unwrap();
    assert_eq!(read.mcp[0].name, "docs");
    // Closed at the end: its session ended with a DELETE.
    l.loaded.close().await;
    assert!(
        seen.lock().unwrap().iter().any(|r| r.starts_with("DELETE")),
        "{:?}",
        seen.lock().unwrap()
    );
}

#[tokio::test]
async fn a_credential_is_asked_again_when_it_cannot_be_taken() {
    let (url, _) = stub(Mode::Ok).await;
    let l = loading(
        &url,
        vec![authorization(true)],
        vec![
            json!("Allow"),
            json!("DEEPSEEK_API_KEY"),
            json!("YB_REMOTE_TEST_NOT_SET"),
            json!(KEY),
            json!("Allow"),
        ],
    );
    let result = l.call().await;
    assert!(result.is_ok(), "{}", text(&result));
    let questions = l.questions();
    assert_eq!(questions.len(), 5);
    assert!(
        questions[2].1.starts_with("Not taken: `DEEPSEEK_API_KEY` is a credential yoagent reads: hand it over in agent.toml if you mean to. Name another."),
        "{}",
        questions[2].1
    );
    assert!(
        questions[3]
            .1
            .starts_with("Not taken: `YB_REMOTE_TEST_NOT_SET` is not set here, or is empty."),
        "{}",
        questions[3].1
    );
}

#[tokio::test]
async fn a_required_credential_left_empty_connects_nothing_and_is_not_remembered() {
    let (url, seen) = stub(Mode::Ok).await;
    let l = loading(
        &url,
        vec![authorization(true)],
        vec![json!("Allow"), json!("")],
    );
    let said = text(&l.call().await);
    assert!(
        said.contains("not connected: you named no variable for its required header `Authorization` (it may be proposed again)"),
        "{said}"
    );
    assert!(seen.lock().unwrap().is_empty(), "nothing connected");
    assert!(super::super::locked(&l.load.state).blocked.is_empty());
}

#[tokio::test]
async fn an_optional_credential_left_empty_is_not_sent() {
    let (url, seen) = stub(Mode::Ok).await;
    let l = loading(
        &url,
        vec![authorization(false)],
        vec![json!("Allow"), json!(""), json!("Allow")],
    );
    let result = l.call().await;
    assert!(result.is_ok(), "{}", text(&result));
    assert!(
        seen.lock().unwrap().iter().all(|r| r.ends_with("auth=-")),
        "{:?}",
        seen.lock().unwrap()
    );
    let block = l.loaded.kept()[0].block();
    assert!(!block.contains("headers"), "{block}");
}

#[tokio::test]
async fn a_remote_that_wants_a_sign_in_is_said_so() {
    let (url, _) = stub(Mode::Unauthorized).await;
    let l = loading(
        &url,
        vec![authorization(true)],
        vec![json!("Allow"), json!(KEY)],
    );
    let said = text(&l.call().await);
    assert!(
        said.contains("io.x/docs refused yobuild's request (HTTP 401): it needs a credential, or a sign-in (OAuth) yobuild cannot do yet"),
        "{said}"
    );
    assert!(!said.contains(SECRET), "{said}");
    let notices = l.notices().await;
    assert!(
        notices.iter().any(|(level, m)| *level == NoticeLevel::Error
            && m.starts_with("The agent's plugin `io.x/docs` was refused: io.x/docs refused")),
        "{notices:?}"
    );
}

#[tokio::test]
async fn a_redirect_is_not_followed() {
    let (url, seen) = stub(Mode::Redirect).await;
    let l = loading(&url, Vec::new(), vec![json!("Allow")]);
    let said = text(&l.call().await);
    assert!(said.contains("io.x/docs did not connect: "), "{said}");
    assert!(said.contains("redirect"), "{said}");
    assert!(
        !seen.lock().unwrap().iter().any(|r| r == "ELSEWHERE"),
        "the redirect was followed"
    );
}

#[tokio::test]
async fn a_remote_that_ends_its_session_is_said_stopped() {
    let (url, _) = stub(Mode::Expire).await;
    let l = loading(&url, Vec::new(), vec![json!("Allow"), json!("Allow")]);
    let result = l.call().await;
    assert!(result.is_ok(), "{}", text(&result));
    let tools = l.loaded.tools().await;
    let failed = tools[0]
        .execute(json!({ "text": "hi" }), ToolContext::new("2", "docs__echo"))
        .await;
    assert!(
        text(&failed).contains("MCP session expired"),
        "{}",
        text(&failed)
    );
    // Its tools leave at once; said stopped once settled.
    assert!(l.loaded.tools().await.is_empty());
    tokio::time::sleep(SETTLE + Duration::from_millis(100)).await;
    assert_eq!(
        l.loaded.stopped(),
        [(
            "docs".to_owned(),
            "the server ended its session (HTTP 404): it is not reconnected".to_owned()
        )]
    );
    assert!(
        l.loaded.asks("docs__echo"),
        "its tools still ask, then are not found"
    );
}
