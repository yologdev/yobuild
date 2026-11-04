//! The browser frontend: a page, the UI plugins' modules, and the protocol
//! over a WebSocket.
//!
//! - `GET /` — the page (`frontend/web/index.html`, `app.js`, `lib.js`, built in).
//! - `GET /ws` — the WebSocket: each text frame is one JSON message,
//!   [`ClientMessage`] in, [`ServerMessage`] out.
//! - `GET /ui-plugins/{name}` — a UI plugin's ES module (`{name}` ends in `.js`).
//! - `GET /runs?t=…` — the agent's past runs, as [`serve_with`] was given
//!   them (`yobuild run`'s: its run log, design §16); 404 without; the
//!   token required, as `/ws`'s.
//! - `/api/runs` — with an [`Api`] ([`serve_api`]; `yobuild serve`, design
//!   §18): `POST /api/runs` `{"task": …}` queues a task; `GET
//!   /api/runs/{id}` (`?wait=S`, up to 600 s, until it is done) says where
//!   it is; `GET /api/runs/{id}/events` streams its run's messages, one
//!   JSON line each, the last its status. The token is required, as
//!   `?t=…` or `Authorization: Bearer …`; a `POST` must be JSON
//!   (`Content-Type: application/json`), so a page elsewhere cannot send
//!   one without the browser asking first, which this server never allows.
//!
//! **The WebSocket needs a token.** Browsers do not apply cross-origin rules
//! to WebSockets, so without one any page the user has open — on any site —
//! could connect to a server on localhost and drive an agent that has
//! `bash`. [`serve`] makes a random token and returns the URL that carries it
//! (`/?t=…`); the page passes it on to `/ws`, which refuses a missing or
//! wrong one. Share that URL only with whoever may drive the agent. UI plugin
//! modules are trusted code: they run in the page with its privileges.

use std::net::SocketAddr;

use axum::body::{Body, Bytes};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, RawQuery, State};
use axum::http::HeaderMap;
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use futures::{SinkExt, StreamExt};

use crate::frontend::protocol::{ClientMessage, NoticeLevel, ServerMessage};
use crate::frontend::session::Session;

const INDEX: &str = include_str!("../../frontend/web/index.html");
const APP: &str = include_str!("../../frontend/web/app.js");
const LIB: &str = include_str!("../../frontend/web/lib.js");

/// A running browser frontend; serving stops with [`Served::stop`] or when
/// this is dropped.
pub struct Served {
    addr: SocketAddr,
    token: String,
    task: tokio::task::JoinHandle<()>,
}

impl Served {
    /// The page's URL, token included: open this one.
    pub fn url(&self) -> String {
        format!("http://{}/?t={}", self.addr, self.token)
    }

    /// The bound address (the one passed may use port 0).
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The token `/ws` requires.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Stop serving: no new connections (open ones end with their socket).
    pub fn stop(self) {}
}

impl Drop for Served {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Clone)]
struct App {
    session: Session,
    token: std::sync::Arc<str>,
    runs: Option<PastRuns>,
    api: Option<std::sync::Arc<dyn Api>>,
}

/// Tasks a program on this machine sends (`yobuild serve`, design §18):
/// each one's status is a JSON object with at least `id`, `state`, `done`
/// and, once its run started, `run` (the session's run).
pub trait Api: Send + Sync + 'static {
    /// Queues `task`: its status, or why not.
    fn submit(&self, task: String) -> Result<serde_json::Value, String>;
    /// The task `id`'s status; `None` when there is no such task.
    fn status(&self, id: &str) -> Option<serde_json::Value>;
    /// The session's run of task `id`, once it started; `None` when there
    /// is no such task.
    fn run_of(&self, id: &str) -> Option<Option<u64>>;
    /// Moves on each change of any task.
    fn changes(&self) -> tokio::sync::watch::Receiver<u64>;
}

/// The page, naming `agent` in its title and header when there is one.
fn index(agent: Option<&str>) -> String {
    let named = agent.map(|name| {
        let name = name
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&#39;");
        format!("{name} · ")
    });
    INDEX.replace("<!--agent-->", named.as_deref().unwrap_or(""))
}

/// What `GET /runs` answers, read each time it is asked (off the async
/// threads): a JSON value the page shows as its past runs.
pub type PastRuns = std::sync::Arc<dyn Fn() -> serde_json::Value + Send + Sync>;

/// Serve the browser frontend for `session` on `addr`, with a new random
/// token, until the returned [`Served`] is stopped or dropped.
pub async fn serve(session: Session, addr: SocketAddr) -> std::io::Result<Served> {
    serve_with(session, addr, None, None).await
}

/// [`serve_with`], with the tasks `/api/runs` takes.
pub async fn serve_api(
    session: Session,
    addr: SocketAddr,
    runs: Option<PastRuns>,
    agent: Option<&str>,
    api: std::sync::Arc<dyn Api>,
) -> std::io::Result<Served> {
    serving(session, addr, runs, agent, Some(api)).await
}

/// [`serve`], with the past runs `GET /runs` answers and the agent's name
/// on the page.
pub async fn serve_with(
    session: Session,
    addr: SocketAddr,
    runs: Option<PastRuns>,
    agent: Option<&str>,
) -> std::io::Result<Served> {
    serving(session, addr, runs, agent, None).await
}

async fn serving(
    session: Session,
    addr: SocketAddr,
    runs: Option<PastRuns>,
    agent: Option<&str>,
    api: Option<std::sync::Arc<dyn Api>>,
) -> std::io::Result<Served> {
    let page = Html(index(agent));
    let token = uuid::Uuid::new_v4().simple().to_string();
    let state = App {
        session,
        token: token.clone().into(),
        runs,
        api,
    };
    let app = Router::new()
        .route("/", get(move || async move { page }))
        .route("/app.js", get(|| async { javascript(APP.to_owned()) }))
        .route("/lib.js", get(|| async { javascript(LIB.to_owned()) }))
        .route("/favicon.ico", get(|| async { StatusCode::NO_CONTENT }))
        .route("/ui-plugins/{name}", get(ui_plugin))
        .route("/ws", get(socket))
        .route("/runs", get(past_runs))
        .route("/api/runs", post(submit))
        .route("/api/runs/{id}", get(status))
        .route("/api/runs/{id}/events", get(events))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    let task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::warn!("the web frontend stopped: {e}");
        }
    });
    Ok(Served {
        addr: bound,
        token,
        task,
    })
}

fn javascript(source: String) -> Response {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        source,
    )
        .into_response()
}

async fn ui_plugin(State(app): State<App>, Path(file): Path<String>) -> Response {
    let name = file.strip_suffix(".js").unwrap_or(&file);
    match app.session.ui_plugin_module(name) {
        Some(module) => javascript(module.to_string()),
        None => (StatusCode::NOT_FOUND, "no such UI plugin").into_response(),
    }
}

/// Whether `query` carries the server's token.
fn token_given(app: &App, query: Option<&str>) -> bool {
    let given = query
        .unwrap_or_default()
        .split('&')
        .find_map(|pair| pair.strip_prefix("t="));
    given == Some(&*app.token)
}

/// Whether `query` or the `Authorization: Bearer` header carries the token.
fn api_token_given(app: &App, query: Option<&str>, headers: &HeaderMap) -> bool {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    bearer == Some(&*app.token) || token_given(app, query)
}

fn json_response(status: StatusCode, value: serde_json::Value) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        value.to_string(),
    )
        .into_response()
}

fn api_error(status: StatusCode, why: impl Into<String>) -> Response {
    json_response(status, serde_json::json!({ "error": why.into() }))
}

/// The API, once the token is checked; else the response to give.
fn api_of(
    app: &App,
    query: Option<&str>,
    headers: &HeaderMap,
) -> Result<std::sync::Arc<dyn Api>, Box<Response>> {
    if !api_token_given(app, query, headers) {
        return Err(Box::new(api_error(
            StatusCode::FORBIDDEN,
            "a valid token is required",
        )));
    }
    app.api.clone().ok_or_else(|| {
        Box::new(api_error(
            StatusCode::NOT_FOUND,
            "this session takes no tasks: `yobuild serve` does",
        ))
    })
}

/// `POST /api/runs` `{"task": "…"}`: 202 and the task's status.
async fn submit(
    State(app): State<App>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let api = match api_of(&app, query.as_deref(), &headers) {
        Ok(api) => api,
        Err(response) => return *response,
    };
    let json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .is_some_and(|t| t.trim() == "application/json")
        });
    if !json {
        return api_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "send the task as JSON (Content-Type: application/json)",
        );
    }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Sent {
        task: String,
    }
    let sent: Sent = match serde_json::from_slice(&body) {
        Ok(sent) => sent,
        Err(e) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                format!("the body is {{\"task\": \"…\"}}: {e}"),
            )
        }
    };
    match api.submit(sent.task) {
        Ok(status) => json_response(StatusCode::ACCEPTED, status),
        Err(why) => api_error(StatusCode::UNPROCESSABLE_ENTITY, why),
    }
}

/// The most messages `events` holds for a task whose run has not started.
const HELD_AT_MOST: usize = 10_000;

/// The longest `?wait=` a status waits.
const WAIT_AT_MOST: u64 = 600;

/// `GET /api/runs/{id}[?wait=S]`: the task's status, once it is done or
/// `S` seconds passed.
async fn status(
    State(app): State<App>,
    Path(id): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    let api = match api_of(&app, query.as_deref(), &headers) {
        Ok(api) => api,
        Err(response) => return *response,
    };
    let wait = query
        .as_deref()
        .unwrap_or_default()
        .split('&')
        .find_map(|pair| pair.strip_prefix("wait="));
    let wait = match wait.map(str::parse::<u64>) {
        None => 0,
        Some(Ok(s)) if s <= WAIT_AT_MOST => s,
        Some(_) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                format!("wait is a number of seconds, 0 to {WAIT_AT_MOST}"),
            )
        }
    };
    let mut changes = api.changes();
    let until = tokio::time::Instant::now() + std::time::Duration::from_secs(wait);
    loop {
        let Some(status) = api.status(&id) else {
            return api_error(StatusCode::NOT_FOUND, format!("no task {id}"));
        };
        let done = status["done"].as_bool().unwrap_or(true);
        if done || tokio::time::Instant::now() >= until {
            return json_response(StatusCode::OK, status);
        }
        if tokio::time::timeout_at(until, changes.changed())
            .await
            .is_ok_and(|r| r.is_err())
        {
            return json_response(StatusCode::OK, status);
        }
    }
}

/// `GET /api/runs/{id}/events`: the task's run's messages as they come
/// (those before included), one JSON line each, then its status, once it
/// is done.
async fn events(
    State(app): State<App>,
    Path(id): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    let api = match api_of(&app, query.as_deref(), &headers) {
        Ok(api) => api,
        Err(response) => return *response,
    };
    if api.status(&id).is_none() {
        return api_error(StatusCode::NOT_FOUND, format!("no task {id}"));
    }
    let session = app.session.clone();
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(64);
    tokio::spawn(async move {
        // Attached as an observer: it is never counted as the owner there.
        let mut watching = session.observe();
        let mut changes = api.changes();
        // Its run's messages until it started are held: the history replay
        // and what comes may hold them before the task knows its run.
        let mut held: Vec<ServerMessage> = Vec::new();
        // The history replay is there at once: held, so a task done already
        // still streams its run.
        while let Ok(message) = watching.messages.try_recv() {
            if held.len() >= HELD_AT_MOST {
                held.remove(0);
            }
            held.push(message);
        }
        let line = |value: &serde_json::Value| Ok(Bytes::from(format!("{value}\n")));
        let of_run = |message: &ServerMessage| match message {
            ServerMessage::RunStart { run, .. }
            | ServerMessage::Event { run, .. }
            | ServerMessage::RunEnd { run, .. } => Some(*run),
            _ => None,
        };
        loop {
            let run = api.run_of(&id).flatten();
            if let Some(run) = run {
                for message in held.drain(..).filter(|m| of_run(m) == Some(run)) {
                    let Ok(value) = serde_json::to_value(&message) else {
                        continue;
                    };
                    if tx.send(line(&value)).await.is_err() {
                        return;
                    }
                }
            }
            let status = api.status(&id).unwrap_or_default();
            if status["done"].as_bool().unwrap_or(true) {
                let end = serde_json::json!({ "type": "task", "task": status });
                let _ = tx.send(line(&end)).await;
                return;
            }
            tokio::select! {
                message = watching.messages.recv() => match message {
                    Some(message) if of_run(&message).is_some() => match run {
                        Some(run) if of_run(&message) == Some(run) => {
                            let Ok(value) = serde_json::to_value(&message) else { continue };
                            if tx.send(line(&value)).await.is_err() {
                                return;
                            }
                        }
                        Some(_) => {}
                        None => {
                            // Bounded: a task waiting long behind others.
                            if held.len() >= HELD_AT_MOST {
                                held.remove(0);
                            }
                            held.push(message);
                        }
                    },
                    Some(_) => {}
                    // The session ended: its status says so next.
                    None => {
                        if changes.changed().await.is_err() {
                            return;
                        }
                    }
                },
                changed = changes.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
            }
        }
    });
    let stream = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    (
        [
            (header::CONTENT_TYPE, "application/x-ndjson"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Body::from_stream(stream),
    )
        .into_response()
}

/// The past runs, as JSON: the agent's own tasks and answers, so the token
/// is required.
async fn past_runs(State(app): State<App>, RawQuery(query): RawQuery) -> Response {
    if !token_given(&app, query.as_deref()) {
        return (StatusCode::FORBIDDEN, "a valid token is required").into_response();
    }
    let Some(runs) = app.runs else {
        return (StatusCode::NOT_FOUND, "no past runs here").into_response();
    };
    match tokio::task::spawn_blocking(move || runs()).await {
        Ok(value) => (
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            value.to_string(),
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    }
}

async fn socket(
    State(app): State<App>,
    RawQuery(query): RawQuery,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !token_given(&app, query.as_deref()) {
        return (StatusCode::FORBIDDEN, "a valid token is required").into_response();
    }
    let session = app.session;
    upgrade.on_upgrade(move |socket| connection(session, socket))
}

async fn connection(session: Session, socket: WebSocket) {
    let mut connection = session.connect();
    let id = connection.id;
    let (mut out, mut incoming) = socket.split();
    let writer = tokio::spawn(async move {
        while let Some(message) = connection.messages.recv().await {
            let text = match serde_json::to_string(&message) {
                Ok(text) => text,
                Err(e) => {
                    tracing::warn!("a message for the browser is not serializable, skipped: {e}");
                    continue;
                }
            };
            if out.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    });
    while let Some(Ok(frame)) = incoming.next().await {
        let Message::Text(text) = frame else { continue };
        match serde_json::from_str::<ClientMessage>(&text) {
            // An ended session already sent this frontend `closed`.
            Ok(message) => {
                session.send(message);
            }
            Err(e) => {
                tracing::warn!("a browser message that is not the protocol, ignored: {e}");
                session.send_to(
                    id,
                    ServerMessage::Notice {
                        level: NoticeLevel::Warning,
                        message: format!("The session did not understand a message: {e}"),
                    },
                );
            }
        }
    }
    session.disconnect(id);
    writer.abort();
}
