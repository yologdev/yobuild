use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value as Json};
use tokio_util::sync::CancellationToken;
use yoagent::tool_source::ToolSource;

use super::{sign_in_all, ManifestHeader, Signing, SignsIn};
use crate::frontend::{ClientMessage, NoticeLevel, ServerMessage, Session, UiRequest};
use crate::run::load::Loaded;
use crate::run::oauth::tests::{access, get as browse, refresh, to_callback, Stub};
use crate::run::oauth::Net;

/// A sign-in question answered in the browser.
const BROWSER: &str = "<browser>";
/// A question left open.
const SILENT: &str = "<silent>";

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
                        _ if value == SILENT => {}
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

/// How the tests sign in: on this machine, its timeout `timeout`.
fn signing(session: &Session, loaded: &Loaded, timeout: Duration) -> Signing {
    Signing {
        session: session.clone(),
        loaded: loaded.clone(),
        phone: false,
        net: Some(Net::loopback()),
        timeout,
        stop: CancellationToken::new(),
    }
}

/// A session whose driver never runs (nothing here needs one).
fn session() -> Session {
    let (session, driver) = Session::new(false);
    std::mem::forget(driver);
    session
}

/// Signs `server` in with a frontend answering `answers`; what it saw.
async fn signed(server: SignsIn, answers: Vec<Json>) -> (Loaded, Arc<Mutex<Seen>>) {
    signed_within(server, answers, Duration::from_secs(10)).await
}

/// The same, the sign-in's timeout `timeout`.
async fn signed_within(
    server: SignsIn,
    answers: Vec<Json>,
    timeout: Duration,
) -> (Loaded, Arc<Mutex<Seen>>) {
    let session = session();
    let seen = answering(&session, answers);
    let loaded = Loaded::default();
    let signing = signing(&session, &loaded, timeout);
    tokio::time::timeout(Duration::from_secs(20), sign_in_all(signing, vec![server]))
        .await
        .unwrap();
    // The last notice is sent before `sign_in_all` returns; the frontend
    // reads it a moment later.
    noticed(&seen, 1).await;
    (loaded, seen)
}

/// Waits until `seen` holds `count` notices (or 5 s).
async fn noticed(seen: &Arc<Mutex<Seen>>, count: usize) {
    for _ in 0..100 {
        if seen.lock().unwrap().notices.len() >= count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Whether `seen` got exactly this notice.
fn said(seen: &Arc<Mutex<Seen>>, level: NoticeLevel, text: &str) -> bool {
    seen.lock()
        .unwrap()
        .notices
        .contains(&(level, text.to_owned()))
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
    // Said as agent.toml's when it stops, not as one the agent loaded.
    assert_eq!(loaded.who("docs"), "agent.toml's [[mcp]] docs");
    assert_eq!(loaded.who("other"), "The loaded MCP server `other`");
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
async fn a_manifest_server_blocked_cancelled_or_unanswered_offers_nothing() {
    for (answers, level, text) in [
        (
            vec![json!("Block")],
            NoticeLevel::Warning,
            "agent.toml's [[mcp]] docs: not signed in: the owner blocked it, so it offers no tools this session",
        ),
        (
            vec![json!("Allow"), json!("cancel")],
            NoticeLevel::Warning,
            "agent.toml's [[mcp]] docs: not signed in: the owner cancelled the sign-in: it offers no tools this session",
        ),
        (
            vec![Json::Null],
            NoticeLevel::Warning,
            "agent.toml's [[mcp]] docs: not signed in: nobody answered whether to sign in, so it offers no tools this session",
        ),
        (
            vec![json!("Allow"), json!(SILENT)],
            NoticeLevel::Warning,
            "agent.toml's [[mcp]] docs: not signed in: nobody signed in within 1 s: it offers no tools this session",
        ),
    ] {
        let stub = Stub::start().await;
        let (loaded, seen) =
            signed_within(docs(&stub, &[]), answers, Duration::from_secs(1)).await;
        assert!(loaded.tools().await.is_empty());
        assert!(said(&seen, level, text), "{:?}", seen.lock().unwrap().notices);
        assert!(
            !stub.seen().iter().any(|s| s.starts_with("token")),
            "{:?}",
            stub.seen()
        );
    }
}

#[tokio::test]
async fn a_failure_of_the_authorization_server_is_an_error() {
    let stub = Stub::start().await;
    stub.set(|c| {
        c.authorization["code_challenge_methods_supported"] = json!(["plain"]);
    });
    let (loaded, seen) = signed(docs(&stub, &[]), Vec::new()).await;
    assert!(loaded.tools().await.is_empty());
    let seen = seen.lock().unwrap();
    assert!(seen.titles.is_empty(), "asked nothing: {:?}", seen.titles);
    assert!(
        matches!(
            seen.notices.as_slice(),
            [(NoticeLevel::Error, n)] if n.starts_with("agent.toml's [[mcp]] docs: not signed in: ")
        ),
        "{:?}",
        seen.notices
    );
}

#[tokio::test]
async fn a_server_answering_unsigned_is_offered_as_it_is() {
    let stub = Stub::start().await;
    stub.set(|c| c.open = true);
    let (loaded, seen) = signed(docs(&stub, &["echo"]), Vec::new()).await;
    {
        let seen = seen.lock().unwrap();
        assert!(seen.titles.is_empty(), "{:?}", seen.titles);
        assert_eq!(
            seen.notices,
            [(
                NoticeLevel::Info,
                "agent.toml's [[mcp]] docs: it answered without a sign-in; its tools are the agent's from its next step: docs__echo".into()
            )]
        );
    }
    assert_eq!(loaded.tools().await.len(), 1);
    loaded.close().await;
    let seen = stub.seen();
    assert!(seen.iter().any(|s| s.starts_with("DELETE")), "{seen:?}");
    assert!(!seen.iter().any(|s| s.starts_with("revoke")), "{seen:?}");
}

#[tokio::test]
async fn a_server_publishing_no_sign_in_or_refusing_is_said_and_never_asked() {
    for (set, level, text) in [
        (
            (|c: &mut crate::run::oauth::tests::Config| {
                c.resource = Json::Null;
                c.challenge_header = false;
            }) as fn(&mut crate::run::oauth::tests::Config),
            NoticeLevel::Error,
            "agent.toml's [[mcp]] docs: docs refused yobuild's request (HTTP 401) and offers no sign-in: it needs a credential, which agent.toml gives as a header in place of `auth = \"oauth\"` (",
        ),
        (
            |c| c.unsigned_status = Some(403),
            NoticeLevel::Error,
            "agent.toml's [[mcp]] docs: docs refused yobuild's request (HTTP 403): it does not let yobuild in without a credential (",
        ),
    ] {
        let stub = Stub::start().await;
        stub.set(set);
        let (loaded, seen) = signed(docs(&stub, &[]), Vec::new()).await;
        assert!(loaded.tools().await.is_empty());
        let seen = seen.lock().unwrap();
        assert!(seen.titles.is_empty(), "{:?}", seen.titles);
        assert!(
            matches!(seen.notices.as_slice(), [(l, n)] if *l == level && n.starts_with(text)),
            "{:?}",
            seen.notices
        );
    }
}

#[tokio::test]
async fn its_headers_go_beside_the_token_and_a_failure_after_revokes_unsaid() {
    let stub = Stub::start().await;
    stub.set(|c| c.signed_status = Some(403));
    let mut server = docs(&stub, &[]);
    server.headers = vec![ManifestHeader {
        name: "X-Org".into(),
        written: "${ORG}".into(),
        filled: "org-secret-12345".into(),
    }];
    server.hide = vec![("org-secret-12345".into(), "${ORG}".into())];
    let (loaded, seen) = signed(server, vec![json!("Allow"), json!(BROWSER)]).await;
    assert!(loaded.tools().await.is_empty());
    {
        let seen = seen.lock().unwrap();
        assert!(
            matches!(
                seen.notices.as_slice(),
                [(NoticeLevel::Error, n)] if n.starts_with(
                    "agent.toml's [[mcp]] docs: docs refused yobuild's request (HTTP 403): it does not take the credential or sign-in it was given ("
                ) && n.contains("${ORG}")
            ),
            "{:?}",
            seen.notices
        );
        for (_, notice) in &seen.notices {
            assert!(!notice.contains("org-secret"), "{notice}");
            assert!(!notice.contains(&access(1)), "{notice}");
        }
    }
    let seen = stub.seen();
    assert!(
        seen.iter()
            .any(|s| s.contains(&format!("auth=Bearer {}", access(1)))
                && s.ends_with("org=org-secret-12345")),
        "{seen:?}"
    );
    assert!(
        seen.contains(&format!("revoke refresh_token {}", refresh(1))),
        "{seen:?}"
    );
}

#[tokio::test]
async fn a_server_listing_no_tools_once_signed_in_is_closed_then_revoked() {
    let stub = Stub::start().await;
    stub.set(|c| c.tools = json!([]));
    let (loaded, seen) = signed(docs(&stub, &[]), vec![json!("Allow"), json!(BROWSER)]).await;
    assert!(loaded.tools().await.is_empty());
    assert!(
        matches!(
            seen.lock().unwrap().notices.as_slice(),
            [(NoticeLevel::Error, _)]
        ),
        "{:?}",
        seen.lock().unwrap().notices
    );
    let seen = stub.seen();
    let delete = seen.iter().position(|s| s.starts_with("DELETE"));
    let revoke = seen
        .iter()
        .position(|s| s.starts_with("revoke refresh_token"));
    assert!(
        matches!((delete, revoke), (Some(d), Some(r)) if d < r),
        "{seen:?}"
    );
    assert!(
        !seen.iter().any(|s| s.starts_with("token refresh")),
        "{seen:?}"
    );
}

#[tokio::test]
async fn a_refused_revocation_is_a_warning() {
    let stub = Stub::start().await;
    stub.set(|c| {
        c.tools = json!([]);
        c.revoke_status = 503;
    });
    let (_, seen) = signed(docs(&stub, &[]), vec![json!("Allow"), json!(BROWSER)]).await;
    noticed(&seen, 2).await;
    let seen = seen.lock().unwrap();
    assert!(
        seen.notices.iter().any(|(l, n)| *l == NoticeLevel::Warning
            && n.starts_with("agent.toml's [[mcp]] docs: ")
            && n.ends_with(": it may stay valid at its provider until it expires.")),
        "{:?}",
        seen.notices
    );
}

#[tokio::test]
async fn nothing_is_connected_until_a_frontend_is_there() {
    let stub = Stub::start().await;
    let session = session();
    let loaded = Loaded::default();
    let signing = signing(&session, &loaded, Duration::from_secs(10));
    let signs = tokio::spawn(sign_in_all(signing, vec![docs(&stub, &[])]));
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(stub.seen().is_empty(), "{:?}", stub.seen());
    // An observer is no frontend.
    let _observer = session.observe();
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(stub.seen().is_empty(), "{:?}", stub.seen());
    let seen = answering(&session, vec![json!("Allow"), json!(BROWSER)]);
    tokio::time::timeout(Duration::from_secs(10), signs)
        .await
        .unwrap()
        .unwrap();
    noticed(&seen, 1).await;
    assert_eq!(
        loaded.tools().await.len(),
        1,
        "{:?}",
        seen.lock().unwrap().notices
    );
    loaded.close().await;
}

/// Where a test stops the session.
#[derive(Debug, Clone, Copy)]
enum StopAt {
    /// While the owner is asked whether to sign in.
    Asked,
    /// While the browser's question is open.
    Browser,
    /// While another sign-in holds the lock.
    Waiting,
}

#[tokio::test]
async fn the_sessions_end_stops_a_sign_in_wherever_it_is() {
    for at in [StopAt::Asked, StopAt::Browser, StopAt::Waiting] {
        let stub = Stub::start().await;
        let session = session();
        let answers = match at {
            StopAt::Asked => vec![json!(SILENT)],
            StopAt::Browser => vec![json!("Allow"), json!(SILENT)],
            StopAt::Waiting => vec![json!("Allow")],
        };
        let seen = answering(&session, answers);
        let loaded = Loaded::default();
        let signing = signing(&session, &loaded, Duration::from_secs(30));
        let stop = signing.stop.clone();
        let never = CancellationToken::new();
        let held = match at {
            StopAt::Waiting => Some(loaded.one_sign_in(&never, || {}).await.unwrap()),
            _ => None,
        };
        let signs = tokio::spawn(sign_in_all(signing, vec![docs(&stub, &[])]));
        // Until it is where the test stops it.
        let wanted = match at {
            StopAt::Asked => 1,
            StopAt::Browser => 2,
            StopAt::Waiting => 1,
        };
        let mut there = false;
        for _ in 0..100 {
            {
                let seen = seen.lock().unwrap();
                let waiting = seen
                    .notices
                    .iter()
                    .any(|(_, n)| n.ends_with("its sign-in waits for another sign-in to end."));
                if seen.titles.len() >= wanted && (!matches!(at, StopAt::Waiting) || waiting) {
                    there = true;
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(there, "{at:?}: {:?}", seen.lock().unwrap().titles);
        stop.cancel();
        tokio::time::timeout(Duration::from_secs(5), signs)
            .await
            .unwrap_or_else(|_| panic!("{at:?}: ended within 5 s"))
            .unwrap();
        drop(held);
        noticed(&seen, if matches!(at, StopAt::Waiting) { 2 } else { 1 }).await;
        assert!(
            said(
                &seen,
                NoticeLevel::Warning,
                "agent.toml's [[mcp]] docs: not signed in: the session ended"
            ),
            "{at:?}: {:?}",
            seen.lock().unwrap().notices
        );
        assert!(loaded.tools().await.is_empty(), "{at:?}");
        assert!(
            !stub.seen().iter().any(|s| s.starts_with("token")),
            "{at:?}: {:?}",
            stub.seen()
        );
    }
}

#[tokio::test]
async fn a_sign_in_waits_for_another_before_its_browser_question() {
    let stub = Stub::start().await;
    let session = session();
    let seen = answering(&session, vec![json!("Allow"), json!(BROWSER)]);
    let loaded = Loaded::default();
    let held = loaded
        .one_sign_in(&CancellationToken::new(), || {})
        .await
        .unwrap();
    let signs = tokio::spawn(sign_in_all(
        signing(&session, &loaded, Duration::from_secs(10)),
        vec![docs(&stub, &[])],
    ));
    noticed(&seen, 1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    {
        let seen = seen.lock().unwrap();
        assert_eq!(
            seen.notices,
            [(
                NoticeLevel::Info,
                "agent.toml's [[mcp]] docs: its sign-in waits for another sign-in to end.".into()
            )]
        );
        // Only the owner's Allow: the browser's question waits.
        assert_eq!(seen.titles.len(), 1, "{:?}", seen.titles);
    }
    drop(held);
    tokio::time::timeout(Duration::from_secs(10), signs)
        .await
        .unwrap()
        .unwrap();
    noticed(&seen, 2).await;
    assert_eq!(
        loaded.tools().await.len(),
        1,
        "{:?}",
        seen.lock().unwrap().notices
    );
    loaded.close().await;
}

#[test]
fn a_notice_names_ten_tools_and_counts_the_rest() {
    let names: Vec<String> = (1..=12).map(|n| format!("t{n}")).collect();
    assert_eq!(
        super::listed(&names),
        "t1, t2, t3, t4, t5, t6, t7, t8, t9, t10, and 2 more"
    );
    assert_eq!(super::listed(&names[..2]), "t1, t2");
}
