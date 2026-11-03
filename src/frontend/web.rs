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
//!
//! **The WebSocket needs a token.** Browsers do not apply cross-origin rules
//! to WebSockets, so without one any page the user has open — on any site —
//! could connect to a server on localhost and drive an agent that has
//! `bash`. [`serve`] makes a random token and returns the URL that carries it
//! (`/?t=…`); the page passes it on to `/ws`, which refuses a missing or
//! wrong one. Share that URL only with whoever may drive the agent. UI plugin
//! modules are trusted code: they run in the page with its privileges.

use std::net::SocketAddr;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, RawQuery, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
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

/// [`serve`], with the past runs `GET /runs` answers and the agent's name
/// on the page.
pub async fn serve_with(
    session: Session,
    addr: SocketAddr,
    runs: Option<PastRuns>,
    agent: Option<&str>,
) -> std::io::Result<Served> {
    let page = Html(index(agent));
    let token = uuid::Uuid::new_v4().simple().to_string();
    let state = App {
        session,
        token: token.clone().into(),
        runs,
    };
    let app = Router::new()
        .route("/", get(move || async move { page }))
        .route("/app.js", get(|| async { javascript(APP.to_owned()) }))
        .route("/lib.js", get(|| async { javascript(LIB.to_owned()) }))
        .route("/favicon.ico", get(|| async { StatusCode::NO_CONTENT }))
        .route("/ui-plugins/{name}", get(ui_plugin))
        .route("/ws", get(socket))
        .route("/runs", get(past_runs))
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
