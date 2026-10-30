use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value as Json};
use yoagent::tool_source::ToolSource;
use yoagent::{AgentTool, Content, ToolContext, ToolError, ToolResult};

use super::super::{kept_text, locked, Files, Kept, KeptRemote, LoadPlugin, Loaded, SETTLE};
use super::{credential, default_name, Lookup};
use crate::builtins::find_plugin::{Remote, RemoteHeader, NO_VERSION};
use crate::builtins::{Candidate, FindPlugin, Sources};
use crate::frontend::{
    ClientMessage, Driver, NoticeLevel, ResolveReason, ServerMessage, Session, UiRequest,
};
use crate::manifest::{ExtendKind, Manifest};

/// The credential the tests name, in their own environment (never this
/// process's).
const KEY: &str = "YB_REMOTE_TEST_KEY";
const SECRET: &str = "sk-remote-test-123456";

/// An environment holding only `vars`.
fn lookup(vars: &[(&str, OsString)]) -> Lookup {
    let vars: HashMap<String, OsString> = vars
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    Arc::new(move |name: &str| vars.get(name).cloned())
}

fn with_key() -> Lookup {
    lookup(&[(KEY, SECRET.into())])
}

#[test]
fn a_remote_is_named_after_its_registry_name() {
    for (registry, name) in [
        ("io.github.upstash/context7", "context7"),
        ("com.notion/mcp", "notion"),
        ("app.linear/linear", "linear"),
        ("io.x/docs-mcp-server", "docs"),
        ("io.x/server", "x"),
        ("io.x/mcp-server", "x"),
        ("io.x/my.docs", "my-docs"),
    ] {
        assert_eq!(default_name(registry), name, "{registry}");
    }
}

#[test]
fn a_credential_is_a_plain_name_set_here_and_not_one_yoagent_reads() {
    let keys = ["DEEPSEEK_API_KEY".to_owned()];
    let vars = lookup(&[
        ("MINE", "v4lue".into()),
        ("DEEPSEEK_API_KEY", "sk-d".into()),
        ("EMPTY", "".into()),
    ]);
    assert_eq!(credential("MINE", &keys, &*vars), Ok("v4lue".into()));
    assert_eq!(
        credential("DEEPSEEK_API_KEY", &keys, &*vars),
        Err("`DEEPSEEK_API_KEY` is a credential yoagent reads: hand it over in agent.toml if you mean to".into())
    );
    // A pasted key — of a name's shape or not — is never repeated.
    for answer in ["UNSET", "EMPTY", "ghp_AbC123secret"] {
        assert_eq!(
            credential(answer, &keys, &*vars),
            Err("no variable of that name is set here, or it is empty".into()),
            "{answer}"
        );
    }
    assert_eq!(
        credential("sk-live-1234", &keys, &*vars),
        Err("that is not a variable's name (letters, digits and `_`): name the variable that holds the key, never the key".into())
    );
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let odd = lookup(&[("ODD", OsString::from_vec(vec![0xff, 0xfe]))]);
        assert_eq!(
            credential("ODD", &keys, &*odd),
            Err("that variable's value is not text (UTF-8)".into())
        );
    }
}

#[test]
fn the_end_says_a_remote_by_its_address_and_its_headers_variables() {
    let mut kept = Kept {
        kind: ExtendKind::Remote,
        name: "docs".into(),
        package: "io.x/docs".into(),
        version: String::new(),
        args: Vec::new(),
        tools: vec!["echo".into()],
        stopped: None,
        remote: Some(KeptRemote {
            url: "https://docs.example.com/mcp".into(),
            host: "docs.example.com".into(),
            headers: vec![("Authorization".into(), "Bearer ${CTX_KEY}".into())],
        }),
    };
    let files = Files {
        manifest: "agent.toml".into(),
        lock: "agent.lock".into(),
        shared: Vec::new(),
    };
    let text = kept_text(std::slice::from_ref(&kept), &files);
    assert_eq!(
        text,
        "Loaded this session: io.x/docs at docs.example.com as docs (tools: echo). To keep it, add to agent.toml:\n\n\
         [[mcp]]\nname = \"docs\"\nurl  = \"https://docs.example.com/mcp\"\nheaders = { Authorization = \"Bearer ${CTX_KEY}\" }\n\
         # allow = [\"echo\"]   # tools that would run without asking\n\n\
         A remote server's headers name the variables you gave: agent.toml reads them from the environment when it connects, as any `[[mcp]]` header's.\n"
    );
    // With no header, nothing is said of headers.
    let mut bare = kept.clone();
    bare.remote.as_mut().unwrap().headers.clear();
    assert_eq!(
        kept_text(&[bare], &files),
        "Loaded this session: io.x/docs at docs.example.com as docs (tools: echo). To keep it, add to agent.toml:\n\n\
         [[mcp]]\nname = \"docs\"\nurl  = \"https://docs.example.com/mcp\"\n\
         # allow = [\"echo\"]   # tools that would run without asking\n"
    );
    kept.stopped = Some(super::ENDED.into());
    assert!(kept_text(&[kept], &files).contains(
        "as docs (tools: echo, stopped this session: the server ended its session (HTTP 404): it is not reconnected this session)"
    ));
}

/// How the stub server answers.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Ok,
    /// 401 at `initialize`, its body echoing the authorization it got.
    Unauthorized,
    Forbidden,
    /// A 307 to another path of its own: followed by nothing.
    Redirect,
    /// Holds a session, then answers a tool call 404: the session expired.
    Expire,
    /// Lists no tools.
    NoTools,
    /// Never answers `tools/list`.
    Hang,
    /// A tool call answered 401, the body echoing the authorization.
    CallUnauthorized,
    /// A tool's own error result saying "MCP session expired".
    CallSaysExpired,
}

/// An MCP server over streamable HTTP on this machine, recording each
/// request (`<method> <JSON-RPC method> auth=<authorization> <header>=…`).
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
                        .unwrap_or("-")
                        .to_owned();
                    let version = headers
                        .get("x-api-version")
                        .and_then(|a| a.to_str().ok())
                        .unwrap_or("-");
                    let what = request["method"].as_str().unwrap_or("-").to_owned();
                    record
                        .lock()
                        .unwrap()
                        .push(format!("{method} {what} auth={auth} version={version}"));
                    if method == Method::DELETE {
                        return StatusCode::OK.into_response();
                    }
                    match (mode, what.as_str()) {
                        (Mode::Unauthorized, _) => {
                            let body = format!("rejected {auth}");
                            return (StatusCode::UNAUTHORIZED, body).into_response();
                        }
                        (Mode::Forbidden, _) => {
                            return (StatusCode::FORBIDDEN, "no").into_response()
                        }
                        (Mode::Redirect, _) => {
                            return (StatusCode::TEMPORARY_REDIRECT, [("location", "/elsewhere")])
                                .into_response()
                        }
                        (Mode::Expire, "tools/call") => {
                            return StatusCode::NOT_FOUND.into_response()
                        }
                        (Mode::CallUnauthorized, "tools/call") => {
                            let body = format!("revoked {auth}");
                            return (StatusCode::UNAUTHORIZED, body).into_response();
                        }
                        (Mode::Hang, "tools/list") => {
                            tokio::time::sleep(Duration::from_secs(600)).await;
                        }
                        _ => {}
                    }
                    let Some(id) = request.get("id").cloned() else {
                        return StatusCode::ACCEPTED.into_response();
                    };
                    let echo = json!({
                        "name": "echo", "description": "Echo",
                        "inputSchema": { "type": "object", "properties": { "text": { "type": "string" } } }
                    });
                    let result = match what.as_str() {
                        "initialize" => json!({
                            "protocolVersion": "2025-03-26",
                            "capabilities": { "tools": {} },
                            "serverInfo": { "name": "docs", "version": "1" }
                        }),
                        "tools/list" if mode == Mode::NoTools => json!({ "tools": [] }),
                        "tools/list" => json!({ "tools": [echo] }),
                        _ if mode == Mode::CallSaysExpired => json!({ "isError": true,
                            "content": [{ "type": "text", "text": "MCP session expired, says the tool" }] }),
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

/// An answer that is never given: the question stays open.
const HOLD: &str = "<hold>";

/// What a frontend saw: each question (title, message), each notice, and
/// each question withdrawn.
#[derive(Default)]
struct Seen {
    questions: Vec<(String, String)>,
    notices: Vec<(NoticeLevel, String)>,
    withdrawn: usize,
}

/// A frontend answering each question in turn with `answers` (`HOLD`:
/// none; out of answers: `null`, nobody's).
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
                    if value != HOLD {
                        session.send(ClientMessage::UiResponse { id, value });
                    }
                }
                ServerMessage::UiResolved {
                    reason: ResolveReason::Withdrawn,
                    ..
                } => record.lock().unwrap().withdrawn += 1,
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
    let text = "[agent]\nname = \"t\"\nmodel = \"deepseek/deepseek-flash\"\n[extend]\nkinds = [\"remote\"]\nmax_attempts = 10\n";
    Manifest::parse(text, &dir.join("agent.toml"), dir).unwrap()
}

/// `load_plugin` with one remote candidate, `io.x/docs` at `url`, the
/// environment `env`, and a frontend answering `answers`.
fn loading_with(url: &str, headers: Vec<RemoteHeader>, answers: Vec<Json>, env: Lookup) -> Loading {
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
    )
    .with_lookup(env);
    Loading {
        load,
        loaded,
        seen,
        _driver: driver,
        _dir: dir,
    }
}

fn loading(url: &str, headers: Vec<RemoteHeader>, answers: Vec<Json>) -> Loading {
    loading_with(url, headers, answers, with_key())
}

impl Loading {
    async fn call_with(&self, args: Json, ctx: ToolContext) -> Result<ToolResult, ToolError> {
        self.load.execute(args, ctx).await
    }

    async fn call(&self) -> Result<ToolResult, ToolError> {
        self.call_with(
            json!({ "package": "io.x/docs" }),
            ToolContext::new("1", LoadPlugin::TOOL),
        )
        .await
    }

    fn questions(&self) -> Vec<(String, String)> {
        self.seen.lock().unwrap().questions.clone()
    }

    /// The notices once one `wanted` accepts has come (within 5 s).
    async fn notices_until(
        &self,
        wanted: impl Fn(&(NoticeLevel, String)) -> bool,
    ) -> Vec<(NoticeLevel, String)> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let notices = self.seen.lock().unwrap().notices.clone();
            if notices.iter().any(&wanted) || Instant::now() > deadline {
                return notices;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn attempts(&self) -> u32 {
        locked(&self.load.state).attempts
    }

    fn blocked(&self) -> bool {
        locked(&self.load.state).blocked.contains("io.x/docs")
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

fn deleted(seen: &Arc<Mutex<Vec<String>>>) -> bool {
    seen.lock().unwrap().iter().any(|r| r.starts_with("DELETE"))
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
            .any(|r| r.starts_with(&format!("POST initialize auth=Bearer {SECRET} "))),
        "{requests:?}"
    );
    let notices = l
        .notices_until(|(level, m)| {
            *level == NoticeLevel::Info && m.starts_with("The agent connected")
        })
        .await;
    let info = notices
        .iter()
        .find(|(_, m)| m.starts_with("The agent connected"))
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
    assert_eq!(l.attempts(), 1);
    // Its tool, asked of the server, and asked of the owner by the gate.
    let tools = l.loaded.tools().await;
    let [echo] = tools.as_slice() else {
        panic!("{}", tools.len())
    };
    assert_eq!(echo.name(), "docs__echo");
    assert!(l.loaded.asks("docs__echo"));
    let echoed = echo
        .execute(json!({ "text": "hi" }), ToolContext::new("2", "docs__echo"))
        .await;
    assert_eq!(text(&echoed), "echoed hi");
    // The block read back.
    let block = l.loaded.kept()[0].block();
    let manifest = format!(
        "[agent]\nname = \"t\"\nmodel = \"deepseek/deepseek-flash\"\n{}",
        block.replace(&url, "https://docs.example.com/mcp")
    );
    let dir = tempfile::tempdir().unwrap();
    let read = Manifest::parse(&manifest, &dir.path().join("a.toml"), dir.path()).unwrap();
    assert_eq!(read.mcp[0].name, "docs");
    // Closed at the end: its session ended with a DELETE.
    l.loaded.close().await;
    assert!(deleted(&seen), "{:?}", seen.lock().unwrap());
}

#[tokio::test]
async fn a_credential_is_asked_again_and_never_repeated() {
    let (url, _) = stub(Mode::Ok).await;
    let l = loading(
        &url,
        vec![authorization(true)],
        vec![
            json!("Allow"),
            json!("DEEPSEEK_API_KEY"),
            json!("ghp_PastedSecret123"),
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
            .starts_with("Not taken: no variable of that name is set here, or it is empty."),
        "{}",
        questions[3].1
    );
    assert!(questions.iter().all(|(_, m)| !m.contains("ghp_")));
}

#[tokio::test]
async fn a_pasted_key_reaches_no_result_nor_notice() {
    let (url, seen) = stub(Mode::Ok).await;
    let pasted = "sk-live-0123456789abcdef";
    let l = loading(
        &url,
        vec![authorization(true)],
        vec![json!("Allow"), json!(pasted), json!(pasted), json!(pasted)],
    );
    let said = text(&l.call().await);
    assert!(
        said.contains("not connected: no usable variable was named for its required header `Authorization` after 3 tries (the last: that is not a variable's name"),
        "{said}"
    );
    let notices = l.notices_until(|(_, m)| m.contains("was refused")).await;
    for text in notices.iter().map(|(_, m)| m).chain([&said]) {
        assert!(!text.contains(pasted), "{text}");
    }
    assert!(l.questions().iter().all(|(_, m)| !m.contains(pasted)));
    assert!(seen.lock().unwrap().is_empty(), "nothing connected");
}

#[tokio::test]
async fn a_value_that_cannot_be_a_header_is_asked_again() {
    let (url, _) = stub(Mode::Ok).await;
    let env = lookup(&[(KEY, SECRET.into()), ("WITH_NEWLINE", "abc\ndef".into())]);
    let l = loading_with(
        &url,
        vec![authorization(true)],
        vec![
            json!("Allow"),
            json!("WITH_NEWLINE"),
            json!(KEY),
            json!("Allow"),
        ],
        env,
    );
    let result = l.call().await;
    assert!(result.is_ok(), "{}", text(&result));
    assert!(
        l.questions()[2].1.starts_with(
            "Not taken: that variable's value cannot be sent in a header (visible ASCII only"
        ),
        "{:?}",
        l.questions()
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
    assert!(!l.blocked());
    let notices = l.notices_until(|(_, m)| m.contains("was refused")).await;
    assert!(
        notices
            .iter()
            .any(|(level, m)| *level == NoticeLevel::Warning && m.contains("not connected")),
        "{notices:?}"
    );
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
        seen.lock().unwrap().iter().all(|r| r.contains("auth=- ")),
        "{:?}",
        seen.lock().unwrap()
    );
    let block = l.loaded.kept()[0].block();
    assert!(!block.contains("headers"), "{block}");
}

#[tokio::test]
async fn an_optional_credential_never_taken_is_said_not_sent() {
    let (url, _) = stub(Mode::Ok).await;
    let l = loading(
        &url,
        vec![authorization(false)],
        vec![
            json!("Allow"),
            json!("NOPE_1"),
            json!("NOPE_2"),
            json!("NOPE_3"),
            json!("Allow"),
        ],
    );
    let result = l.call().await;
    assert!(result.is_ok(), "{}", text(&result));
    let said = "its optional header `Authorization` is not sent: no usable variable was named after 3 tries (the last: no variable of that name is set here, or it is empty)";
    let keep = &l.questions()[4].1;
    assert!(keep.ends_with(&format!("Not sent: {said}.")), "{keep}");
    let notices = l.notices_until(|(_, m)| m.contains(said)).await;
    assert!(
        notices
            .iter()
            .any(|(level, m)| *level == NoticeLevel::Warning && m.contains(said)),
        "{notices:?}"
    );
}

#[tokio::test]
async fn a_fixed_header_is_sent_as_listed_and_nothing_asked() {
    let (url, seen) = stub(Mode::Ok).await;
    let fixed = RemoteHeader {
        name: "X-Api-Version".into(),
        required: true,
        secret: false,
        description: String::new(),
        template: Some("v1".into()),
    };
    let l = loading(&url, vec![fixed], vec![json!("Allow"), json!("Allow")]);
    let result = l.call().await;
    assert!(result.is_ok(), "{}", text(&result));
    assert_eq!(l.questions().len(), 2, "no credential asked");
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .any(|r| r.starts_with("POST initialize ") && r.ends_with("version=v1")),
        "{:?}",
        seen.lock().unwrap()
    );
    let block = l.loaded.kept()[0].block();
    assert!(
        block.contains("headers = { X-Api-Version = \"v1\" }"),
        "{block}"
    );
}

#[tokio::test]
async fn block_at_either_question_is_remembered_and_nobody_answering_is_not() {
    // Block at the first: nothing connected.
    let (url, seen) = stub(Mode::Ok).await;
    let l = loading(&url, Vec::new(), vec![json!("Block")]);
    let said = text(&l.call().await);
    assert!(
        said.contains("the owner blocked it (connect to it)"),
        "{said}"
    );
    assert!(l.blocked() && seen.lock().unwrap().is_empty());
    let again = text(&l.call().await);
    assert!(
        again.contains("the owner blocked io.x/docs earlier this session"),
        "{again}"
    );
    // Block at the second: connected, then closed.
    let (url, seen) = stub(Mode::Ok).await;
    let l = loading(&url, Vec::new(), vec![json!("Allow"), json!("Block")]);
    let said = text(&l.call().await);
    assert!(said.contains("the owner blocked it (keep it)"), "{said}");
    assert!(l.blocked() && deleted(&seen));
    assert!(l.loaded.tools().await.is_empty());
    // Nobody answering either one: not remembered.
    for answers in [vec![], vec![json!("Allow")]] {
        let (url, seen) = stub(Mode::Ok).await;
        let connects = !answers.is_empty();
        let l = loading(&url, Vec::new(), answers);
        let said = text(&l.call().await);
        assert!(said.contains("nobody answered whether to"), "{said}");
        assert!(!l.blocked());
        assert_eq!(deleted(&seen), connects, "{:?}", seen.lock().unwrap());
    }
    // Nobody answering the credential: not remembered, nothing connected.
    let (url, seen) = stub(Mode::Ok).await;
    let l = loading(&url, vec![authorization(true)], vec![json!("Allow")]);
    let said = text(&l.call().await);
    assert!(
        said.contains("nobody answered which variable holds its `Authorization` header: it may be proposed again"),
        "{said}"
    );
    assert!(!l.blocked() && seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_run_stopped_at_any_question_or_while_connecting_ends_the_load() {
    // At each question, held open, then the run stopped: the question
    // withdrawn, a Warning.
    for at in 0..3 {
        let (url, seen) = stub(Mode::Ok).await;
        let mut answers = vec![json!("Allow"), json!(KEY), json!("Allow")];
        answers[at] = json!(HOLD);
        let l = loading(&url, vec![authorization(true)], answers);
        let ctx = ToolContext::new("1", LoadPlugin::TOOL);
        let cancel = ctx.cancel.clone();
        let questions = l.seen.clone();
        tokio::spawn(async move {
            while questions.lock().unwrap().questions.len() <= at {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            cancel.cancel();
        });
        let said = text(&l.call_with(json!({ "package": "io.x/docs" }), ctx).await);
        assert!(
            said.contains("the run was stopped while the owner was asked"),
            "{at}: {said}"
        );
        assert!(!l.blocked());
        assert_eq!(deleted(&seen), at == 2, "{at}: {:?}", seen.lock().unwrap());
        let deadline = Instant::now() + Duration::from_secs(5);
        while l.seen.lock().unwrap().withdrawn == 0 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(l.seen.lock().unwrap().withdrawn, 1, "{at}");
    }
    // While it connects (`tools/list` never answered): ended at once, its
    // session closed within the bound.
    let (url, seen) = stub(Mode::Hang).await;
    let l = loading(&url, Vec::new(), vec![json!("Allow")]);
    let ctx = ToolContext::new("1", LoadPlugin::TOOL);
    let cancel = ctx.cancel.clone();
    let watched = seen.clone();
    tokio::spawn(async move {
        while !watched
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.contains("tools/list"))
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        cancel.cancel();
    });
    let started = Instant::now();
    let said = text(&l.call_with(json!({ "package": "io.x/docs" }), ctx).await);
    assert!(
        said.contains("the run was stopped while it connected"),
        "{said}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn a_remote_that_wants_a_sign_in_is_said_so_and_the_key_hidden() {
    for (mode, status) in [(Mode::Unauthorized, 401), (Mode::Forbidden, 403)] {
        let (url, _) = stub(mode).await;
        let l = loading(
            &url,
            vec![authorization(true)],
            vec![json!("Allow"), json!(KEY)],
        );
        let said = text(&l.call().await);
        assert!(
            said.contains(&format!("io.x/docs refused yobuild's request (HTTP {status}): it needs a credential, or a sign-in (OAuth) yobuild cannot do yet")),
            "{said}"
        );
        assert!(!said.contains(SECRET), "{said}");
        if mode == Mode::Unauthorized {
            // The server echoed the key it refused: shown as its name.
            assert!(
                said.contains(&format!("rejected Bearer ${{{KEY}}}")),
                "{said}"
            );
        }
        let notices = l.notices_until(|(_, m)| m.contains("was refused")).await;
        assert!(
            notices.iter().any(|(level, m)| *level == NoticeLevel::Error
                && m.starts_with("The agent's plugin `io.x/docs` was refused: io.x/docs refused")
                && !m.contains(SECRET)),
            "{notices:?}"
        );
        assert_eq!(l.attempts(), 1);
    }
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
    let notices = l.notices_until(|(_, m)| m.contains("was refused")).await;
    assert!(
        notices
            .iter()
            .any(|(level, m)| *level == NoticeLevel::Error && m.contains("did not connect")),
        "{notices:?}"
    );
}

#[tokio::test]
async fn a_remote_listing_no_tools_is_refused_and_closed() {
    let (url, seen) = stub(Mode::NoTools).await;
    let l = loading(&url, Vec::new(), vec![json!("Allow")]);
    let said = text(&l.call().await);
    assert!(said.contains("io.x/docs: it lists no tools"), "{said}");
    assert_eq!(l.questions().len(), 1, "never asked to keep it");
    assert!(deleted(&seen));
}

#[tokio::test]
async fn a_version_is_refused_uncounted_and_args_counted() {
    let (url, _) = stub(Mode::Ok).await;
    let l = loading(&url, Vec::new(), Vec::new());
    let said = text(
        &l.call_with(
            json!({ "package": "io.x/docs", "version": "1.0.0" }),
            ToolContext::new("1", LoadPlugin::TOOL),
        )
        .await,
    );
    assert!(
        said.contains("io.x/docs: a remote server has no version to take (give no `version`)"),
        "{said}"
    );
    assert_eq!(l.attempts(), 0);
    let said = text(
        &l.call_with(
            json!({ "package": "io.x/docs", "args": ["--x"] }),
            ToolContext::new("1", LoadPlugin::TOOL),
        )
        .await,
    );
    assert!(
        said.contains("a remote MCP server takes no `args` (they are a stdio server's)"),
        "{said}"
    );
    assert_eq!(l.attempts(), 1);
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
    // Not told to reconnect: it cannot.
    assert_eq!(text(&failed), super::ENDED);
    // Its tools leave at once; said stopped once settled.
    assert!(l.loaded.tools().await.is_empty());
    tokio::time::sleep(SETTLE + Duration::from_millis(100)).await;
    assert_eq!(
        l.loaded.stopped(),
        [("docs".to_owned(), super::ENDED.to_owned())]
    );
    assert!(
        l.loaded.asks("docs__echo"),
        "its tools still ask, then are not found"
    );
}

#[tokio::test]
async fn a_calls_error_hides_the_key_and_a_tools_own_words_end_nothing() {
    // A 401 to a call, echoing the key: shown as its name.
    let (url, _) = stub(Mode::CallUnauthorized).await;
    let l = loading(
        &url,
        vec![authorization(true)],
        vec![json!("Allow"), json!(KEY), json!("Allow")],
    );
    assert!(l.call().await.is_ok());
    let tools = l.loaded.tools().await;
    let failed = text(
        &tools[0]
            .execute(json!({ "text": "hi" }), ToolContext::new("2", "docs__echo"))
            .await,
    );
    assert!(
        failed.contains(&format!("revoked Bearer ${{{KEY}}}")),
        "{failed}"
    );
    assert!(!failed.contains(SECRET), "{failed}");
    // A tool's error result that says "MCP session expired": its own
    // words, not the transport's: the server stays.
    let (url, _) = stub(Mode::CallSaysExpired).await;
    let l = loading(&url, Vec::new(), vec![json!("Allow"), json!("Allow")]);
    assert!(l.call().await.is_ok());
    let tools = l.loaded.tools().await;
    let failed = text(
        &tools[0]
            .execute(json!({ "text": "hi" }), ToolContext::new("2", "docs__echo"))
            .await,
    );
    assert_eq!(failed, "MCP session expired, says the tool");
    assert_eq!(l.loaded.tools().await.len(), 1);
}
