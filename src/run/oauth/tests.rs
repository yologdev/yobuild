use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::{json, Value as Json};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use url::Url;
use yoagent::mcp::{AuthChallenge, AuthorizationSource, HttpTransport, McpClient, Redirects};

use super::*;
use crate::frontend::{ClientMessage, Connection, ResolveReason, ServerMessage, Session};

const WAIT: Duration = Duration::from_secs(10);

/// How the stub answers; the tests change it as they go.
pub(crate) struct Config {
    /// The resource metadata (`null`: 404).
    pub(crate) resource: Json,
    /// The authorization server's metadata (`null`: 404).
    pub(crate) authorization: Json,
    /// The registration's answer and status.
    pub(crate) registration: (u16, Json),
    /// The `iss` `/authorize` sends back (`None`: none).
    pub(crate) iss: Option<String>,
    /// `/authorize` sends the browser back with this error instead.
    pub(crate) error: Option<(String, String)>,
    /// The token endpoint's answer to the code instead of tokens.
    pub(crate) token_refusal: Option<(u16, String)>,
    /// Refresh refused.
    pub(crate) refresh_refused: bool,
    /// The access token the MCP server takes now.
    pub(crate) valid: String,
    /// The refresh token the token endpoint takes now.
    pub(crate) refresh: String,
    /// Tokens issued so far.
    pub(crate) issued: u32,
    /// PKCE's challenge, as `/authorize` got it.
    pub(crate) challenge: String,
    /// The resource metadata's status when not 200 (302: to `/elsewhere`).
    pub(crate) resource_status: u16,
    /// The resource metadata at the root's well-known address (`null`: 404).
    pub(crate) root_resource: Json,
    /// OpenID Connect discovery's metadata (`null`: 404).
    pub(crate) openid: Json,
    /// The registration and the token endpoint wait this long first.
    pub(crate) slow: Duration,
    /// A refresh rotates the refresh token.
    pub(crate) rotate: bool,
    /// A refresh answered with this status instead (a passing trouble).
    pub(crate) refresh_status: Option<u16>,
    /// The revocation endpoint's status.
    pub(crate) revoke_status: u16,
    /// The MCP server lets `initialize` through unsigned and asks for the
    /// sign-in at `tools/list`.
    pub(crate) challenge_at_list: bool,
    /// The MCP server's 401 carries its `WWW-Authenticate` challenge.
    pub(crate) challenge_header: bool,
    /// The resource metadata waits this long first.
    pub(crate) slow_metadata: Duration,
}

/// An MCP server and its authorization server on this machine, recording
/// what they get.
pub(crate) struct Stub {
    pub(crate) base: String,
    seen: Arc<Mutex<Vec<String>>>,
    config: Arc<Mutex<Config>>,
}

const CODE: &str = "the-authorization-code-0123456789";

pub(crate) fn access(n: u32) -> String {
    format!("access-token-{n}-abcdefghijklmnop")
}
pub(crate) fn refresh(n: u32) -> String {
    format!("refresh-token-{n}-qrstuvwxyz012345")
}

fn form(body: &str) -> std::collections::HashMap<String, String> {
    url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect()
}

impl Stub {
    pub(crate) async fn start() -> Stub {
        use axum::http::{HeaderMap, Method, StatusCode};
        use axum::response::IntoResponse;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let config = Arc::new(Mutex::new(Config {
            resource: json!({
                "resource": format!("{base}/mcp"),
                "authorization_servers": [base],
                "scopes_supported": ["read", "write"],
            }),
            authorization: json!({
                "issuer": base,
                "authorization_endpoint": format!("{base}/authorize"),
                "token_endpoint": format!("{base}/token"),
                "registration_endpoint": format!("{base}/register"),
                "revocation_endpoint": format!("{base}/revoke"),
                "code_challenge_methods_supported": ["S256"],
                "authorization_response_iss_parameter_supported": true,
            }),
            registration: (
                201,
                json!({ "client_id": "client-1", "client_secret": "dropped-secret" }),
            ),
            iss: Some(base.clone()),
            error: None,
            token_refusal: None,
            refresh_refused: false,
            valid: String::new(),
            refresh: String::new(),
            issued: 0,
            challenge: String::new(),
            resource_status: 200,
            root_resource: Json::Null,
            openid: Json::Null,
            slow: Duration::ZERO,
            rotate: true,
            refresh_status: None,
            revoke_status: 200,
            challenge_at_list: false,
            challenge_header: true,
            slow_metadata: Duration::ZERO,
        }));
        let challenge = format!(
            "Bearer realm=\"OAuth\", resource_metadata=\"{base}/.well-known/oauth-protected-resource/mcp\", scope=\"read write\""
        );
        let (c, s) = (config.clone(), seen.clone());
        let resource = move || {
            let c = c.clone();
            let s = s.clone();
            async move {
                let slow = c.lock().unwrap().slow_metadata;
                tokio::time::sleep(slow).await;
                s.lock().unwrap().push("resource metadata".into());
                let (json, status) = {
                    let c = c.lock().unwrap();
                    (c.resource.clone(), c.resource_status)
                };
                if status == 302 {
                    return (StatusCode::FOUND, [("location", "/elsewhere")]).into_response();
                }
                if status != 200 {
                    return StatusCode::from_u16(status).unwrap().into_response();
                }
                if json.is_null() {
                    return StatusCode::NOT_FOUND.into_response();
                }
                if json == json!("not json") {
                    return "{not json: the-authorization-code".into_response();
                }
                answer(json).into_response()
            }
        };
        let (c, s) = (config.clone(), seen.clone());
        let authorization = move || {
            let c = c.clone();
            let s = s.clone();
            async move {
                s.lock().unwrap().push("authorization metadata".into());
                let json = c.lock().unwrap().authorization.clone();
                if json.is_null() {
                    return StatusCode::NOT_FOUND.into_response();
                }
                answer(json).into_response()
            }
        };
        let (c, s) = (config.clone(), seen.clone());
        let root = move || {
            let c = c.clone();
            let s = s.clone();
            async move {
                s.lock().unwrap().push("root resource metadata".into());
                let json = c.lock().unwrap().root_resource.clone();
                if json.is_null() {
                    return StatusCode::NOT_FOUND.into_response();
                }
                answer(json).into_response()
            }
        };
        let (c, s) = (config.clone(), seen.clone());
        let openid = move || {
            let c = c.clone();
            let s = s.clone();
            async move {
                s.lock().unwrap().push("openid metadata".into());
                let json = c.lock().unwrap().openid.clone();
                if json.is_null() {
                    return StatusCode::NOT_FOUND.into_response();
                }
                answer(json).into_response()
            }
        };
        let (c, s) = (config.clone(), seen.clone());
        let register = move |body: String| {
            let c = c.clone();
            let s = s.clone();
            async move {
                let slow = c.lock().unwrap().slow;
                tokio::time::sleep(slow).await;
                let request: Json = serde_json::from_str(&body).unwrap();
                s.lock().unwrap().push(format!(
                    "register auth={} redirect={}",
                    request["token_endpoint_auth_method"].as_str().unwrap(),
                    request["redirect_uris"][0]
                        .as_str()
                        .unwrap()
                        .starts_with("http://127.0.0.1:")
                ));
                let (status, mut registered) = c.lock().unwrap().registration.clone();
                if registered.get("redirect_uris") == Some(&json!("echo")) {
                    registered["redirect_uris"] = request["redirect_uris"].clone();
                }
                (StatusCode::from_u16(status).unwrap(), answer(registered)).into_response()
            }
        };
        let (c, s) = (config.clone(), seen.clone());
        let authorize = move |query: axum::extract::RawQuery| {
            let c = c.clone();
            let s = s.clone();
            async move {
                let q = form(query.0.as_deref().unwrap_or(""));
                s.lock().unwrap().push(format!(
                    "authorize method={} resource={} scope={} client={}",
                    q["code_challenge_method"],
                    q["resource"],
                    q.get("scope").map(String::as_str).unwrap_or("-"),
                    q["client_id"]
                ));
                let mut config = c.lock().unwrap();
                config.challenge = q["code_challenge"].clone();
                let mut back = Url::parse(&q["redirect_uri"]).unwrap();
                {
                    let mut pairs = back.query_pairs_mut();
                    match &config.error {
                        Some((error, description)) => {
                            pairs
                                .append_pair("error", error)
                                .append_pair("error_description", description);
                        }
                        None => {
                            pairs.append_pair("code", CODE);
                        }
                    }
                    pairs.append_pair("state", &q["state"]);
                    if let Some(iss) = &config.iss {
                        pairs.append_pair("iss", iss);
                    }
                }
                (StatusCode::FOUND, [("location", back.to_string())]).into_response()
            }
        };
        let (c, s) = (config.clone(), seen.clone());
        let mcp_base = base.clone();
        let token = move |body: String| {
            let c = c.clone();
            let s = s.clone();
            let base = mcp_base.clone();
            async move {
                let slow = c.lock().unwrap().slow;
                tokio::time::sleep(slow).await;
                let f = form(&body);
                let mut config = c.lock().unwrap();
                let grant = f["grant_type"].as_str();
                // Recorded, not asserted here: a test reads it.
                let resource =
                    if f["resource"] == format!("{base}/mcp") && f["client_id"] == "client-1" {
                        "yes"
                    } else {
                        "no"
                    };
                if grant == "authorization_code" {
                    let verified = URL_SAFE_NO_PAD
                        .encode(Sha256::digest(f["code_verifier"].as_bytes()))
                        == config.challenge;
                    s.lock().unwrap().push(format!(
                        "token code={} verifier={verified} resource={resource}",
                        f["code"] == CODE
                    ));
                    if let Some((status, body)) = config.token_refusal.clone() {
                        return (StatusCode::from_u16(status).unwrap(), body).into_response();
                    }
                } else {
                    let current = f["refresh_token"] == config.refresh;
                    s.lock().unwrap().push(format!(
                        "token refresh current={current} resource={resource}"
                    ));
                    if let Some(status) = config.refresh_status {
                        return StatusCode::from_u16(status).unwrap().into_response();
                    }
                    if config.refresh_refused || !current {
                        return (
                            StatusCode::BAD_REQUEST,
                            answer(json!({ "error": "invalid_grant", "error_description": format!("refused {}", f["refresh_token"]) })),
                        )
                            .into_response();
                    }
                }
                config.issued += 1;
                config.valid = access(config.issued);
                let rotate = grant == "authorization_code" || config.rotate;
                if rotate {
                    config.refresh = refresh(config.issued);
                }
                let mut tokens = json!({
                    "token_type": "bearer",
                    "access_token": config.valid,
                    "expires_in": 3600,
                });
                if rotate {
                    tokens["refresh_token"] = json!(config.refresh);
                }
                answer(tokens).into_response()
            }
        };
        let (c, s) = (config.clone(), seen.clone());
        let revoke = move |body: String| {
            let c = c.clone();
            let s = s.clone();
            async move {
                let f = form(&body);
                s.lock()
                    .unwrap()
                    .push(format!("revoke {} {}", f["token_type_hint"], f["token"]));
                StatusCode::from_u16(c.lock().unwrap().revoke_status).unwrap()
            }
        };
        let (c, s) = (config.clone(), seen.clone());
        let mcp = move |method: Method, headers: HeaderMap, body: String| {
            let c = c.clone();
            let s = s.clone();
            let challenge = challenge.clone();
            async move {
                let request: Json = serde_json::from_str(&body).unwrap_or(Json::Null);
                let auth = headers
                    .get("authorization")
                    .and_then(|a| a.to_str().ok())
                    .unwrap_or("-")
                    .to_owned();
                let session = headers
                    .get("mcp-session-id")
                    .and_then(|a| a.to_str().ok())
                    .unwrap_or("-")
                    .to_owned();
                let what = request["method"].as_str().unwrap_or("-").to_owned();
                // A header besides the token, when one is sent.
                let org = headers
                    .get("x-org")
                    .and_then(|a| a.to_str().ok())
                    .map(|o| format!(" org={o}"))
                    .unwrap_or_default();
                s.lock().unwrap().push(format!(
                    "{method} {what} auth={auth} session={session}{org}"
                ));
                let (valid, at_list, with_challenge) = {
                    let c = c.lock().unwrap();
                    (c.valid.clone(), c.challenge_at_list, c.challenge_header)
                };
                let unsigned_through = at_list
                    && auth == "-"
                    && (what == "initialize" || what.starts_with("notifications/"));
                if auth != format!("Bearer {valid}") && !unsigned_through {
                    let mut refused = (StatusCode::UNAUTHORIZED, "sign in").into_response();
                    if with_challenge {
                        refused.headers_mut().insert(
                            "www-authenticate",
                            axum::http::HeaderValue::from_str(&challenge).unwrap(),
                        );
                    }
                    return refused;
                }
                if method == Method::DELETE {
                    return StatusCode::OK.into_response();
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
                    "tools/list" => {
                        json!({ "tools": [{ "name": "echo", "inputSchema": { "type": "object" } }] })
                    }
                    _ => json!({ "content": [{ "type": "text", "text": "echoed" }] }),
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
        };
        let app = axum::Router::new()
            .route(
                "/.well-known/oauth-protected-resource/mcp",
                axum::routing::get(resource),
            )
            .route(
                "/.well-known/oauth-authorization-server",
                axum::routing::get(authorization),
            )
            .route(
                "/.well-known/oauth-protected-resource",
                axum::routing::get(root),
            )
            .route(
                "/.well-known/openid-configuration",
                axum::routing::get(openid),
            )
            .route("/register", axum::routing::post(register))
            .route("/authorize", axum::routing::get(authorize))
            .route("/token", axum::routing::post(token))
            .route("/revoke", axum::routing::post(revoke))
            .route("/mcp", axum::routing::any(mcp));
        tokio::spawn(async move { axum::serve(listener, app).await });
        Stub { base, seen, config }
    }

    pub(crate) fn server(&self) -> Url {
        Url::parse(&format!("{}/mcp", self.base)).unwrap()
    }

    fn challenge(&self) -> String {
        format!(
            "Bearer realm=\"OAuth\", resource_metadata=\"{}/.well-known/oauth-protected-resource/mcp\", scope=\"read write\"",
            self.base
        )
    }

    pub(crate) fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }

    pub(crate) fn set(&self, change: impl FnOnce(&mut Config)) {
        change(&mut self.config.lock().unwrap());
    }

    async fn found(&self) -> Found {
        discover(
            &Net::loopback(),
            &self.server(),
            Some(&self.challenge()),
            Site::Own,
        )
        .await
        .unwrap()
    }
}

/// A browser's GET: the status and where it is sent on.
pub(crate) async fn get(url: &str) -> Result<(u16, Option<String>), reqwest::Error> {
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let response = http.get(url).send().await?;
    let location = response
        .headers()
        .get("location")
        .and_then(|l| l.to_str().ok())
        .map(str::to_owned);
    Ok((response.status().as_u16(), location))
}

/// From the start link to the sign-in page and back: the callback's URL
/// the authorization server sends the browser to.
pub(crate) async fn to_callback(start: &str) -> String {
    let (status, authorize) = get(start).await.unwrap();
    assert_eq!(status, 302);
    let (status, back) = get(&authorize.unwrap()).await.unwrap();
    assert_eq!(status, 302);
    back.unwrap()
}

/// The next sign-in question a frontend gets.
async fn question(ui: &mut Connection) -> (u64, UiRequest) {
    loop {
        let message = tokio::time::timeout(WAIT, ui.messages.recv())
            .await
            .unwrap()
            .unwrap();
        if let ServerMessage::UiRequest { id, request } = message {
            return (id, request);
        }
    }
}

/// How the next question closed.
async fn resolved(ui: &mut Connection) -> ResolveReason {
    loop {
        let message = tokio::time::timeout(WAIT, ui.messages.recv())
            .await
            .unwrap()
            .unwrap();
        if let ServerMessage::UiResolved { reason, .. } = message {
            return reason;
        }
    }
}

fn link(request: &UiRequest) -> String {
    match request {
        UiRequest::SignIn { url, .. } => url.clone().unwrap(),
        other => panic!("not a sign-in: {other:?}"),
    }
}

/// Signs in on a task: the session, its frontend and the result to come.
async fn signing_in(
    stub: &Stub,
    timeout: Duration,
) -> (
    Session,
    Connection,
    CancellationToken,
    tokio::task::JoinHandle<Result<Arc<Source>, Refusal>>,
) {
    let (session, driver) = Session::new(true);
    std::mem::forget(driver);
    let mut ui = session.connect();
    // Its `Hello`.
    ui.messages.recv().await.unwrap();
    let found = stub.found().await;
    let stop = CancellationToken::new();
    let task = {
        let (session, stop) = (session.clone(), stop.clone());
        tokio::spawn(async move {
            let ask = Ask {
                session: &session,
                title: "Sign in to io.x/docs (127.0.0.1) at 127.0.0.1?".into(),
                timeout,
                printed: None,
            };
            sign_in(&Net::loopback(), &found, ask, &stop).await
        })
    };
    (session, ui, stop, task)
}

#[test]
fn a_challenge_is_read_into_its_schemes_and_parameters() {
    let c = Challenge::parse(
        r#"Basic realm="x", Bearer realm="OAuth", resource_metadata="https://h/.well-known/x", scope="read write", error=invalid_token"#,
    );
    assert_eq!(
        c.param("resource_metadata"),
        Some("https://h/.well-known/x")
    );
    assert_eq!(c.param("scope"), Some("read write"));
    assert_eq!(c.param("error"), Some("invalid_token"));
    assert_eq!(
        c.param("realm"),
        Some("OAuth"),
        "the Bearer's own, not Basic's"
    );
    let quoted = Challenge::parse(r#"bearer scope="a \"b\"""#);
    assert_eq!(quoted.param("scope"), Some(r#"a "b""#));
    assert!(Challenge::parse("Basic realm=\"x\"").bearer().is_none());
    assert!(Challenge::parse("").bearer().is_none());
    assert!(Challenge::parse("Bearer").bearer().unwrap().is_empty());
}

#[test]
fn the_well_known_addresses_are_the_specifications() {
    let at = |u: &str| Url::parse(u).unwrap();
    let urls = |v: Vec<Url>| v.into_iter().map(|u| u.to_string()).collect::<Vec<_>>();
    assert_eq!(
        urls(well_known_resource(&at("https://mcp.x.example/v1/mcp?a=1"))),
        [
            "https://mcp.x.example/.well-known/oauth-protected-resource/v1/mcp",
            "https://mcp.x.example/.well-known/oauth-protected-resource"
        ]
    );
    assert_eq!(
        urls(well_known_authorization(&at("https://auth.x.example"))),
        [
            "https://auth.x.example/.well-known/oauth-authorization-server",
            "https://auth.x.example/.well-known/openid-configuration"
        ]
    );
    assert_eq!(
        urls(well_known_authorization(&at(
            "https://auth.x.example/tenant1"
        ))),
        [
            "https://auth.x.example/.well-known/oauth-authorization-server/tenant1",
            "https://auth.x.example/.well-known/openid-configuration/tenant1",
            "https://auth.x.example/tenant1/.well-known/openid-configuration"
        ]
    );
}

#[tokio::test]
async fn real_requests_refuse_local_names_and_addresses() {
    let net = Net::checked().unwrap();
    for refused in [
        "http://auth.example.com/", "https://localhost/", "https://10.0.0.1/", "https://[::1]/",
        "https://intranet/", "https://printer.local/", "https://user@auth.example.com/",
    ] {
        assert!(net.address(refused).is_err(), "{refused}");
    }
    assert!(net.address("https://auth.example.com/authorize").is_ok());
    // A name pointing inside the network is refused when resolved.
    let resolved = reqwest::dns::Resolve::resolve(
        &PublicOnly::with_proxies(std::iter::empty()),
        "localhost".parse().unwrap(),
    )
    .await;
    let error = resolved.err().unwrap();
    assert_eq!(
        error.to_string(),
        "localhost resolves to an address on this machine or its network"
    );
    for (ip, is) in [
        ("8.8.8.8", true),
        ("127.0.0.1", false),
        ("10.1.2.3", false),
        ("172.16.0.1", false),
        ("192.168.1.1", false),
        ("169.254.169.254", false),
        ("100.64.0.1", false),
        ("0.0.0.0", false),
        ("2606:4700::1111", true),
        ("::1", false),
        ("fd00::1", false),
        ("fe80::1", false),
        ("::ffff:10.0.0.1", false),
        ("::10.0.0.1", false),
        ("2002:a00:1::1", false),
        ("2002:808:808::1", true),
        ("2001:0:4136::1", false),
        ("fec0::1", false),
        ("100::1", false),
        ("64:ff9b:1::1", false),
        ("64:ff9b::808:808", true),
        ("64:ff9b::a00:1", false),
    ] {
        assert_eq!(public(ip.parse().unwrap()), is, "{ip}");
    }
    assert_eq!(registrable("mcp.linear.app"), registrable("linear.app"));
    assert_eq!(registrable("MCP.Notion.com."), Some("notion.com".into()));
    assert_ne!(registrable("a.github.io"), registrable("b.github.io"));
    assert_eq!(registrable("auth.x.co.uk"), Some("x.co.uk".into()));
    assert_eq!(registrable("github.io"), None, "a public suffix is no site");
}

#[tokio::test]
async fn discovery_finds_the_sign_in_from_the_challenge_or_the_well_known_address() {
    let stub = Stub::start().await;
    let found = stub.found().await;
    assert_eq!(found.scopes(), ["read", "write"]);
    assert_eq!(found.issuer(), stub.base);
    assert_eq!(found.server(), &stub.server());
    assert!(found.iss_required);
    let details = found.details();
    assert!(details.contains("The sign-in page: 127.0.0.1"), "{details}");
    assert!(details.contains("Scopes asked: read write"), "{details}");
    assert!(details.contains("kept for this session only"), "{details}");
    assert!(!details.contains("Its issuer"), "{details}");

    // No challenge: the well-known address, the resource's scopes.
    stub.set(|c| c.resource["scopes_supported"] = json!(["offline_access", "docs"]));
    let found = discover(&Net::loopback(), &stub.server(), None, Site::Own)
        .await
        .unwrap();
    assert_eq!(found.scopes(), ["offline_access", "docs"]);
    assert!(found.details().contains("Broad: offline_access"));
}

#[tokio::test]
async fn discovery_refuses_before_anyone_is_asked() {
    let stub = Stub::start().await;
    let base = stub.base.clone();
    let elsewhere = base.replace("127.0.0.1", "localhost");
    let refused = |stub: &Stub, challenge: Option<String>, site: Site| {
        let server = stub.server();
        async move {
            discover(&Net::loopback(), &server, challenge.as_deref(), site)
                .await
                .unwrap_err()
        }
    };
    let challenge = Some(stub.challenge());

    // Metadata on another origin.
    let r = refused(
        &stub,
        Some(format!(
            "Bearer resource_metadata=\"{elsewhere}/.well-known/oauth-protected-resource/mcp\""
        )),
        Site::Any,
    )
    .await;
    assert_eq!(r.why, Why::Discovery);
    assert!(r.said.contains("another origin (localhost)"), "{}", r.said);

    type Change = Box<dyn Fn(&mut Config)>;
    let cases: Vec<(Change, Why, &str)> = vec![
        (
            Box::new(|c| c.resource = Json::Null),
            Why::NoSignIn,
            "no resource metadata",
        ),
        (
            Box::new(|c| c.resource = json!("not json")),
            Why::Discovery,
            "its answer is not JSON",
        ),
        (
            Box::new(move |c| c.resource["resource"] = json!(format!("{}/mcp/", c.issuer_of()))),
            Why::Discovery,
            "another resource",
        ),
        (
            Box::new(|c| c.resource["authorization_servers"] = json!(["https://auth.example.com"])),
            Why::Discovery,
            "no authorization server yobuild takes",
        ),
        (
            Box::new(|c| c.authorization = Json::Null),
            Why::Discovery,
            "publishes no metadata",
        ),
        (
            Box::new(|c| c.authorization["issuer"] = json!("http://127.0.0.1:1")),
            Why::Discovery,
            "another issuer",
        ),
        (
            Box::new(|c| c.authorization["code_challenge_methods_supported"] = json!(["plain"])),
            Why::NoPkce,
            "offers no PKCE",
        ),
        (
            Box::new(|c| {
                c.authorization["token_endpoint"] = json!("https://auth.example.com/token")
            }),
            Why::Discovery,
            "its `token_endpoint`: an address refused",
        ),
        (
            Box::new(|c| {
                c.authorization
                    .as_object_mut()
                    .unwrap()
                    .remove("registration_endpoint");
            }),
            Why::NoRegistration,
            "no way for yobuild to register",
        ),
        (
            Box::new(|c| {
                c.resource["scopes_supported"] =
                    json!((0..21).map(|n| format!("s{n}")).collect::<Vec<_>>())
            }),
            Why::Discovery,
            "21 scopes, more than 20",
        ),
        (
            Box::new(|c| c.resource["scopes_supported"] = json!(["a\"b"])),
            Why::Discovery,
            "a scope that is not one",
        ),
        (
            Box::new(|c| c.resource["scopes_supported"] = json!(["x".repeat(65)])),
            Why::Discovery,
            "a scope that is not one",
        ),
        (
            Box::new(|c| c.resource["scopes_supported"] = json!([""])),
            Why::Discovery,
            "a scope that is not one",
        ),
        (
            Box::new(|c| c.resource["padding"] = json!("x".repeat(70 * 1024))),
            Why::Discovery,
            "its answer is past 64 KiB",
        ),
        (
            Box::new(|c| c.resource_status = 500),
            Why::Discovery,
            "its resource metadata: it answered HTTP 500",
        ),
        (
            Box::new(|c| c.resource_status = 302),
            Why::Discovery,
            "a redirect (HTTP 302), which is not followed",
        ),
        (
            Box::new(|c| {
                c.authorization["token_endpoint"] =
                    json!(c.issuer_of().replace("127.0.0.1", "localhost") + "/token")
            }),
            Why::OtherSite,
            "its sign-in's `token_endpoint` is at another site (localhost)",
        ),
    ];
    for (change, why, said) in cases {
        let (resource, authorization) = {
            let c = stub.config.lock().unwrap();
            (c.resource.clone(), c.authorization.clone())
        };
        stub.set(|c| change(c));
        let no_challenge = refused(&stub, None, Site::Own).await;
        assert_eq!(no_challenge.why, why, "{said}: {}", no_challenge.said);
        assert!(
            no_challenge.said.contains(said),
            "{said}: {}",
            no_challenge.said
        );
        assert!(!no_challenge.said.contains("the-authorization-code"));
        stub.set(|c| {
            c.resource = resource;
            c.authorization = authorization;
            c.resource_status = 200;
        });
    }

    // Where the challenge points, the metadata must be.
    stub.set(|c| c.resource_status = 404);
    let r = refused(&stub, challenge.clone(), Site::Any).await;
    assert!(
        r.said
            .contains("where its challenge points: it answered HTTP 404"),
        "{}",
        r.said
    );
    stub.set(|c| c.resource_status = 200);

    // An authorization server at another site: refused for a server the
    // agent found, taken for one the owner wrote.
    stub.set(|c| {
        c.resource["authorization_servers"] = json!([elsewhere]);
        c.authorization["issuer"] = json!(elsewhere);
    });
    let r = refused(&stub, challenge.clone(), Site::Own).await;
    assert_eq!(r.why, Why::OtherSite);
    assert!(r.said.contains("another site (localhost)"), "{}", r.said);
    let found = discover(
        &Net::loopback(),
        &stub.server(),
        challenge.as_deref(),
        Site::Any,
    )
    .await
    .unwrap();
    assert!(found.details().contains("The sign-in page: 127.0.0.1"));
    assert!(found
        .details()
        .contains("Its issuer: localhost (not the sign-in page's host)"));
    // Nothing was registered or asked for.
    assert!(!stub
        .seen()
        .iter()
        .any(|s| s.starts_with("register") || s.starts_with("authorize")));
}

impl Config {
    fn issuer_of(&self) -> String {
        self.authorization["issuer"].as_str().unwrap().to_owned()
    }
}

#[tokio::test]
async fn a_registration_is_checked_and_its_secret_dropped() {
    let stub = Stub::start().await;
    let found = stub.found().await;
    let net = Net::loopback();
    let redirect = "http://127.0.0.1:9/callback/x";
    // A secret handed out is dropped; the client is a public one.
    assert_eq!(register(&net, &found, redirect).await.unwrap(), "client-1");
    assert_eq!(
        stub.seen().last().unwrap(),
        "register auth=none redirect=true"
    );
    stub.set(|c| c.registration.1["redirect_uris"] = json!("echo"));
    assert_eq!(register(&net, &found, redirect).await.unwrap(), "client-1");
    for (answer, status, said) in [
        (
            json!({ "client_id": "c", "token_endpoint_auth_method": "client_secret_basic" }),
            201,
            "must authenticate (`client_secret_basic`)",
        ),
        (
            json!({ "client_id": "c", "redirect_uris": ["http://127.0.0.1:9/other"] }),
            201,
            "other `redirect_uris`",
        ),
        (
            json!({ "client_id": "c\nd" }),
            201,
            "not one (printable ASCII",
        ),
        (
            json!({ "client_id": 7 }),
            201,
            "its `client_id` is not text",
        ),
        (json!({}), 201, "gave no `client_id`"),
        (
            json!({ "error": "invalid_client_metadata", "error_description": "the\u{202e} redirect\nis bad" }),
            400,
            "refused yobuild (HTTP 400: invalid_client_metadata — the redirect is bad)",
        ),
    ] {
        stub.set(|c| c.registration = (status, answer));
        let r = register(&net, &found, redirect).await.unwrap_err();
        assert_eq!(r.why, Why::Registration);
        assert!(r.said.contains(said), "{said}: {}", r.said);
    }
}

#[tokio::test]
async fn signed_in_from_the_page_the_token_reaches_the_server_and_is_refreshed_and_revoked() {
    let stub = Stub::start().await;
    let (_session, mut ui, _stop, task) = signing_in(&stub, WAIT).await;
    let (_, request) = question(&mut ui).await;
    let UiRequest::SignIn {
        title,
        message,
        host,
        url,
    } = &request
    else {
        panic!("{request:?}");
    };
    assert_eq!(title, "Sign in to io.x/docs (127.0.0.1) at 127.0.0.1?");
    assert_eq!(host, "127.0.0.1");
    assert!(message.contains("Scopes asked: read write"));
    let start = url.clone().unwrap();
    assert!(
        start.starts_with("http://127.0.0.1:") && start.contains("/start/"),
        "{start}"
    );
    let back = to_callback(&start).await;
    let (status, _) = get(&back).await.unwrap();
    assert_eq!(status, 200);
    let source = tokio::time::timeout(WAIT, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    // The question is withdrawn: the browser came back.
    assert_eq!(resolved(&mut ui).await, ResolveReason::Withdrawn);
    // The listener closed after it.
    let gone = async {
        while get(&back).await.is_ok() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(WAIT, gone).await.unwrap();

    let seen = stub.seen();
    assert!(
        seen.contains(&"register auth=none redirect=true".to_owned()),
        "{seen:?}"
    );
    let resource = format!("{}/mcp", stub.base);
    assert!(
        seen.contains(&format!(
            "authorize method=S256 resource={resource} scope=read write client=client-1"
        )),
        "{seen:?}"
    );
    assert!(
        seen.contains(&"token code=true verifier=true resource=yes".to_owned()),
        "{seen:?}"
    );

    // The token on every request, through yoagent's source.
    let transport = HttpTransport::new(stub.server().as_str())
        .and_then(|t| t.with_redirects(Redirects::None))
        .unwrap()
        .with_authorization(source.clone())
        .unwrap();
    let mut client = McpClient::from_transport(Box::new(transport));
    client.initialize().await.unwrap();
    assert_eq!(client.list_tools().await.unwrap().len(), 1);
    assert!(stub.seen().contains(&format!(
        "POST tools/list auth=Bearer {} session=s-1",
        access(1)
    )));

    // Expired: refreshed, and the call sent again in the same session.
    stub.set(|c| c.valid = "expired".into());
    let before = stub.seen().len();
    client.call_tool("echo", json!({})).await.unwrap();
    let after: Vec<String> = stub.seen()[before..].to_vec();
    assert_eq!(
        after,
        [
            format!("POST tools/call auth=Bearer {} session=s-1", access(1)),
            "token refresh current=true resource=yes".to_owned(),
            format!("POST tools/call auth=Bearer {} session=s-1", access(2)),
        ]
    );

    // Two calls refused at once: refreshed once.
    stub.set(|c| c.valid = "expired".into());
    let before = stub.seen().len();
    let (a, b) = tokio::join!(
        client.call_tool("echo", json!({})),
        client.call_tool("echo", json!({}))
    );
    a.unwrap();
    b.unwrap();
    let refreshes = stub.seen()[before..]
        .iter()
        .filter(|s| s.starts_with("token refresh"))
        .count();
    assert_eq!(refreshes, 1, "{:?}", &stub.seen()[before..]);

    // Every token handed out is hidden as `${oauth}`.
    let hidden = source.hidden();
    for n in 1..=3 {
        assert!(hidden.contains(&(access(n), SHOWN.to_owned())), "{n}");
        assert!(hidden.contains(&(refresh(n), SHOWN.to_owned())), "{n}");
    }
    assert_eq!(format!("{:?}", Secret(access(1))), "Secret(…)");
    assert!(!format!("{source:?}").contains("access-token"));

    // The end: closed first — its DELETE refused gets no refresh — then
    // revoked, the refresh token first.
    source.closing();
    stub.set(|c| c.valid = "expired".into());
    let before = stub.seen().len();
    let _ = client.close().await;
    assert_eq!(source.revoke().await, Vec::<String>::new());
    let end: Vec<String> = stub.seen()[before..].to_vec();
    assert_eq!(
        end,
        [
            format!("DELETE - auth=Bearer {} session=s-1", access(3)),
            format!("revoke refresh_token {}", refresh(3)),
            format!("revoke access_token {}", access(3)),
        ]
    );
}

#[tokio::test]
async fn the_callback_takes_only_its_own_state_and_issuer() {
    let stub = Stub::start().await;
    let (_session, mut ui, _stop, task) = signing_in(&stub, WAIT).await;
    let (_, request) = question(&mut ui).await;
    let back = to_callback(&link(&request)).await;
    let mut url = Url::parse(&back).unwrap();
    let (path, port) = (url.path().to_owned(), url.port().unwrap());
    // Another state, and an error without the state: answered 400, the
    // sign-in waits on.
    url.set_query(Some("code=x&state=wrong"));
    assert_eq!(get(url.as_str()).await.unwrap().0, 400);
    url.set_query(Some("error=access_denied"));
    assert_eq!(get(url.as_str()).await.unwrap().0, 400);
    // Another path on its port: nothing there.
    assert_eq!(
        get(&format!(
            "http://127.0.0.1:{port}/callback/guess?{}",
            Url::parse(&back).unwrap().query().unwrap()
        ))
        .await
        .unwrap()
        .0,
        404
    );
    assert!(!task.is_finished());
    assert_eq!(path, Url::parse(&back).unwrap().path());
    assert_eq!(get(&back).await.unwrap().0, 200);
    assert!(tokio::time::timeout(WAIT, task)
        .await
        .unwrap()
        .unwrap()
        .is_ok());

    // An issuer that differs is refused (mix-up), and so is none where
    // the metadata says one comes.
    for (iss, said) in [
        (Some("http://127.0.0.1:1".to_owned()), "another issuer"),
        (None, "without the issuer it must name"),
    ] {
        stub.set(|c| c.iss = iss);
        let (_session, mut ui, _stop, task) = signing_in(&stub, WAIT).await;
        let (_, request) = question(&mut ui).await;
        let back = to_callback(&link(&request)).await;
        get(&back).await.unwrap();
        let r = tokio::time::timeout(WAIT, task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(r.why, Why::Callback);
        assert!(r.said.contains(said), "{}", r.said);
        assert_eq!(resolved(&mut ui).await, ResolveReason::Withdrawn);
    }
}

#[tokio::test]
async fn an_error_from_the_browser_is_said_cleaned_and_never_to_the_agent() {
    let stub = Stub::start().await;
    stub.set(|c| c.error = Some(("access_denied".into(), "the user\u{202e} said\n\tno".into())));
    let (_session, mut ui, _stop, task) = signing_in(&stub, WAIT).await;
    let (_, request) = question(&mut ui).await;
    let back = to_callback(&link(&request)).await;
    get(&back).await.unwrap();
    let r = tokio::time::timeout(WAIT, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(
        r.said,
        "the sign-in ended with access_denied: the user said no"
    );
    assert_eq!(
        r.for_agent(),
        "the sign-in did not complete (the owner was told why)"
    );
}

#[tokio::test]
async fn the_question_cancelled_unanswered_timed_out_or_printed() {
    let stub = Stub::start().await;
    // Cancel.
    let (session, mut ui, _stop, task) = signing_in(&stub, WAIT).await;
    let (id, _) = question(&mut ui).await;
    session.send(ClientMessage::UiResponse {
        id,
        value: json!("cancel"),
    });
    let r = tokio::time::timeout(WAIT, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(r.why, Why::Cancelled);

    // Nobody answering in time.
    let (_session, mut ui, _stop, task) = signing_in(&stub, Duration::from_millis(200)).await;
    question(&mut ui).await;
    let r = tokio::time::timeout(WAIT, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(r.why, Why::TimedOut);
    assert_eq!(resolved(&mut ui).await, ResolveReason::TimedOut);

    // Nobody there at all.
    let (session, _driver) = Session::new(true);
    let found = stub.found().await;
    let ask = Ask {
        session: &session,
        title: "Sign in?".into(),
        timeout: WAIT,
        printed: None,
    };
    let r = sign_in(&Net::loopback(), &found, ask, &CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(r.why, Why::NoFrontend);

    // Phone mode: no link on the page, the link printed here; signing in
    // with it works.
    let mut ui = session.connect();
    ui.messages.recv().await.unwrap();
    let printed: Arc<Mutex<Option<String>>> = Arc::default();
    let task = {
        let (session, printed) = (session.clone(), printed.clone());
        tokio::spawn(async move {
            let print = move |link: &str| *printed.lock().unwrap() = Some(link.to_owned());
            let ask = Ask {
                session: &session,
                title: "Sign in?".into(),
                timeout: WAIT,
                printed: Some(&print),
            };
            sign_in(&Net::loopback(), &found, ask, &CancellationToken::new()).await
        })
    };
    let (_, request) = question(&mut ui).await;
    assert!(
        matches!(request, UiRequest::SignIn { url: None, .. }),
        "{request:?}"
    );
    let start = printed.lock().unwrap().clone().unwrap();
    get(&to_callback(&start).await).await.unwrap();
    assert!(tokio::time::timeout(WAIT, task)
        .await
        .unwrap()
        .unwrap()
        .is_ok());
}

#[tokio::test]
async fn a_stopped_run_ends_the_sign_in_and_its_listener() {
    let stub = Stub::start().await;
    let (_session, mut ui, stop, task) = signing_in(&stub, WAIT).await;
    let (_, request) = question(&mut ui).await;
    let start = link(&request);
    stop.cancel();
    let r = tokio::time::timeout(WAIT, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(r.why, Why::Stopped);
    assert_eq!(
        r.said,
        "the run was stopped while it waited for the sign-in"
    );
    assert_eq!(resolved(&mut ui).await, ResolveReason::Withdrawn);
    let gone = async {
        while get(&start).await.is_ok() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(WAIT, gone).await.unwrap();
}

#[tokio::test]
async fn a_token_refusal_or_an_answer_that_is_not_one_is_never_quoted() {
    let stub = Stub::start().await;
    for (status, body, said) in [
        (
            400,
            json!({ "error": "invalid_grant", "error_description": format!("bad code {CODE}") })
                .to_string(),
            "refused it (HTTP 400: invalid_grant — bad code ${oauth})",
        ),
        (
            200,
            format!("access_token={CODE}"),
            "its answer is not a JSON object",
        ),
        (
            200,
            json!({ "token_type": "mac", "access_token": CODE }).to_string(),
            "gave no `Bearer` token",
        ),
        (
            200,
            json!({ "token_type": "Bearer", "access_token": "two words" }).to_string(),
            "no token yobuild can send",
        ),
    ] {
        stub.set(|c| c.token_refusal = Some((status, body)));
        let (_session, mut ui, _stop, task) = signing_in(&stub, WAIT).await;
        let (_, request) = question(&mut ui).await;
        get(&to_callback(&link(&request)).await).await.unwrap();
        let r = tokio::time::timeout(WAIT, task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(r.why, Why::Exchange);
        assert!(r.said.contains(said), "{said}: {}", r.said);
        assert!(!r.said.contains(CODE), "{}", r.said);
    }
}

/// A source signed in at the stub, without the browser.
async fn source_at(stub: &Stub) -> Arc<Source> {
    let found = stub.found().await;
    stub.set(|c| {
        c.issued = 1;
        c.valid = access(1);
        c.refresh = refresh(1);
    });
    let tokens = Tokens {
        access: Secret(access(1)),
        refresh: Some(Secret(refresh(1))),
    };
    Arc::new(Source::new(
        Net::loopback(),
        &found,
        "client-1".into(),
        tokens,
    ))
}

#[tokio::test]
async fn a_refresh_refused_or_a_challenge_pointing_elsewhere_ends_the_sign_in() {
    let stub = Stub::start().await;
    let current = format!("Bearer {}", access(1));

    // A value refused that is not the current one: the current, unrefreshed.
    let source = source_at(&stub).await;
    let stale = AuthChallenge::new(401, "tools/call").with_refused("Bearer old");
    assert_eq!(
        source.challenged(&stale).await.unwrap(),
        Some(current.clone())
    );
    // A 403 is not refreshed.
    let forbidden = AuthChallenge::new(403, "tools/call").with_refused(current.clone());
    assert_eq!(source.challenged(&forbidden).await.unwrap(), None);
    assert!(!stub.seen().iter().any(|s| s.starts_with("token")));

    // A refresh refused: ended, said to the owner without the token.
    stub.set(|c| c.refresh_refused = true);
    let refused = AuthChallenge::new(401, "tools/call")
        .with_www_authenticate(stub.challenge())
        .with_refused(current.clone());
    let e = source.challenged(&refused).await.unwrap_err();
    assert_eq!(e.to_string(), format!("Transport error: {ENDED}"));
    let why = source.ended().unwrap();
    assert_eq!(
        why,
        "its token could not be refreshed: its token endpoint (127.0.0.1) refused it (HTTP 400: invalid_grant — refused ${oauth})"
    );
    assert!(source.authorization().await.is_err());

    // A challenge pointing at other metadata, or another scheme: ended,
    // never a new discovery.
    for (challenge, said) in [
        (
            "Bearer resource_metadata=\"https://evil.example/m\"",
            "other resource metadata",
        ),
        ("Basic realm=\"x\"", "another kind of sign-in"),
    ] {
        let source = source_at(&stub).await;
        let c = AuthChallenge::new(401, "tools/call")
            .with_www_authenticate(challenge)
            .with_refused(current.clone());
        assert!(source.challenged(&c).await.is_err());
        assert!(source.ended().unwrap().contains(said));
    }

    // No refresh token: ended.
    let found = stub.found().await;
    let bare = Source::new(
        Net::loopback(),
        &found,
        "client-1".into(),
        Tokens {
            access: Secret(access(1)),
            refresh: None,
        },
    );
    let c = AuthChallenge::new(401, "tools/call").with_refused(current);
    assert!(bare.challenged(&c).await.is_err());
    assert_eq!(
        bare.ended().unwrap(),
        "its token expired and it gave no refresh token"
    );
}

#[test]
fn what_a_server_says_is_cleaned_and_cut() {
    assert_eq!(clean("a\u{1b}[31m b\u{2066}c\r\nd", 50), "a[31m bc d");
    assert_eq!(clean(&"x".repeat(10), 4), "xxxx…");
    assert!(is_error_code("invalid_grant"));
    assert!(!is_error_code("two words") && !is_error_code("") && !is_error_code("a\"b"));
}

/// A JSON answer (the crate's axum has no `json` feature).
fn answer(json: Json) -> ([(&'static str, &'static str); 1], String) {
    ([("content-type", "application/json")], json.to_string())
}

#[tokio::test]
async fn the_real_client_resolves_to_public_addresses_but_for_the_proxy() {
    // Through the client a real sign-in uses: its resolver, and its error
    // told apart from "not reached".
    let net = Net::checked().unwrap();
    let e = net
        .get(&Url::parse("https://localhost:1/x").unwrap())
        .await
        .err()
        .unwrap();
    assert_eq!(
        e,
        "localhost resolves to an address on this machine or its network"
    );
    // The proxy's own name resolves as the system would.
    let proxied = PublicOnly::with_proxies(
        [
            "http://localhost:3128".to_owned(),
            "proxy.corp.example:8080".to_owned(),
        ]
        .into_iter(),
    );
    assert_eq!(proxied.proxies, ["localhost", "proxy.corp.example"]);
    assert!(
        reqwest::dns::Resolve::resolve(&proxied, "localhost".parse().unwrap())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn discovery_falls_back_to_the_root_openid_and_the_next_authorization_server() {
    let stub = Stub::start().await;
    stub.set(|c| {
        c.root_resource = c.resource.clone();
        c.resource = Json::Null;
        c.openid = c.authorization.clone();
        c.authorization = Json::Null;
        let base = c.issuer_of_openid();
        c.root_resource["authorization_servers"] = json!(["https://auth.example.com", base]);
    });
    let found = discover(&Net::loopback(), &stub.server(), None, Site::Own)
        .await
        .unwrap();
    assert_eq!(found.issuer(), stub.base);
    let seen = stub.seen();
    assert_eq!(
        seen,
        [
            "resource metadata", "root resource metadata", "authorization metadata",
            "openid metadata"
        ]
    );
    // None of them taken: the first one's reason said.
    stub.set(|c| c.root_resource["authorization_servers"] = json!(["https://auth.example.com"]));
    let r = discover(&Net::loopback(), &stub.server(), None, Site::Own)
        .await
        .unwrap_err();
    assert!(
        r.said
            .contains("(the first: an address refused (the tests' stub is on this machine))"),
        "{}",
        r.said
    );
}

impl Config {
    fn issuer_of_openid(&self) -> String {
        self.openid["issuer"].as_str().unwrap().to_owned()
    }
}

#[tokio::test]
async fn a_run_stopped_while_registering_or_exchanging_ends_it_and_revokes_what_came() {
    let stub = Stub::start().await;
    // While registering: no question asked.
    stub.set(|c| c.slow = Duration::from_secs(5));
    let (_session, _ui, stop, task) = signing_in(&stub, WAIT).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    stop.cancel();
    let r = tokio::time::timeout(WAIT, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(r.why(), Why::Stopped);
    assert_eq!(
        r.for_owner(),
        "the run was stopped while yobuild registered with its sign-in"
    );

    // While exchanging: the tokens it still gets are revoked.
    stub.set(|c| c.slow = Duration::ZERO);
    let (_session, mut ui, stop, task) = signing_in(&stub, WAIT).await;
    let (_, request) = question(&mut ui).await;
    let back = to_callback(&link(&request)).await;
    stub.set(|c| c.slow = Duration::from_millis(800));
    assert_eq!(get(&back).await.unwrap().0, 200);
    tokio::time::sleep(Duration::from_millis(100)).await;
    stop.cancel();
    let r = tokio::time::timeout(WAIT, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(
        r.for_owner(),
        "the run was stopped while the sign-in's code was exchanged"
    );
    let revoked = async {
        loop {
            let issued = stub.config.lock().unwrap().issued;
            if issued > 0
                && stub
                    .seen()
                    .contains(&format!("revoke refresh_token {}", refresh(issued)))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    tokio::time::timeout(WAIT, revoked).await.unwrap();
}

#[tokio::test]
async fn a_refresh_keeps_an_unrotated_token_and_a_passing_trouble_keeps_the_sign_in() {
    let stub = Stub::start().await;
    let source = source_at(&stub).await;
    stub.set(|c| c.rotate = false);
    // The challenge naming the server's root well-known address is its own.
    let root = format!("{}/.well-known/oauth-protected-resource", stub.base);
    for n in 1..=2 {
        let c = AuthChallenge::new(401, "tools/call")
            .with_www_authenticate(format!("Bearer resource_metadata=\"{root}\""))
            .with_refused(format!("Bearer {}", access(n)));
        assert_eq!(
            source.challenged(&c).await.unwrap(),
            Some(format!("Bearer {}", access(n + 1)))
        );
    }
    let refreshes: Vec<String> = stub
        .seen()
        .into_iter()
        .filter(|s| s.starts_with("token refresh"))
        .collect();
    assert_eq!(refreshes, ["token refresh current=true resource=yes"; 2]);

    // A 503: this request fails, the sign-in stays; the next one refreshes.
    stub.set(|c| c.refresh_status = Some(503));
    let c = AuthChallenge::new(401, "tools/call").with_refused(format!("Bearer {}", access(3)));
    let e = source.challenged(&c).await.unwrap_err().to_string();
    assert!(
        e.contains("could not be refreshed now (its token endpoint (127.0.0.1) answered HTTP 503)"),
        "{e}"
    );
    assert_eq!(source.ended(), None);
    stub.set(|c| c.refresh_status = None);
    assert!(source.challenged(&c).await.unwrap().is_some());

    // A revocation refused is said, for the caller to tell the owner.
    stub.set(|c| c.revoke_status = 500);
    assert_eq!(
        source.revoke().await,
        [
            "the sign-in's refresh token was not revoked at 127.0.0.1 (HTTP 500)",
            "the sign-in's access token was not revoked at 127.0.0.1 (HTTP 500)"
        ]
    );
}

/// The callback's URL with its query replaced.
fn with_query(back: &str, query: &str) -> String {
    let mut url = Url::parse(back).unwrap();
    url.set_query(Some(query));
    url.to_string()
}

#[tokio::test]
async fn a_parameter_given_twice_or_an_error_that_is_no_code_ends_it_so() {
    let stub = Stub::start().await;
    let base = stub.base.clone();
    for (query, said) in [
        ("code=a&code=b", "the browser came back with `code` twice"),
        (
            "error=a&error=b&code=x",
            "the browser came back with `error` twice",
        ),
        ("iss=x&code=y", "the browser came back with `iss` twice"),
        (
            "error=no%20code%20here",
            "the sign-in ended with an error that is no OAuth error code",
        ),
    ] {
        let (_session, mut ui, _stop, task) = signing_in(&stub, WAIT).await;
        let (_, request) = question(&mut ui).await;
        let back = to_callback(&link(&request)).await;
        let state = Url::parse(&back)
            .unwrap()
            .query_pairs()
            .find(|(k, _)| k == "state")
            .unwrap()
            .1
            .into_owned();
        let iss = url::form_urlencoded::byte_serialize(base.as_bytes()).collect::<String>();
        let query = format!("{query}&state={state}&iss={iss}");
        assert_eq!(get(&with_query(&back, &query)).await.unwrap().0, 200);
        let r = tokio::time::timeout(WAIT, task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(r.why(), Why::Callback);
        assert_eq!(r.for_owner(), said);
    }
    // A state given twice is no state: 400, the sign-in waits on.
    let (_session, mut ui, _stop, task) = signing_in(&stub, WAIT).await;
    let (_, request) = question(&mut ui).await;
    let back = to_callback(&link(&request)).await;
    let doubled = format!(
        "{}&state=again",
        Url::parse(&back).unwrap().query().unwrap()
    );
    assert_eq!(get(&with_query(&back, &doubled)).await.unwrap().0, 400);
    assert!(!task.is_finished());
    get(&back).await.unwrap();
    assert!(tokio::time::timeout(WAIT, task)
        .await
        .unwrap()
        .unwrap()
        .is_ok());
}

#[tokio::test]
async fn a_session_ending_during_the_sign_in_says_so() {
    let stub = Stub::start().await;
    let found = stub.found().await;
    let (session, driver) = Session::new(true);
    let mut ui = session.connect();
    ui.messages.recv().await.unwrap();
    let task = {
        let session = session.clone();
        tokio::spawn(async move {
            let ask = Ask {
                session: &session,
                title: "Sign in?".into(),
                timeout: WAIT,
                printed: None,
            };
            sign_in(&Net::loopback(), &found, ask, &CancellationToken::new()).await
        })
    };
    question(&mut ui).await;
    drop(driver);
    let r = tokio::time::timeout(WAIT, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(r.why(), Why::SessionEnded);
    assert_eq!(r.for_agent(), "the session ended during the sign-in");
}
