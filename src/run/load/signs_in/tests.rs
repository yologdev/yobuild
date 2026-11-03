use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value as Json};
use tokio_util::sync::CancellationToken;
use yoagent::tool_source::ToolSource;

use super::{sign_in_all, Signing, SignsIn};
use crate::frontend::{ClientMessage, NoticeLevel, ServerMessage, Session, UiRequest};
use crate::run::load::Loaded;
use crate::run::oauth::tests::{access, get as browse, refresh, to_callback, Stub};
use crate::run::oauth::Net;

/// A sign-in question answered in the browser.
const BROWSER: &str = "<browser>";

/// What the frontend saw.
#[derive(Default)]
struct Seen {
    titles: Vec<String>,
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
                    let title = match &request {
                        UiRequest::Select { title, .. } | UiRequest::SignIn { title, .. } => {
                            title.clone()
                        }
                        other => format!("{other:?}"),
                    };
                    record.lock().unwrap().titles.push(title);
                    let value = answers.next().unwrap_or(Json::Null);
                    match &request {
                        UiRequest::SignIn { url: Some(url), .. } if value == BROWSER => {
                            let (url, session) = (url.clone(), session.clone());
                            tokio::spawn(async move {
                                let browsed = tokio::spawn(async move {
                                    browse(&to_callback(&url).await).await.unwrap();
                                })
                                .await;
                                if browsed.is_err() {
                                    session.send(ClientMessage::UiResponse {
                                        id,
                                        value: json!("cancel"),
                                    });
                                }
                            });
                        }
                        _ => {
                            session.send(ClientMessage::UiResponse { id, value });
                        }
                    }
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

/// The stub's server as agent.toml's `[[mcp]] docs`, allowing `allow`.
fn docs(stub: &Stub, allow: &[&str]) -> SignsIn {
    SignsIn {
        name: "docs".into(),
        url: stub.server(),
        headers: Vec::new(),
        hide: Vec::new(),
        allow: allow.iter().map(|a| a.to_string()).collect(),
    }
}

/// Signs `server` in with a frontend answering `answers`; what it saw.
async fn signed(server: SignsIn, answers: Vec<Json>) -> (Loaded, Arc<Mutex<Seen>>) {
    let (session, driver) = Session::new(false);
    std::mem::forget(driver);
    let seen = answering(&session, answers);
    let loaded = Loaded::default();
    let signing = Signing {
        session,
        loaded: loaded.clone(),
        phone: false,
        net: Some(Net::loopback()),
        timeout: Duration::from_secs(10),
        stop: CancellationToken::new(),
    };
    tokio::time::timeout(Duration::from_secs(10), sign_in_all(signing, vec![server]))
        .await
        .unwrap();
    // The last notice is sent before `sign_in_all` returns; the frontend
    // reads it a moment later.
    tokio::time::sleep(Duration::from_millis(100)).await;
    (loaded, seen)
}

#[tokio::test]
async fn a_manifest_server_is_signed_in_offered_and_its_allow_checked() {
    let stub = Stub::start().await;
    let (loaded, seen) = signed(
        docs(&stub, &["echo", "search"]),
        vec![json!("Allow"), json!(BROWSER)],
    )
    .await;
    {
        let seen = seen.lock().unwrap();
        assert_eq!(
            seen.titles,
            [
                "Sign in to docs (127.0.0.1) at 127.0.0.1? (for agent.toml's [[mcp]] docs)",
                "Sign in to docs at 127.0.0.1 in your browser",
            ]
        );
        assert!(
        seen.notices.contains(&(
            NoticeLevel::Info,
            "agent.toml's [[mcp]] docs: signed in; its tools are the agent's from its next step: docs__echo".into()
        )),
        "{:?}",
        seen.notices
    );
        assert!(
        seen.notices.contains(&(
            NoticeLevel::Warning,
            "agent.toml's [[mcp]] docs allows `search`, which it does not offer (it offers: echo): nothing of that name runs".into()
        )),
        "{:?}",
        seen.notices
    );
        for (_, notice) in &seen.notices {
            assert!(!notice.contains(&access(1)), "{notice}");
        }
    }
    // Offered through `Loaded`, under the manifest's `allow`: not one the
    // agent loaded, so in no block.
    let tools = loaded.tools().await;
    assert_eq!(
        tools.iter().map(|t| t.name()).collect::<Vec<_>>(),
        ["docs__echo"]
    );
    assert!(!loaded.asks("docs__echo"));
    assert!(loaded.kept().is_empty());
    // The session's end: closed, then revoked.
    loaded.close().await;
    let seen = stub.seen();
    assert!(
        seen.contains(&format!("revoke refresh_token {}", refresh(1))),
        "{seen:?}"
    );
}

#[tokio::test]
async fn a_manifest_server_may_sign_in_on_another_site() {
    let stub = Stub::start().await;
    let elsewhere = stub.base.replace("127.0.0.1", "localhost");
    stub.set(|c| {
        c.resource["authorization_servers"] = json!([elsewhere]);
        c.authorization["issuer"] = json!(elsewhere);
        c.iss = Some(elsewhere.clone());
    });
    let (loaded, seen) = signed(docs(&stub, &[]), vec![json!("Allow"), json!(BROWSER)]).await;
    assert_eq!(
        loaded.tools().await.len(),
        1,
        "{:?}",
        seen.lock().unwrap().notices
    );
    loaded.close().await;
}

#[tokio::test]
async fn a_manifest_server_blocked_or_unanswered_offers_nothing() {
    for (answers, said) in [
        (
            vec![json!("Block")],
            "agent.toml's [[mcp]] docs: not signed in: the owner blocked it, so it offers no tools this session",
        ),
        (
            vec![json!("Allow"), json!("cancel")],
            "agent.toml's [[mcp]] docs: not signed in: the owner cancelled the sign-in: it offers no tools this session",
        ),
    ] {
        let stub = Stub::start().await;
        let (loaded, seen) = signed(docs(&stub, &[]), answers).await;
        assert!(loaded.tools().await.is_empty());
        let seen = seen.lock().unwrap();
        assert!(
            seen.notices.contains(&(NoticeLevel::Warning, said.into())),
            "{:?}",
            seen.notices
        );
    }
}
