//! The sign-in for a remote MCP server that wants one (design §14, "Stage
//! 4: remote servers that need a sign-in"): OAuth 2.1 as the MCP
//! specification's "Authorization" asks of a client, in the owner's own
//! browser, the token handed to the server through yoagent's
//! [`AuthorizationSource`] and never to the agent.
//!
//! The steps, each a function here: [`discover`] (the resource's and the
//! authorization server's metadata, every step checked before anyone is
//! asked: point 3; it holds nothing open, so its caller ends it by dropping
//! it), then — once the caller's Allow question was answered (point 4) —
//! [`sign_in`]: the callback's listener on a loopback port of its own,
//! dynamic registration (point 7), PKCE, the sign-in question with the
//! short start link raced against the callback (points 5 and 6) and the
//! exchange (point 8). Its [`Source`] hands the server the token, refreshes
//! it on a 401 (point 9) and revokes it at the end (point 10).
//!
//! The server, its metadata and everything they name are untrusted: no
//! redirect is followed, an answer past [`ANSWER_LIMIT`] is refused and one
//! within it read for the fields used and nothing else, names are resolved
//! only to public addresses ([`PublicOnly`]), and what a server says is
//! never quoted to the agent ([`Refusal::for_agent`]).

// Used by `load_plugin`'s remote and the manifest's `auth = "oauth"`, the
// next steps (design §14, "Order of PRs"); until then only its tests call it.
#![cfg_attr(not(test), allow(dead_code))]

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::{Map, Value as Json};
use sha2::{Digest, Sha256};
use tokio::sync::{oneshot, Notify};
use tokio_util::sync::CancellationToken;
use url::Url;
use yoagent::mcp::{AuthChallenge, AuthorizationSource, McpError};

use crate::builtins::find_plugin::remote_address;
use crate::frontend::{Session, UiRequest, Unanswered};

/// Its tests, and the stub servers `load_plugin`'s sign-in tests use.
#[cfg(test)]
pub(crate) mod tests;

/// How long each request of the sign-in may take.
const REQUEST_TIME: Duration = Duration::from_secs(15);

/// The largest answer taken (metadata, a registration, a token): a larger
/// success is refused; a larger refusal is cut there, its status kept.
const ANSWER_LIMIT: usize = 64 * 1024;

/// The most scopes a server may ask for, and the longest one.
const MAX_SCOPES: usize = 20;
const MAX_SCOPE_CHARS: usize = 64;

/// Scopes said as broad in the question.
const BROAD_SCOPES: [&str; 4] = ["offline_access", "*", "all", "admin"];

/// The longest `client_id` taken from a registration.
const MAX_CLIENT_ID: usize = 256;

/// The longest text of a server's shown to the owner (an error's
/// description).
const SAID_CHARS: usize = 200;

/// What a token is shown as wherever an error yobuild makes would show
/// one (a server echoing it in its refusal, a later call's error); the code
/// and the verifier are hidden so in what a token endpoint says too.
pub(crate) const SHOWN: &str = "${oauth}";

/// Why a signed-in server's calls fail once its sign-in ended: its token
/// expired with no refresh token, a refresh was refused, or a challenge
/// asked for another kind of sign-in or pointed at other metadata.
pub(crate) const ENDED: &str = "its sign-in ended: it is not signed in again this session";

/// `mutex`'s guard, a poisoned lock taken over: what it guards is whole at
/// every await-free step.
fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A secret of the sign-in (a token, the code, the verifier, `state`, the
/// callback's paths): `Debug` prints nothing of it.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Secret(String);

impl Secret {
    /// 32 random bytes, base64url without padding (43 characters).
    fn random() -> Result<Secret, Refusal> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| {
            Refusal::new(
                Why::Local,
                format!("the system's random numbers are not available here: {e}"),
            )
        })?;
        Ok(Secret(URL_SAFE_NO_PAD.encode(bytes)))
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(…)")
    }
}

/// Why a sign-in did not happen, or ended: [`Refusal::for_owner`] (what a
/// server says cleaned and cut, never a secret) and [`Refusal::for_agent`]
/// (fixed words). No `Display`: which reader a text is for is always
/// named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refusal {
    why: Why,
    said: String,
}

/// What kind of refusal, for the agent's fixed words and the owner's hints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Why {
    /// No sign-in published: no challenge named metadata, and none is at
    /// the well-known addresses (a server that wants a credential instead).
    NoSignIn,
    /// Discovery found something it refuses, or could not look.
    Discovery,
    /// The authorization server offers no `S256`.
    NoPkce,
    /// No `registration_endpoint`.
    NoRegistration,
    /// A loaded server's sign-in on another site, or its own site unknown.
    OtherSite,
    /// The registration's answer refused, or the request failed.
    Registration,
    /// The owner's Cancel.
    Cancelled,
    /// Nobody signed in within the question's time.
    TimedOut,
    /// No frontend to ask.
    NoFrontend,
    /// The session ended while it waited.
    SessionEnded,
    /// The run was stopped meanwhile.
    Stopped,
    /// The browser came back with an error, or one yobuild refuses.
    Callback,
    /// The token request refused, or its answer.
    Exchange,
    /// The sign-in could not start on this machine (the callback's
    /// listener, the system's random numbers), or its callback stopped.
    Local,
}

impl Refusal {
    fn new(why: Why, said: impl Into<String>) -> Refusal {
        Refusal {
            why,
            said: said.into(),
        }
    }

    /// What kind of refusal.
    pub(crate) fn why(&self) -> Why {
        self.why
    }

    /// What the owner is told: what went wrong, a server's words cleaned
    /// and cut, never a secret.
    pub(crate) fn for_owner(&self) -> &str {
        &self.said
    }

    /// What the agent is told: fixed words, never the server's.
    pub(crate) fn for_agent(&self) -> &'static str {
        Refusal::agent_words(self.why)
    }

    /// The agent's words for each kind (the hints match them).
    pub(crate) fn agent_words(why: Why) -> &'static str {
        match why {
            Why::NoSignIn => "it offers no sign-in",
            Why::Discovery => "the server's sign-in could not be used (the owner was told why)",
            Why::NoPkce => "its sign-in offers no PKCE, which yobuild requires",
            Why::NoRegistration => "its sign-in offers no way for yobuild to register",
            Why::OtherSite => {
                "its sign-in is not on its own site, which a server the agent found must keep to"
            }
            Why::Registration => {
                "yobuild could not register with its sign-in (the owner was told why)"
            }
            Why::Cancelled => "the owner cancelled the sign-in",
            Why::TimedOut => "nobody signed in in time",
            Why::NoFrontend => "nobody is here to sign in",
            Why::SessionEnded => "the session ended during the sign-in",
            Why::Stopped => "the run was stopped during the sign-in",
            Why::Callback => "the sign-in did not complete (the owner was told why)",
            Why::Exchange => "the sign-in's token could not be had (the owner was told why)",
            Why::Local => "the sign-in failed on this machine (the owner was told why)",
        }
    }
}

/// Where a sign-in may be (design §14 "Stage 4", point 3): for a server the
/// agent found, every part of it — the authorization server and each
/// endpoint its metadata names — on the server's own site (the same
/// registrable domain, by the public suffix list compiled in), so a server
/// cannot send the owner to a real provider's sign-in whose token works for
/// its whole service; for a manifest's `[[mcp]]`, which the owner wrote,
/// anywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Site {
    Own,
    Any,
}

/// The sign-in's requests: no redirect followed, [`REQUEST_TIME`] each,
/// names resolved to public addresses only, https only — or, for the tests,
/// a stub on this machine ([`Net::loopback`]), with the same client but for
/// those two.
#[derive(Clone)]
pub(crate) struct Net {
    http: reqwest::Client,
    loopback: bool,
}

/// The client both kinds of [`Net`] start from.
fn client() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(REQUEST_TIME)
}

/// What a GET found.
enum Fetched {
    Object(Map<String, Json>),
    /// 404, 405 or 410: not there (another address may hold it).
    Missing(u16),
}

impl Net {
    /// The requests of a real sign-in.
    pub(crate) fn checked() -> Result<Net, String> {
        let http = client()
            .https_only(true)
            .dns_resolver(Arc::new(PublicOnly::from_environment()))
            .build()
            .map_err(|e| format!("the sign-in's HTTP client could not be made: {e}"))?;
        Ok(Net {
            http,
            loopback: false,
        })
    }

    /// For the tests: endpoints over http(s) on this machine, the system's
    /// resolver, a site its host.
    #[cfg(test)]
    pub(crate) fn loopback() -> Net {
        Net {
            http: client().build().unwrap(),
            loopback: true,
        }
    }

    /// An endpoint's address, checked as a remote's is (`remote_address`:
    /// https, no user, no IP address, no local or one-label name).
    fn address(&self, url: &str) -> Result<Url, String> {
        if !self.loopback {
            return remote_address(url);
        }
        let parsed = Url::parse(url).map_err(|_| "an address refused (not a URL)".to_owned())?;
        let local = matches!(parsed.host_str(), Some("127.0.0.1" | "localhost"));
        if !local || !parsed.username().is_empty() || !matches!(parsed.scheme(), "http" | "https") {
            return Err("an address refused (the tests' stub is on this machine)".into());
        }
        Ok(parsed)
    }

    /// The site a host is on: its registrable domain.
    fn site(&self, host: &str) -> Option<String> {
        if self.loopback {
            return Some(host.to_owned());
        }
        registrable(host)
    }

    /// A GET's JSON object, or that it is not there; any other status is
    /// an error that says it.
    async fn get(&self, url: &Url) -> Result<Fetched, String> {
        let response = self
            .http
            .get(url.clone())
            .header("accept", "application/json")
            .send()
            .await
            .map_err(failed)?;
        let status = response.status().as_u16();
        match status {
            200..=299 => object(&read(response, true).await?).map(Fetched::Object),
            404 | 405 | 410 => Ok(Fetched::Missing(status)),
            300..=399 => Err(format!(
                "it answered with a redirect (HTTP {status}), which is not followed"
            )),
            _ => Err(format!("it answered HTTP {status}")),
        }
    }

    /// A POST's status and its answer as a JSON object (an answer that is
    /// none is `None`: a refusal may carry no JSON).
    async fn post(
        &self,
        url: &Url,
        body: Body,
    ) -> Result<(u16, Option<Map<String, Json>>), String> {
        let request = self
            .http
            .post(url.clone())
            .header("accept", "application/json");
        let request = match body {
            Body::Json(json) => request
                .header("content-type", "application/json")
                .body(json.to_string()),
            Body::Form(pairs) => {
                let mut form = url::form_urlencoded::Serializer::new(String::new());
                for (name, value) in &pairs {
                    form.append_pair(name, value);
                }
                request
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(form.finish())
            }
        };
        let response = request.send().await.map_err(failed)?;
        let status = response.status().as_u16();
        // A refusal's status is what matters: its body is read as far as
        // it goes, never failing the read.
        let answer = read(response, (200..300).contains(&status)).await?;
        Ok((status, object(&answer).ok()))
    }
}

/// A POST's body.
enum Body {
    Json(Json),
    Form(Vec<(&'static str, String)>),
}

/// A request that failed, in fixed words: reqwest's own text names the URL
/// and may say more than is wanted.
fn failed(e: reqwest::Error) -> String {
    let mut source: Option<&dyn std::error::Error> = Some(&e);
    while let Some(err) = source {
        if let Some(refused) = err.downcast_ref::<NameRefused>() {
            return refused.to_string();
        }
        source = err.source();
    }
    if e.is_timeout() {
        format!("no answer within {} s", REQUEST_TIME.as_secs())
    } else if e.is_connect() {
        "it could not be reached".into()
    } else {
        "the request failed".into()
    }
}

/// An answer's body. `strict`: past [`ANSWER_LIMIT`], or cut off, is an
/// error; else what came within it.
async fn read(mut response: reqwest::Response, strict: bool) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(e) if strict => return Err(failed(e)),
            Err(_) => break,
        };
        if body.len() + chunk.len() > ANSWER_LIMIT {
            if strict {
                return Err(format!("its answer is past {} KiB", ANSWER_LIMIT / 1024));
            }
            break;
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// A JSON object, said in fixed words when it is none (serde's text quotes
/// what it read).
fn object(body: &[u8]) -> Result<Map<String, Json>, String> {
    match serde_json::from_slice(body) {
        Ok(Json::Object(map)) => Ok(map),
        Ok(_) => Err("its answer is not a JSON object".into()),
        Err(_) => Err("its answer is not JSON".into()),
    }
}

/// A field that, when there, is text.
fn text<'a>(map: &'a Map<String, Json>, key: &str) -> Result<Option<&'a str>, String> {
    match map.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(s)) => Ok(Some(s)),
        Some(_) => Err(format!("its `{key}` is not text")),
    }
}

/// A field that, when there, is a list of text.
fn texts(map: &Map<String, Json>, key: &str) -> Result<Option<Vec<String>>, String> {
    match map.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::Array(items)) => items
            .iter()
            .map(|item| item.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()
            .map(Some)
            .ok_or_else(|| format!("its `{key}` is not a list of text")),
        Some(_) => Err(format!("its `{key}` is not a list of text")),
    }
}

/// A host's registrable domain (`mcp.linear.app` → `linear.app`), by the
/// public suffix list compiled in; `None` for a public suffix itself.
fn registrable(host: &str) -> Option<String> {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    psl::domain_str(&host).map(str::to_owned)
}

/// A resolver that refuses a name with any address on this machine or its
/// network (loopback, private, link-local, …): the server's metadata may
/// name a host that points inside the network (DNS rebinding). The proxy's
/// own name (`HTTPS_PROXY`, `ALL_PROXY`), which reqwest resolves through
/// it too, is resolved as the system would: a proxy is the owner's, often
/// on the network. Through a proxy, the targets' names are the proxy's to
/// resolve, so this guard holds only without one.
struct PublicOnly {
    proxies: Vec<String>,
}

impl PublicOnly {
    /// With the proxies this process's environment names.
    fn from_environment() -> PublicOnly {
        let named = ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"]
            .iter()
            .filter_map(|name| std::env::var(name).ok());
        PublicOnly::with_proxies(named)
    }

    fn with_proxies(named: impl Iterator<Item = String>) -> PublicOnly {
        let proxies = named
            .filter_map(|value| {
                let value = value.trim();
                let url = if value.contains("://") {
                    Url::parse(value)
                } else {
                    Url::parse(&format!("http://{value}"))
                };
                url.ok()?.host_str().map(|h| h.to_ascii_lowercase())
            })
            .collect();
        PublicOnly { proxies }
    }
}

/// A name [`PublicOnly`] refused, told apart from other failures.
#[derive(Debug)]
struct NameRefused {
    host: String,
    /// It has an address on this machine or its network; else none.
    private: bool,
}

impl fmt::Display for NameRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.private {
            write!(
                f,
                "{} resolves to an address on this machine or its network",
                self.host
            )
        } else {
            write!(f, "{} has no address", self.host)
        }
    }
}

impl std::error::Error for NameRefused {}

impl reqwest::dns::Resolve for PublicOnly {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_owned();
        let proxy = self.proxies.contains(&host.to_ascii_lowercase());
        Box::pin(async move {
            let addresses: Vec<SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let refused = |private| {
                Err(Box::new(NameRefused { host, private })
                    as Box<dyn std::error::Error + Send + Sync>)
            };
            if addresses.is_empty() {
                return refused(false);
            }
            if !proxy && addresses.iter().any(|a| !public(a.ip())) {
                return refused(true);
            }
            Ok(Box::new(addresses.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Whether an address is on the public internet.
fn public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => public_v4(v4),
        IpAddr::V6(v6) => public_v6(v6),
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || a == 0
        || (a == 100 && (64..128).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && (b == 18 || b == 19))
        || a >= 240)
}

fn public_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return public_v4(v4);
    }
    let s = ip.segments();
    let embedded = |high: u16, low: u16| {
        let [a, b] = high.to_be_bytes();
        let [c, d] = low.to_be_bytes();
        Ipv4Addr::new(a, b, c, d)
    };
    // NAT64 (64:ff9b::/96) and 6to4 (2002::/16): the IPv4 address each
    // stands for.
    if s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        return public_v4(embedded(s[6], s[7]));
    }
    if s[0] == 0x2002 {
        return public_v4(embedded(s[1], s[2]));
    }
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        // IPv4-compatible (::a.b.c.d, deprecated).
        || s[..6] == [0, 0, 0, 0, 0, 0]
        // Discard-only (100::/64).
        || s[..4] == [0x100, 0, 0, 0]
        // Local-use NAT64 (64:ff9b:1::/48).
        || s[..3] == [0x64, 0xff9b, 1]
        // Teredo (2001::/32), documentation (2001:db8::/32).
        || (s[0] == 0x2001 && (s[1] == 0 || s[1] == 0x0db8))
        // Unique local (fc00::/7), link-local (fe80::/10), site-local
        // (fec0::/10, deprecated).
        || (s[0] & 0xfe00) == 0xfc00
        || (s[0] & 0xffc0) == 0xfe80
        || (s[0] & 0xffc0) == 0xfec0)
}

/// A `WWW-Authenticate` value read into its challenges: each scheme
/// (lowercase) with its parameters (names lowercase).
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Challenge {
    schemes: Vec<(String, Vec<(String, String)>)>,
}

impl Challenge {
    /// Reads `Bearer realm="OAuth", resource_metadata="…", scope="a b"`,
    /// several challenges joined with `, ` too (yoagent joins a response's
    /// headers so). Lenient: what does not read is skipped.
    pub(crate) fn parse(header: &str) -> Challenge {
        let chars: Vec<char> = header.chars().collect();
        let mut at = 0;
        let mut schemes: Vec<(String, Vec<(String, String)>)> = Vec::new();
        let token = |at: &mut usize| {
            let start = *at;
            while *at < chars.len() && !matches!(chars[*at], ' ' | '\t' | ',' | '=' | '"') {
                *at += 1;
            }
            chars[start..*at].iter().collect::<String>()
        };
        let spaces = |at: &mut usize| {
            while *at < chars.len() && matches!(chars[*at], ' ' | '\t') {
                *at += 1;
            }
        };
        while at < chars.len() {
            if matches!(chars[at], ' ' | '\t' | ',') {
                at += 1;
                continue;
            }
            let word = token(&mut at);
            if word.is_empty() {
                // A stray `=` or `"`: skipped.
                at += 1;
                continue;
            }
            let mut after = at;
            spaces(&mut after);
            if after < chars.len() && chars[after] == '=' {
                // A parameter of the scheme before it.
                at = after + 1;
                spaces(&mut at);
                let value = if at < chars.len() && chars[at] == '"' {
                    at += 1;
                    let mut value = String::new();
                    while at < chars.len() && chars[at] != '"' {
                        if chars[at] == '\\' && at + 1 < chars.len() {
                            at += 1;
                        }
                        value.push(chars[at]);
                        at += 1;
                    }
                    at += 1;
                    value
                } else {
                    let start = at;
                    while at < chars.len() && !matches!(chars[at], ',' | ' ' | '\t') {
                        at += 1;
                    }
                    chars[start..at].iter().collect()
                };
                if let Some((_, params)) = schemes.last_mut() {
                    params.push((word.to_ascii_lowercase(), value));
                }
            } else {
                schemes.push((word.to_ascii_lowercase(), Vec::new()));
            }
        }
        Challenge { schemes }
    }

    /// The `Bearer` challenge's parameters, when there is one.
    pub(crate) fn bearer(&self) -> Option<&[(String, String)]> {
        self.schemes
            .iter()
            .find(|(scheme, _)| scheme == "bearer")
            .map(|(_, params)| params.as_slice())
    }

    /// A `Bearer` parameter.
    fn param(&self, name: &str) -> Option<&str> {
        self.bearer()?
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// What discovery found, every part checked and none changeable after: the
/// sign-in [`sign_in`] does, and what the owner is asked about
/// ([`Found::details`]).
#[derive(Debug, Clone)]
pub(crate) struct Found {
    /// The MCP server's URL: the `resource`, where the token goes.
    server: Url,
    /// The resource metadata's address, pinned: a later challenge pointing
    /// elsewhere ends the sign-in.
    metadata: Url,
    /// The authorization server's issuer, as its metadata says it.
    issuer: String,
    authorization_endpoint: Url,
    token_endpoint: Url,
    registration_endpoint: Url,
    revocation_endpoint: Option<Url>,
    /// Whether the callback must carry `iss` (RFC 9207).
    iss_required: bool,
    /// The scopes asked for: the challenge's, else the resource's.
    scopes: Vec<String>,
}

impl Found {
    /// The MCP server's URL.
    pub(crate) fn server(&self) -> &Url {
        &self.server
    }

    /// The authorization server's issuer.
    pub(crate) fn issuer(&self) -> &str {
        &self.issuer
    }

    /// The scopes asked for, within the caps.
    pub(crate) fn scopes(&self) -> &[String] {
        &self.scopes
    }

    /// The host the sign-in page is on (the authorization endpoint's).
    pub(crate) fn link_host(&self) -> String {
        self.authorization_endpoint
            .host_str()
            .unwrap_or_default()
            .to_owned()
    }

    /// The question's message (design §14 "Stage 4", point 4).
    pub(crate) fn details(&self) -> String {
        let server = self.server.host_str().unwrap_or_default();
        let link = self.link_host();
        let mut lines = vec![
            format!("The MCP server: {server}"),
            format!("The sign-in page: {link}"),
        ];
        let issuer = Url::parse(&self.issuer)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_else(|| self.issuer.clone());
        if issuer != link {
            lines.push(format!(
                "Its issuer: {issuer} (not the sign-in page's host)"
            ));
        }
        if self.scopes.is_empty() {
            lines.push("Scopes asked: none named".into());
        } else {
            lines.push(format!("Scopes asked: {}", self.scopes.join(" ")));
            let broad: Vec<&str> = self
                .scopes
                .iter()
                .map(String::as_str)
                .filter(|s| BROAD_SCOPES.contains(s))
                .collect();
            if !broad.is_empty() {
                lines.push(format!(
                    "Broad: {} (more than one task needs, or access that lasts)",
                    broad.join(", ")
                ));
            }
        }
        lines.push(
            "yobuild registers itself with it as a new client for this sign-in (dynamic registration)."
                .into(),
        );
        lines.push(
            "The token is kept for this session only, in memory; the agent never sees it.".into(),
        );
        lines.join("\n")
    }
}

/// Finds and checks how `server` signs in (design §14 "Stage 4", point 3),
/// from its 401's `WWW-Authenticate` when it sent one. Nothing is asked and
/// nothing is registered: what is refused is refused before the owner is
/// asked.
pub(crate) async fn discover(
    net: &Net,
    server: &Url,
    challenge: Option<&str>,
    site: Site,
) -> Result<Found, Refusal> {
    let refused = |why: Why, said: String| Refusal::new(why, said);
    let at = |said: String| refused(Why::Discovery, said);
    let challenge = challenge.map(Challenge::parse).unwrap_or_default();

    // The resource metadata: the challenge's address, else the well-known
    // ones; on the server's own origin only.
    let named = challenge.param("resource_metadata");
    let candidates = match named {
        Some(named) => vec![Url::parse(named)
            .map_err(|_| at("its challenge names resource metadata that is not a URL".into()))?],
        None => well_known_resource(server),
    };
    let mut resource = None;
    let mut missing = None;
    for candidate in &candidates {
        if candidate.origin() != server.origin() {
            return Err(at(format!(
                "its resource metadata is on another origin ({}): it must be on the server's own",
                candidate.host_str().unwrap_or("no host")
            )));
        }
        let address = net
            .address(candidate.as_str())
            .map_err(|e| at(format!("its resource metadata: {e}")))?;
        match net.get(&address).await {
            Ok(Fetched::Object(map)) => {
                resource = Some((address, map));
                break;
            }
            // Where the challenge points, it must be.
            Ok(Fetched::Missing(status)) if named.is_some() => {
                return Err(at(format!(
                    "its resource metadata, where its challenge points: it answered HTTP {status}"
                )))
            }
            Ok(Fetched::Missing(status)) => missing = Some(status),
            Err(e) => return Err(at(format!("its resource metadata: {e}"))),
        }
    }
    let Some((metadata, resource)) = resource else {
        return Err(refused(Why::NoSignIn, format!(
            "it names no resource metadata (RFC 9728; HTTP {} at its well-known addresses), so no sign-in",
            missing.unwrap_or(404)
        )));
    };
    let field = |e: String| at(format!("its resource metadata: {e}"));
    let named_resource = text(&resource, "resource").map_err(field)?;
    if named_resource.and_then(|r| Url::parse(r).ok()).as_ref() != Some(server) {
        return Err(at(format!(
            "its resource metadata is for {} ({}), not the server's URL ({server})",
            if named_resource.is_some() {
                "another resource"
            } else {
                "no resource"
            },
            clean(named_resource.unwrap_or("none"), SAID_CHARS)
        )));
    }

    // The authorization server: the first entry an address check takes.
    let servers = texts(&resource, "authorization_servers")
        .map_err(field)?
        .unwrap_or_default();
    let mut not_taken = Vec::new();
    let mut chosen = None;
    for entry in &servers {
        match net.address(entry) {
            Ok(url) => {
                chosen = Some((entry.clone(), url));
                break;
            }
            Err(e) => not_taken.push(e),
        }
    }
    let Some((entry, issuer_url)) = chosen else {
        return Err(at(match not_taken.first() {
            None => "its resource metadata names no authorization server".into(),
            Some(first) => format!(
                "its resource metadata names no authorization server yobuild takes (the first: {first}{})",
                match not_taken.len() {
                    1 => String::new(),
                    n => format!("; {} more refused", n - 1),
                }
            ),
        }));
    };
    let issuer_host = issuer_url.host_str().unwrap_or_default().to_owned();
    let own_site = match site {
        Site::Any => None,
        Site::Own => {
            let host = server.host_str().unwrap_or_default();
            Some(net.site(host).ok_or_else(|| {
                refused(
                    Why::OtherSite,
                    format!("the server's own site cannot be told ({host} is a public suffix): a sign-in for a server the agent found must be on it"),
                )
            })?)
        }
    };
    // For a server the agent found, each part of its sign-in on its site.
    let on_site = |what: &str, url: &Url| -> Result<(), Refusal> {
        let Some(own) = &own_site else {
            return Ok(());
        };
        let host = url.host_str().unwrap_or_default();
        if net.site(host).as_ref() == Some(own) {
            return Ok(());
        }
        Err(refused(
            Why::OtherSite,
            format!("its sign-in's {what} is at another site ({host}), which a server the agent found may not name"),
        ))
    };
    on_site("authorization server", &issuer_url)?;

    // Its metadata, in the specification's order; its issuer the entry.
    let mut metadata_of = None;
    let mut missing = None;
    for candidate in well_known_authorization(&issuer_url) {
        match net.get(&candidate).await {
            Ok(Fetched::Object(map)) => {
                metadata_of = Some(map);
                break;
            }
            Ok(Fetched::Missing(status)) => missing = Some(status),
            Err(e) => {
                return Err(at(format!(
                    "the authorization server's metadata ({issuer_host}): {e}"
                )))
            }
        }
    }
    let Some(server_metadata) = metadata_of else {
        return Err(at(format!(
            "the authorization server ({issuer_host}) publishes no metadata (RFC 8414; HTTP {} at its well-known addresses)",
            missing.unwrap_or(404)
        )));
    };
    let field = |e: String| at(format!("the authorization server's metadata: {e}"));
    if text(&server_metadata, "issuer").map_err(field)? != Some(entry.as_str()) {
        return Err(at(format!(
            "the authorization server's metadata names another issuer than {issuer_host}"
        )));
    }
    let endpoint = |key: &str| -> Result<Option<Url>, Refusal> {
        match text(&server_metadata, key).map_err(field)? {
            None => Ok(None),
            Some(url) => {
                let url = net
                    .address(url)
                    .map_err(|e| at(format!("its `{key}`: {e}")))?;
                on_site(&format!("`{key}`"), &url)?;
                Ok(Some(url))
            }
        }
    };
    let authorization_endpoint = endpoint("authorization_endpoint")?
        .ok_or_else(|| at("its metadata names no `authorization_endpoint`".into()))?;
    let token_endpoint = endpoint("token_endpoint")?
        .ok_or_else(|| at("its metadata names no `token_endpoint`".into()))?;
    let methods = texts(&server_metadata, "code_challenge_methods_supported")
        .map_err(field)?
        .unwrap_or_default();
    if !methods.iter().any(|m| m == "S256") {
        return Err(refused(
            Why::NoPkce,
            format!("its sign-in ({issuer_host}) offers no PKCE (`S256`), which yobuild requires"),
        ));
    }
    let registration_endpoint = endpoint("registration_endpoint")?.ok_or_else(|| {
        refused(
            Why::NoRegistration,
            format!("its sign-in ({issuer_host}) offers no way for yobuild to register (no `registration_endpoint`)"),
        )
    })?;
    let revocation_endpoint = endpoint("revocation_endpoint")?;
    let iss_required = match server_metadata.get("authorization_response_iss_parameter_supported") {
        None | Some(Json::Null) => false,
        Some(Json::Bool(b)) => *b,
        Some(_) => {
            return Err(field(
                "its `authorization_response_iss_parameter_supported` is not true or false".into(),
            ))
        }
    };

    // The scopes: the challenge's, else the resource's.
    let scopes: Vec<String> = match challenge.param("scope") {
        Some(scope) => scope.split_whitespace().map(str::to_owned).collect(),
        None => texts(&resource, "scopes_supported")
            .map_err(|e| at(format!("its resource metadata: {e}")))?
            .unwrap_or_default(),
    };
    check_scopes(&scopes).map_err(at)?;

    Ok(Found {
        server: server.clone(),
        metadata,
        issuer: entry,
        authorization_endpoint,
        token_endpoint,
        registration_endpoint,
        revocation_endpoint,
        iss_required,
        scopes,
    })
}

/// The scopes a server asks for, within the caps: at most [`MAX_SCOPES`],
/// each 1 to [`MAX_SCOPE_CHARS`] characters of what a scope may hold (RFC
/// 6749 §3.3).
fn check_scopes(scopes: &[String]) -> Result<(), String> {
    if scopes.len() > MAX_SCOPES {
        return Err(format!(
            "it asks for {} scopes, more than {MAX_SCOPES}",
            scopes.len()
        ));
    }
    for scope in scopes {
        let fits = |c: char| c == '!' || ('#'..='[').contains(&c) || (']'..='~').contains(&c);
        let chars = scope.chars().count();
        if chars == 0 || chars > MAX_SCOPE_CHARS || !scope.chars().all(fits) {
            return Err(format!(
                "it asks for a scope that is not one (1 to {MAX_SCOPE_CHARS} characters of what a scope may hold)"
            ));
        }
    }
    Ok(())
}

/// The resource metadata's well-known addresses (RFC 9728 §3): at the
/// endpoint's path, then at the root.
fn well_known_resource(server: &Url) -> Vec<Url> {
    let mut root = server.clone();
    root.set_query(None);
    root.set_fragment(None);
    let path = server.path().trim_end_matches('/').to_owned();
    let mut found = Vec::new();
    if !path.is_empty() {
        let mut at_path = root.clone();
        at_path.set_path(&format!("/.well-known/oauth-protected-resource{path}"));
        found.push(at_path);
    }
    root.set_path("/.well-known/oauth-protected-resource");
    found.push(root);
    found
}

/// An authorization server's metadata addresses, in the MCP
/// specification's order: RFC 8414's, then OpenID Connect discovery's,
/// with the issuer's path inserted, then appended.
fn well_known_authorization(issuer: &Url) -> Vec<Url> {
    let mut base = issuer.clone();
    base.set_query(None);
    base.set_fragment(None);
    let path = issuer.path().trim_end_matches('/').to_owned();
    let at = |path: String| {
        let mut url = base.clone();
        url.set_path(&path);
        url
    };
    if path.is_empty() {
        vec![
            at("/.well-known/oauth-authorization-server".into()),
            at("/.well-known/openid-configuration".into()),
        ]
    } else {
        vec![
            at(format!("/.well-known/oauth-authorization-server{path}")),
            at(format!("/.well-known/openid-configuration{path}")),
            at(format!("{path}/.well-known/openid-configuration")),
        ]
    }
}

/// How the sign-in question is asked.
pub(crate) struct Ask<'a> {
    pub(crate) session: &'a Session,
    /// Its title (`Sign in to io.x/docs (mcp.x.example) at auth.x.example?
    /// (asked by the agent)`).
    pub(crate) title: String,
    /// How long it waits: [`crate::frontend::session::SIGN_IN_TIMEOUT`].
    pub(crate) timeout: Duration,
    /// Phone mode (design §14 "Stage 4", point 5): the link is not on the
    /// page but given to this, which prints it on yobuild's machine.
    pub(crate) printed: Option<&'a (dyn Fn(&str) + Sync)>,
}

/// A duration as the owner reads it: seconds below two minutes.
fn duration_text(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds < 120 {
        format!("{seconds} s")
    } else {
        format!("{} minutes", seconds / 60)
    }
}

/// Signs in (design §14 "Stage 4", points 5–8): opens the callback's
/// listener, registers a client, asks the sign-in question with the short
/// start link, and once the browser comes back exchanges its code. `stop`
/// (the run's cancellation) ends it at any step; tokens an exchange it cut
/// short still gets are revoked.
pub(crate) async fn sign_in(
    net: &Net,
    found: &Found,
    ask: Ask<'_>,
    stop: &CancellationToken,
) -> Result<Arc<Source>, Refusal> {
    let stopped =
        |during: &str| Refusal::new(Why::Stopped, format!("the run was stopped while {during}"));
    let listener = Listener::bind().await?;
    let redirect_uri = listener.redirect_uri();
    let client_id = tokio::select! {
        _ = stop.cancelled() => return Err(stopped("yobuild registered with its sign-in")),
        registered = register(net, found, &redirect_uri) => registered?,
    };
    let verifier = Secret::random()?;
    let state = Secret::random()?;
    let authorize = authorization_url(found, &client_id, &redirect_uri, &verifier, &state);
    let mut callback = listener.serve(authorize, state, found.issuer.clone(), found.iss_required);
    let link = callback.start_link();
    let url = match ask.printed {
        Some(print) => {
            print(&link);
            None
        }
        None => Some(link),
    };
    let question = ask.session.ask_or_why(
        UiRequest::SignIn {
            title: ask.title,
            message: found.details(),
            host: found.link_host(),
            url,
        },
        ask.timeout,
    );
    // The question is dropped — withdrawn from every frontend — however
    // this ends: the browser came back, the run stopped.
    let code = tokio::select! {
        _ = stop.cancelled() => return Err(stopped("it waited for the sign-in")),
        came = callback.came_back() => came?,
        answer = question => return Err(match answer {
            Ok(_) => Refusal::new(Why::Cancelled, "the owner cancelled the sign-in"),
            Err(Unanswered::TimedOut) => Refusal::new(
                Why::TimedOut,
                format!("nobody signed in within {}", duration_text(ask.timeout)),
            ),
            Err(Unanswered::NoFrontend) => Refusal::new(
                Why::NoFrontend,
                "nobody is here to sign in: open the page or the terminal UI",
            ),
            Err(Unanswered::SessionEnded) => {
                Refusal::new(Why::SessionEnded, "the session ended during the sign-in")
            }
        }),
    };
    drop(callback);
    // On a task of its own: a run stopped meanwhile lets it finish, and
    // revokes what it gets.
    let mut exchanging = {
        let (net, found, client_id) = (net.clone(), found.clone(), client_id.clone());
        tokio::spawn(async move {
            exchange(&net, &found, &client_id, &redirect_uri, &code, &verifier).await
        })
    };
    let finished = tokio::select! {
        _ = stop.cancelled() => None,
        joined = &mut exchanging => Some(joined),
    };
    let tokens = match finished {
        Some(Ok(tokens)) => tokens?,
        Some(Err(_)) => {
            return Err(Refusal::new(
                Why::Exchange,
                "the exchange of the sign-in's code failed on this machine",
            ))
        }
        None => {
            let (net, found, client_id) = (net.clone(), found.clone(), client_id.clone());
            tokio::spawn(async move {
                if let Ok(Ok(tokens)) = exchanging.await {
                    let source = Source::new(net, &found, client_id, tokens);
                    for failure in source.revoke().await {
                        tracing::warn!("a sign-in the run stopped: {failure}");
                    }
                }
            });
            return Err(stopped("the sign-in's code was exchanged"));
        }
    };
    Ok(Arc::new(Source::new(net.clone(), found, client_id, tokens)))
}

/// Registers yobuild as a new public client for this sign-in (design §14
/// "Stage 4", point 7): its `client_id`, the answer checked; a secret it
/// hands out is dropped unread.
async fn register(net: &Net, found: &Found, redirect_uri: &str) -> Result<String, Refusal> {
    let at = |said: String| Refusal::new(Why::Registration, said);
    let host = found.registration_endpoint.host_str().unwrap_or_default();
    let request = serde_json::json!({
        "client_name": "yobuild",
        "redirect_uris": [redirect_uri],
        "token_endpoint_auth_method": "none",
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
    });
    let (status, answer) = net
        .post(&found.registration_endpoint, Body::Json(request))
        .await
        .map_err(|e| at(format!("its registration ({host}): {e}")))?;
    if !(200..300).contains(&status) {
        return Err(at(format!(
            "its registration ({host}) refused yobuild (HTTP {status}{})",
            oauth_error(answer.as_ref(), &[])
        )));
    }
    let answer = answer.ok_or_else(|| {
        at(format!(
            "its registration ({host}): its answer is not a JSON object"
        ))
    })?;
    let field = |e: String| at(format!("its registration ({host}): {e}"));
    let client_id = text(&answer, "client_id")
        .map_err(field)?
        .ok_or_else(|| at(format!("its registration ({host}) gave no `client_id`")))?;
    let plain = |c: char| c.is_ascii_graphic() || c == ' ';
    if client_id.is_empty() || client_id.len() > MAX_CLIENT_ID || !client_id.chars().all(plain) {
        return Err(at(format!(
            "its registration ({host}) gave a `client_id` that is not one (printable ASCII, at most {MAX_CLIENT_ID})"
        )));
    }
    if let Some(uris) = texts(&answer, "redirect_uris").map_err(field)? {
        if uris != [redirect_uri] {
            return Err(at(format!(
                "its registration ({host}) answered with other `redirect_uris` than yobuild's"
            )));
        }
    }
    if let Some(method) = text(&answer, "token_endpoint_auth_method").map_err(field)? {
        if method != "none" {
            return Err(at(format!(
                "its registration ({host}) made a client that must authenticate (`{}`), not a public one",
                if is_error_code(method) { method } else { "another method" }
            )));
        }
    }
    Ok(client_id.to_owned())
}

/// An OAuth refusal's `error` and `error_description`, said as `: code —
/// description` (the code when it is one, the description cleaned and cut,
/// each of `secrets` in it shown as [`SHOWN`]); nothing when it has
/// neither.
fn oauth_error(answer: Option<&Map<String, Json>>, secrets: &[&Secret]) -> String {
    let get = |key: &str| answer.and_then(|a| a.get(key)).and_then(Json::as_str);
    let code = get("error").filter(|code| is_error_code(code));
    let description = get("error_description")
        .map(|d| {
            let hidden = secrets
                .iter()
                .fold(d.to_owned(), |d, s| d.replace(s.expose(), SHOWN));
            clean(&hidden, SAID_CHARS)
        })
        .filter(|d| !d.is_empty());
    match (code, description) {
        (Some(code), Some(d)) => format!(": {code} — {d}"),
        (Some(code), None) => format!(": {code}"),
        (None, Some(d)) => format!(": {d}"),
        (None, None) => String::new(),
    }
}

/// Whether `code` is an OAuth error code: 1 to 64 of `%x20-21 / %x23-5B /
/// %x5D-7E` (RFC 6749 §4.1.2.1), no space either (none uses one).
fn is_error_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 64
        && code
            .chars()
            .all(|c| c == '!' || ('#'..='[').contains(&c) || (']'..='~').contains(&c))
}

/// The authorization request's URL: the endpoint with the code flow's
/// parameters, PKCE's challenge and the resource (RFC 8707).
fn authorization_url(
    found: &Found,
    client_id: &str,
    redirect_uri: &str,
    verifier: &Secret,
    state: &Secret,
) -> Url {
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.expose().as_bytes()));
    let mut url = found.authorization_endpoint.clone();
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("response_type", "code")
            .append_pair("client_id", client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", state.expose())
            .append_pair("resource", found.server.as_str());
        if !found.scopes.is_empty() {
            query.append_pair("scope", &found.scopes.join(" "));
        }
    }
    url
}

/// The tokens a sign-in holds. A token's lifetime (`expires_in`) is not
/// kept: a 401 says it ended.
#[derive(Debug, Clone)]
struct Tokens {
    access: Secret,
    refresh: Option<Secret>,
}

/// A token answer read (design §14 "Stage 4", point 8): a `Bearer`
/// `token_type` in any case, an `access_token` of visible ASCII, a
/// `refresh_token` kept when given. A refusal says its OAuth error, each of
/// `secrets` (what was sent) hidden; a success is never quoted.
fn tokens_of(
    status: u16,
    answer: Option<Map<String, Json>>,
    host: &str,
    secrets: &[&Secret],
) -> Result<(Secret, Option<Secret>), String> {
    if !(200..300).contains(&status) {
        return Err(format!(
            "its token endpoint ({host}) refused it (HTTP {status}{})",
            oauth_error(answer.as_ref(), secrets)
        ));
    }
    let Some(answer) = answer else {
        return Err(format!(
            "its token endpoint ({host}): its answer is not a JSON object"
        ));
    };
    let field = |key: &str| match answer.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(s)) => Ok(Some(s.as_str())),
        Some(_) => Err(format!(
            "its token endpoint ({host}): its `{key}` is not text"
        )),
    };
    if !field("token_type")?.is_some_and(|t| t.eq_ignore_ascii_case("bearer")) {
        return Err(format!(
            "its token endpoint ({host}) gave no `Bearer` token"
        ));
    }
    let visible = |t: &str| !t.is_empty() && t.chars().all(|c| c.is_ascii_graphic());
    let access = field("access_token")?
        .filter(|t| visible(t))
        .ok_or_else(|| format!("its token endpoint ({host}) gave no token yobuild can send"))?;
    let refresh = match field("refresh_token")? {
        None => None,
        Some(t) if visible(t) => Some(Secret(t.to_owned())),
        Some(_) => {
            return Err(format!(
                "its token endpoint ({host}) gave a refresh token yobuild cannot send"
            ))
        }
    };
    Ok((Secret(access.to_owned()), refresh))
}

/// Exchanges the code for the tokens, at the endpoint discovery checked.
async fn exchange(
    net: &Net,
    found: &Found,
    client_id: &str,
    redirect_uri: &str,
    code: &Secret,
    verifier: &Secret,
) -> Result<Tokens, Refusal> {
    let host = found.token_endpoint.host_str().unwrap_or_default();
    let at = |said: String| Refusal::new(Why::Exchange, said);
    let form = vec![
        ("grant_type", "authorization_code".to_owned()),
        ("code", code.expose().to_owned()),
        ("redirect_uri", redirect_uri.to_owned()),
        ("client_id", client_id.to_owned()),
        ("code_verifier", verifier.expose().to_owned()),
        ("resource", found.server.to_string()),
    ];
    let (status, answer) = net
        .post(&found.token_endpoint, Body::Form(form))
        .await
        .map_err(|e| at(format!("its token endpoint ({host}): {e}")))?;
    let (access, refresh) = tokens_of(status, answer, host, &[code, verifier]).map_err(at)?;
    Ok(Tokens { access, refresh })
}

/// The callback's listener, bound before registering: its address is the
/// `redirect_uri`.
struct Listener {
    listener: tokio::net::TcpListener,
    port: u16,
    callback: Secret,
    start: Secret,
}

impl Listener {
    async fn bind() -> Result<Listener, Refusal> {
        let local = |e: std::io::Error| {
            Refusal::new(
                Why::Local,
                format!("the sign-in's callback could not listen on 127.0.0.1: {e}"),
            )
        };
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(local)?;
        let port = listener.local_addr().map_err(local)?.port();
        Ok(Listener {
            listener,
            port,
            callback: Secret::random()?,
            start: Secret::random()?,
        })
    }

    fn redirect_uri(&self) -> String {
        format!(
            "http://127.0.0.1:{}/callback/{}",
            self.port,
            self.callback.expose()
        )
    }

    /// Serves the start link (a redirect to `authorize`) and the callback
    /// until the browser comes back with the matching `state`.
    fn serve(self, authorize: Url, state: Secret, issuer: String, iss_required: bool) -> Callback {
        let (tx, rx) = oneshot::channel();
        let done = Arc::new(Notify::new());
        let shared = Arc::new(Serving {
            authorize,
            state,
            issuer,
            iss_required,
            result: Mutex::new(Some(tx)),
            done: done.clone(),
        });
        let start_path = format!("/start/{}", self.start.expose());
        let callback_path = format!("/callback/{}", self.callback.expose());
        let at_start = shared.clone();
        let app = axum::Router::new()
            .route(
                &start_path,
                axum::routing::get(move || {
                    let serving = at_start.clone();
                    async move { serving.start() }
                }),
            )
            .route(
                &callback_path,
                axum::routing::get(move |query: axum::extract::RawQuery| {
                    let serving = shared.clone();
                    async move { serving.callback(query.0.as_deref().unwrap_or("")) }
                }),
            );
        let listener = self.listener;
        let task = tokio::spawn(async move {
            let served = axum::serve(listener, app)
                .with_graceful_shutdown(async move { done.notified().await })
                .await;
            if let Err(e) = served {
                tracing::warn!("the sign-in's callback stopped: {e}");
            }
        });
        Callback {
            start: format!("http://127.0.0.1:{}{start_path}", self.port),
            result: rx,
            task,
        }
    }
}

/// What the callback's handlers share.
struct Serving {
    authorize: Url,
    state: Secret,
    issuer: String,
    iss_required: bool,
    /// Taken by the first request with the matching `state`.
    result: Mutex<Option<oneshot::Sender<Result<Secret, Refusal>>>>,
    done: Arc<Notify>,
}

/// The page the browser is left on: no code, nothing from the server.
fn page(status: axum::http::StatusCode, text: &'static str) -> axum::response::Response {
    use axum::response::IntoResponse;
    (
        status,
        [
            ("content-type", "text/plain; charset=utf-8"),
            ("cache-control", "no-store"),
            ("referrer-policy", "no-referrer"),
        ],
        text,
    )
        .into_response()
}

/// The page once this sign-in is over.
const OVER: &str = "This sign-in is over: go back to yobuild.";

impl Serving {
    fn start(&self) -> axum::response::Response {
        use axum::http::StatusCode;
        if locked(&self.result).is_none() {
            return page(StatusCode::GONE, OVER);
        }
        let Ok(location) = axum::http::HeaderValue::from_str(self.authorize.as_str()) else {
            return page(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The sign-in page's address cannot be sent on: go back to yobuild.",
            );
        };
        let mut response = page(StatusCode::FOUND, "Sending you on to the sign-in page.");
        response.headers_mut().insert("location", location);
        response
    }

    /// The browser back (design §14 "Stage 4", point 6): only the matching
    /// `state` counts; then its `iss`, its `error` or its `code`, each
    /// given once.
    fn callback(&self, query: &str) -> axum::response::Response {
        use axum::http::StatusCode;
        let (mut state, mut code, mut error, mut description, mut iss) =
            (None, None, None, None, None);
        let mut twice: Vec<&'static str> = Vec::new();
        for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
            let (slot, name) = match name.as_ref() {
                "state" => (&mut state, "state"),
                "code" => (&mut code, "code"),
                "error" => (&mut error, "error"),
                "error_description" => (&mut description, "error_description"),
                "iss" => (&mut iss, "iss"),
                _ => continue,
            };
            if slot.is_some() {
                twice.push(name);
            }
            *slot = Some(value.into_owned());
        }
        // A `state` given twice matches nothing: which one counts is
        // unclear.
        if twice.contains(&"state") || state.as_deref() != Some(self.state.expose()) {
            return page(StatusCode::BAD_REQUEST, "Not this sign-in.");
        }
        let Some(sender) = locked(&self.result).take() else {
            return page(StatusCode::GONE, OVER);
        };
        let refused = |said: String| Err(Refusal::new(Why::Callback, said));
        let result = if let Some(name) = twice.iter().find(|n| **n != "error_description") {
            refused(format!("the browser came back with `{name}` twice"))
        } else {
            match (iss, self.iss_required) {
                (Some(iss), _) if iss != self.issuer => refused(
                    "the browser came back from another issuer than the sign-in's (`iss`)".into(),
                ),
                (None, true) => {
                    refused("the browser came back without the issuer it must name (`iss`)".into())
                }
                _ => match (error, code) {
                    (Some(error), _) => {
                        let code = if is_error_code(&error) {
                            error.as_str()
                        } else {
                            "an error that is no OAuth error code"
                        };
                        let description = description
                            .filter(|_| !twice.contains(&"error_description"))
                            .map(|d| clean(&d, SAID_CHARS))
                            .filter(|d| !d.is_empty());
                        refused(match description {
                            Some(d) => format!("the sign-in ended with {code}: {d}"),
                            None => format!("the sign-in ended with {code}"),
                        })
                    }
                    (None, Some(code)) if !code.is_empty() => Ok(Secret(code)),
                    _ => refused("the browser came back with no code".into()),
                },
            }
        };
        let ok = result.is_ok();
        let heard = sender.send(result).is_ok();
        self.done.notify_one();
        match (heard, ok) {
            (false, _) => page(StatusCode::GONE, OVER),
            (true, true) => page(
                StatusCode::OK,
                "Back from the sign-in: go back to yobuild, which finishes it.",
            ),
            (true, false) => page(
                StatusCode::OK,
                "The sign-in did not complete: go back to yobuild.",
            ),
        }
    }
}

/// `text` as one plain line of at most `max` characters: control and bidi
/// characters dropped, white space joined (what a server says, shown to the
/// owner).
fn clean(text: &str, max: usize) -> String {
    let bidi = |c: char| matches!(c, '\u{200e}' | '\u{200f}' | '\u{061c}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}');
    let kept: String = text
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|c| !c.is_control() && !bidi(*c))
        .collect();
    let joined = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.chars().count() <= max {
        return joined;
    }
    let mut cut: String = joined.chars().take(max).collect();
    cut.push('…');
    cut
}

/// The callback being served; dropping it closes its listener (a browser's
/// connection still open is answered [`OVER`] once the sign-in ended).
struct Callback {
    start: String,
    result: oneshot::Receiver<Result<Secret, Refusal>>,
    task: tokio::task::JoinHandle<()>,
}

impl Callback {
    /// The short link the owner opens: `http://127.0.0.1:<port>/start/…`.
    fn start_link(&self) -> String {
        self.start.clone()
    }

    /// The code, once the browser came back with the matching `state`.
    async fn came_back(&mut self) -> Result<Secret, Refusal> {
        match (&mut self.result).await {
            Ok(result) => result,
            Err(_) => Err(Refusal::new(
                Why::Local,
                "the sign-in's callback stopped answering (the log says why)",
            )),
        }
    }
}

impl Drop for Callback {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Why a refresh failed.
enum RefreshFailed {
    /// Refused, or its answer: the sign-in ends.
    Refused(String),
    /// Not reached, or a server's passing trouble (408, 429, 5xx): this
    /// request fails, the sign-in stays.
    Now(String),
}

/// The token for a signed-in server's requests, through yoagent's
/// `with_authorization` (design §14 "Stage 4", points 9 and 10): the
/// current access token; on a 401 a refresh at the pinned token endpoint,
/// one at a time; ended for good when there is no refresh token or one is
/// refused.
pub(crate) struct Source {
    net: Net,
    server: Url,
    /// The resource metadata a challenge may name: the pinned one, or
    /// another of the server's well-known addresses.
    metadata: Vec<Url>,
    token_endpoint: Url,
    revocation_endpoint: Option<Url>,
    client_id: String,
    tokens: tokio::sync::Mutex<Tokens>,
    /// Every token handed out, for [`Source::hidden`].
    seen: Mutex<Vec<Secret>>,
    /// Why the sign-in ended, once it has.
    ended: Mutex<Option<String>>,
    /// Set as the server is closed: its `DELETE`'s 401 gets no refresh.
    closing: AtomicBool,
}

impl Source {
    fn new(net: Net, found: &Found, client_id: String, tokens: Tokens) -> Source {
        let seen = std::iter::once(tokens.access.clone())
            .chain(tokens.refresh.clone())
            .collect();
        let mut metadata = vec![found.metadata.clone()];
        metadata.extend(well_known_resource(&found.server));
        Source {
            net,
            server: found.server.clone(),
            metadata,
            token_endpoint: found.token_endpoint.clone(),
            revocation_endpoint: found.revocation_endpoint.clone(),
            client_id,
            tokens: tokio::sync::Mutex::new(tokens),
            seen: Mutex::new(seen),
            ended: Mutex::new(None),
            closing: AtomicBool::new(false),
        }
    }

    /// Why the sign-in ended, once it has (every request fails with
    /// [`ENDED`] from then on): for the caller to say, and to mark a loaded
    /// server stopped.
    pub(crate) fn ended(&self) -> Option<String> {
        locked(&self.ended).clone()
    }

    /// Each token handed out so far, as [`SHOWN`], for `run::hidden`:
    /// longest first.
    pub(crate) fn hidden(&self) -> Vec<(String, String)> {
        let mut hide: Vec<(String, String)> = locked(&self.seen)
            .iter()
            .map(|s| (s.expose().to_owned(), SHOWN.to_owned()))
            .collect();
        hide.sort_by_key(|(value, _)| std::cmp::Reverse(value.len()));
        hide
    }

    /// The server is being closed: a 401 from now on gets no refresh.
    pub(crate) fn closing(&self) {
        self.closing.store(true, Ordering::SeqCst);
    }

    /// Revokes the tokens where the metadata offers revocation (RFC 7009;
    /// with none, nothing to do), the refresh token first, each request
    /// within [`REQUEST_TIME`]. Returns what failed, for the caller to say
    /// (a token that outlives the session at its provider). Called after
    /// the server is closed.
    pub(crate) async fn revoke(&self) -> Vec<String> {
        self.revoke_within(REQUEST_TIME).await
    }

    /// [`Source::revoke`], each request within `each`.
    pub(crate) async fn revoke_within(&self, each: Duration) -> Vec<String> {
        self.closing();
        let Some(endpoint) = &self.revocation_endpoint else {
            return Vec::new();
        };
        let host = endpoint.host_str().unwrap_or_default();
        let tokens = self.tokens.lock().await.clone();
        let mut revoked = Vec::new();
        if let Some(refresh) = &tokens.refresh {
            revoked.push((refresh, "refresh_token", "refresh token"));
        }
        revoked.push((&tokens.access, "access_token", "access token"));
        let mut failures = Vec::new();
        for (token, hint, what) in revoked {
            let form = vec![
                ("token", token.expose().to_owned()),
                ("token_type_hint", hint.to_owned()),
                ("client_id", self.client_id.clone()),
            ];
            match tokio::time::timeout(each, self.net.post(endpoint, Body::Form(form))).await {
                Ok(Ok((status, _))) if (200..300).contains(&status) => {}
                Ok(Ok((status, _))) => failures.push(format!(
                    "the sign-in's {what} was not revoked at {host} (HTTP {status})"
                )),
                Ok(Err(e)) => failures.push(format!(
                    "the sign-in's {what} was not revoked at {host}: {e}"
                )),
                Err(_) => failures.push(format!(
                    "the sign-in's {what} was not revoked at {host}: no answer within {} s",
                    each.as_secs()
                )),
            }
        }
        failures
    }

    /// Ends the sign-in for good, saying why to the owner (`ended`); the
    /// request fails with [`ENDED`].
    fn end(&self, why: String) -> McpError {
        let mut ended = locked(&self.ended);
        if ended.is_none() {
            tracing::info!(
                server = self.server.host_str().unwrap_or_default(),
                "a sign-in ended: {why}"
            );
            *ended = Some(why);
        }
        McpError::Transport(ENDED.into())
    }

    /// A refresh at the pinned token endpoint; the rotated refresh token
    /// kept (else the old one).
    async fn refresh(&self, tokens: &Tokens, refresh: &Secret) -> Result<Tokens, RefreshFailed> {
        let host = self.token_endpoint.host_str().unwrap_or_default();
        let form = vec![
            ("grant_type", "refresh_token".to_owned()),
            ("refresh_token", refresh.expose().to_owned()),
            ("client_id", self.client_id.clone()),
            ("resource", self.server.to_string()),
        ];
        let (status, answer) = self
            .net
            .post(&self.token_endpoint, Body::Form(form))
            .await
            .map_err(|e| RefreshFailed::Now(format!("its token endpoint ({host}): {e}")))?;
        if status == 408 || status == 429 || status >= 500 {
            return Err(RefreshFailed::Now(format!(
                "its token endpoint ({host}) answered HTTP {status}"
            )));
        }
        let (access, rotated) = tokens_of(status, answer, host, &[refresh, &tokens.access])
            .map_err(RefreshFailed::Refused)?;
        Ok(Tokens {
            access,
            refresh: rotated.or_else(|| Some(refresh.clone())),
        })
    }

    /// Whether a challenge's `resource_metadata` is this sign-in's.
    fn ours(&self, named: &str) -> bool {
        Url::parse(named).is_ok_and(|url| self.metadata.contains(&url))
    }
}

impl fmt::Debug for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Source")
            .field("server", &self.server.as_str())
            .field("ended", &self.ended())
            .finish_non_exhaustive()
    }
}

/// `Bearer <token>`.
fn bearer(token: &Secret) -> String {
    format!("Bearer {}", token.expose())
}

#[async_trait::async_trait]
impl AuthorizationSource for Source {
    async fn authorization(&self) -> Result<Option<String>, McpError> {
        if self.ended().is_some() {
            return Err(McpError::Transport(ENDED.into()));
        }
        Ok(Some(bearer(&self.tokens.lock().await.access)))
    }

    async fn challenged(&self, challenge: &AuthChallenge) -> Result<Option<String>, McpError> {
        if challenge.status != 401 || self.closing.load(Ordering::SeqCst) {
            return Ok(None);
        }
        // Held across the refresh: requests refused meanwhile wait, then
        // find a value newer than the one they were refused.
        let mut tokens = self.tokens.lock().await;
        if self.ended().is_some() {
            return Err(McpError::Transport(ENDED.into()));
        }
        if let Some(header) = &challenge.www_authenticate {
            let parsed = Challenge::parse(header);
            if parsed.bearer().is_none() {
                return Err(
                    self.end("its server asked for another kind of sign-in than `Bearer`".into())
                );
            }
            if let Some(other) = parsed.param("resource_metadata") {
                if !self.ours(other) {
                    return Err(self.end(
                        "its server pointed at other resource metadata than the sign-in's".into(),
                    ));
                }
            }
        }
        let current = bearer(&tokens.access);
        if challenge.refused() != Some(current.as_str()) {
            return Ok(Some(current));
        }
        let Some(refresh) = tokens.refresh.clone() else {
            return Err(self.end("its token expired and it gave no refresh token".into()));
        };
        match self.refresh(&tokens, &refresh).await {
            Ok(fresh) => {
                {
                    let mut seen = locked(&self.seen);
                    seen.push(fresh.access.clone());
                    if let Some(rotated) = &fresh.refresh {
                        if !seen.contains(rotated) {
                            seen.push(rotated.clone());
                        }
                    }
                }
                *tokens = fresh;
                Ok(Some(bearer(&tokens.access)))
            }
            Err(RefreshFailed::Refused(why)) => {
                Err(self.end(format!("its token could not be refreshed: {why}")))
            }
            Err(RefreshFailed::Now(why)) => Err(McpError::Transport(format!(
                "its sign-in's token could not be refreshed now ({why}): the next call tries again"
            ))),
        }
    }
}
