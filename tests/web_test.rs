//! The browser frontend's server: the page, UI plugin modules, and the
//! protocol over a real WebSocket.

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value as Json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;
use yoagent::provider::mock::MockResponse;
use yoagent::provider::{MockProvider, ModelConfig};
use yoagent::Agent;
use yobuild::frontend::{web, Session, UiPlugin, UiRequest};

async fn get(addr: std::net::SocketAddr, path: &str) -> String {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    response
}

const WAIT: Duration = Duration::from_secs(10);

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn open(served: &web::Served) -> Socket {
    let (ws, _) =
        tokio_tungstenite::connect_async(format!("ws://{}/ws?t={}", served.addr(), served.token()))
            .await
            .unwrap();
    ws
}

async fn receive(ws: &mut Socket) -> Json {
    let frame = tokio::time::timeout(WAIT, ws.next())
        .await
        .expect("a frame in time")
        .unwrap()
        .unwrap();
    serde_json::from_str(frame.to_text().unwrap()).unwrap()
}

#[tokio::test]
async fn a_browser_runs_a_prompt_and_answers_a_question_over_the_websocket() {
    let (session, driver) = Session::new(false);
    let agent = Agent::from_provider(
        MockProvider::new(vec![MockResponse::Text("hi from the agent".into())]),
        ModelConfig::mock(),
    );
    tokio::spawn(driver.run(agent));
    let served = web::serve(session.clone(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let mut ws = open(&served).await;
    assert_eq!(receive(&mut ws).await["type"], "hello");

    ws.send(Message::Text(
        json!({"type": "prompt", "text": "hello"})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let mut text = String::new();
    loop {
        let message = receive(&mut ws).await;
        if message["type"] == "event" && message["event"]["type"] == "messageUpdate" {
            text.push_str(
                message["event"]["delta"]["delta"]
                    .as_str()
                    .unwrap_or_default(),
            );
        }
        if message["type"] == "runEnd" {
            assert_eq!(message["outcome"], "completed");
            assert!(message.get("totalCostUsd").is_some());
            break;
        }
    }
    assert_eq!(text, "hi from the agent");

    // A plugin's question reaches the browser; its answer comes back.
    let asking = tokio::spawn({
        let session = session.clone();
        async move {
            session
                .ask(
                    UiRequest::Select {
                        title: "Which?".into(),
                        message: String::new(),
                        options: vec!["a".into(), "b".into()],
                        multiple: false,
                    },
                    Duration::from_secs(10),
                )
                .await
        }
    });
    let request: Json = loop {
        let message = receive(&mut ws).await;
        if message["type"] == "uiRequest" {
            break message;
        }
    };
    assert_eq!(request["request"]["kind"], "select");
    ws.send(Message::Text(
        json!({"type": "uiResponse", "id": request["id"], "value": "b"})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    assert_eq!(asking.await.unwrap(), json!("b"));
    served.stop();
}

/// A frame that is not the protocol is answered with a notice, and the
/// connection carries on; closing the socket detaches the frontend.
#[tokio::test]
async fn a_malformed_frame_gets_a_notice_and_a_closed_socket_detaches() {
    let (session, _driver) = Session::new(false);
    let served = web::serve(session.clone(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let mut ws = open(&served).await;
    assert_eq!(receive(&mut ws).await["type"], "hello");
    assert_eq!(session.frontends(), 1);
    ws.send(Message::Text(r#"{"type": "dance"}"#.into()))
        .await
        .unwrap();
    let notice = receive(&mut ws).await;
    assert_eq!(notice["type"], "notice");
    assert_eq!(notice["level"], "warning");
    ws.close(None).await.unwrap();
    let deadline = tokio::time::Instant::now() + WAIT;
    while session.frontends() != 0 {
        assert!(tokio::time::Instant::now() < deadline, "still attached");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn the_page_and_ui_plugin_modules_are_served() {
    let (session, _driver) = Session::new(false);
    session.add_ui_plugin(
        UiPlugin {
            name: "links".into(),
            tools: vec!["search".into()],
            panel: false,
            version: 0,
        },
        "export function renderTool() {}",
    );
    let served = web::serve(session, "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    assert!(served.url().ends_with(&format!("/?t={}", served.token())));
    let addr = served.addr();
    assert!(get(addr, "/").await.contains("<title>yobuild</title>"));
    assert!(get(addr, "/app.js").await.contains("text/javascript"));
    assert!(get(addr, "/lib.js").await.contains("markdownParts"));
    let module = get(addr, "/ui-plugins/links.js").await;
    assert!(module.contains("200 OK") && module.contains("renderTool"));
    assert!(get(addr, "/ui-plugins/nope.js").await.contains("404"));
    served.stop();
}

/// Browsers do not apply cross-origin rules to WebSockets: any page could
/// connect. Without the server's token, `/ws` refuses.
#[tokio::test]
async fn the_websocket_refuses_a_missing_or_wrong_token() {
    let (session, _driver) = Session::new(false);
    let served = web::serve(session, "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    for query in ["", "?t=", "?t=guess", "?x=1"] {
        let result =
            tokio_tungstenite::connect_async(format!("ws://{}/ws{query}", served.addr())).await;
        match result {
            Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                assert_eq!(response.status(), 403, "with {query:?}")
            }
            other => panic!("with {query:?}: {:?}", other.map(|_| ())),
        }
    }
    open(&served).await;
    served.stop();
}

/// The past runs (design §16): the owner's own tasks and answers, so the
/// token is required, as `/ws`'s; read fresh each time; none without a log.
#[tokio::test]
async fn past_runs_need_the_token_and_are_read_when_asked() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (session, _driver) = Session::new(false);
    let asked = std::sync::Arc::new(AtomicUsize::new(0));
    let runs: web::PastRuns = {
        let asked = asked.clone();
        std::sync::Arc::new(move || {
            let n = asked.fetch_add(1, Ordering::SeqCst) + 1;
            json!({ "runs": [{ "run": n }] })
        })
    };
    let served = web::serve_with(
        session.clone(),
        "127.0.0.1:0".parse().unwrap(),
        Some(runs),
        Some("<notes> & 'co'"),
    )
    .await
    .unwrap();
    let addr = served.addr();
    // The page names yobuild and the agent, its name escaped.
    let page = get(addr, "/").await;
    assert!(
        page.contains("<title>&lt;notes&gt; &amp; &#39;co&#39; · yobuild</title>")
            && page.contains("<b>yobuild</b><span>&lt;notes&gt; &amp; &#39;co&#39; · this page")
            && !page.contains("yoagent"),
        "{page}"
    );
    for query in ["", "?t=", "?t=guess"] {
        let response = get(addr, &format!("/runs{query}")).await;
        assert!(
            response.starts_with("HTTP/1.1 403"),
            "with {query:?}: {response}"
        );
    }
    assert_eq!(asked.load(Ordering::SeqCst), 0);
    for n in 1..=2 {
        let response = get(addr, &format!("/runs?t={}", served.token())).await;
        assert!(
            response.starts_with("HTTP/1.1 200")
                && response.contains("content-type: application/json")
                && response.contains("cache-control: no-store")
                && response.ends_with(&format!(r#"{{"runs":[{{"run":{n}}}]}}"#)),
            "{response}"
        );
    }
    served.stop();
    // `serve`: no log, none.
    let served = web::serve(session, "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let response = get(served.addr(), &format!("/runs?t={}", served.token())).await;
    assert!(response.starts_with("HTTP/1.1 404"), "{response}");
}
