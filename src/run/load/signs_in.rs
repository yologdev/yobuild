//! The manifest's servers that sign in (`[[mcp]] auth = "oauth"`, design
//! §14 "Stage 4", point 11): signed in once a frontend is attached, each in
//! turn, as a server the agent loads is (`remote.rs`) — but on any site,
//! since the owner wrote it, and asked `for agent.toml's [[mcp]] <name>`.
//! Its tools reach the agent through [`Loaded`] from the next turn, under
//! the manifest's `allow`, checked once they are listed; until then, and
//! after a sign-in refused, it offers none.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use yoagent::mcp::McpClient;

use super::remote::{connect, Connect, Ending, Header, Origin, Unkept};
use super::{close_quietly, tools_problem, Loaded, Refused, Server, CLOSE_TIME};
use crate::frontend::session::{Unanswered, UI_TIMEOUT};
use crate::frontend::{NoticeLevel, Session, UiRequest};
use crate::run::host::clip;
use crate::run::oauth::{self, Site, Source, Why};

/// How long the session's end waits for a sign-in it stopped: a close
/// and a revocation of two tokens, each within [`CLOSE_TIME`], and a
/// second to spare.
pub(crate) const ENDING: Duration = Duration::from_secs(CLOSE_TIME.as_secs() * 3 + 1);

/// A manifest server that signs in, as `run` filled it in.
pub(crate) struct SignsIn {
    pub name: String,
    pub url: url::Url,
    pub headers: Vec<ManifestHeader>,
    /// What each filled value is shown as (`run::hidden`).
    pub hide: Vec<(String, String)>,
    pub allow: Vec<String>,
}

/// One of its headers.
pub(crate) struct ManifestHeader {
    pub name: String,
    /// As agent.toml writes it.
    pub written: String,
    /// As `run` filled it in: what is sent.
    pub filled: String,
}

/// How the sign-ins go: phone mode prints the link here; `net` and
/// `timeout` a test's.
pub(crate) struct Signing {
    pub session: Session,
    pub loaded: Loaded,
    pub phone: bool,
    pub net: Option<oauth::Net>,
    pub timeout: Duration,
    /// The session's end: whatever is left is not signed in.
    pub stop: CancellationToken,
}

/// Signs in each of `servers` in turn, each outcome a notice. A server
/// whose question found no frontend (the page reloaded, closed a moment)
/// waits for one and is asked again.
pub(crate) async fn sign_in_all(signing: Signing, servers: Vec<SignsIn>) {
    for server in servers {
        let outcome = loop {
            if !frontend(&signing).await {
                return;
            }
            match sign_in(&signing, &server).await {
                Err(Not::Again) => {
                    tracing::info!(
                        "agent.toml's [[mcp]] {}: no frontend to ask: asked again once one is there",
                        server.name
                    );
                    tokio::time::sleep(TICK).await;
                }
                outcome => break outcome,
            }
        };
        let (level, said) = match outcome {
            Ok(said) => (NoticeLevel::Info, said),
            Err(Not::Said(level, said)) => (level, said),
            Err(Not::Again) => unreachable!("asked again above"),
        };
        signing.session.notice(
            level,
            format!("agent.toml's [[mcp]] {}: {said}", server.name),
        );
    }
}

/// How often a frontend is looked for.
const TICK: Duration = Duration::from_millis(250);

/// Waits until a frontend is attached: `false` once the session ends.
async fn frontend(signing: &Signing) -> bool {
    loop {
        if signing.stop.is_cancelled() || signing.session.is_closed() {
            return false;
        }
        if signing.session.frontends() > 0 {
            return true;
        }
        tokio::select! {
            _ = signing.stop.cancelled() => return false,
            _ = tokio::time::sleep(TICK) => {}
        }
    }
}

/// What did not come about.
enum Not {
    /// The notice's level and words.
    Said(NoticeLevel, String),
    /// No frontend was there to ask: asked again once one is.
    Again,
}

fn error(said: impl Into<String>) -> Not {
    Not::Said(NoticeLevel::Error, said.into())
}

fn warning(said: impl Into<String>) -> Not {
    Not::Said(NoticeLevel::Warning, said.into())
}

const SESSION_ENDED: &str = "not signed in: the session ended";

/// A connection's refusal, as said: the session's end in its own words
/// (`connect` says a run was stopped), else with what `hide` names hidden.
fn refused(stop: &CancellationToken, no: Refused, hide: &[(String, String)]) -> Not {
    match stop.is_cancelled() {
        true => warning(SESSION_ENDED),
        false => Not::Said(no.level, crate::run::hidden(&no.message, hide)),
    }
}

/// One server: connected as it is first (one answering needs no sign-in),
/// then — a 401 that may offer one (`oauth::may_sign_in`) — discovered
/// (`Site::Any`), the owner asked, signed in, connected with its token,
/// its tools judged and its `allow` checked, and offered. Each refusal of
/// the sign-in's is said with the filled values hidden.
async fn sign_in(signing: &Signing, server: &SignsIn) -> Result<String, Not> {
    let Signing { session, stop, .. } = signing;
    let name = &server.name;
    let hide = &server.hide;
    let headers: Vec<Header> = server
        .headers
        .iter()
        .map(|h| Header::as_written(h.name.clone(), h.written.clone(), h.filled.clone()))
        .collect();
    let (challenge, otherwise) = match connect(
        name,
        &server.url,
        Origin::Manifest,
        &headers,
        None,
        hide,
        stop,
    )
    .await
    {
        Ok((client, tools)) => {
            return offer(signing, server, client, tools, None)
                .await
                .map(|said| format!("it answered without a sign-in; {said}"))
        }
        Err(Connect::SignIn {
            challenge,
            otherwise,
        }) => (challenge, otherwise),
        Err(Connect::Denied(no) | Connect::Refused(no)) => return Err(refused(stop, no, hide)),
    };
    let net = match &signing.net {
        Some(net) => net.clone(),
        None => oauth::Net::checked().map_err(error)?,
    };
    let found = tokio::select! {
        _ = stop.cancelled() => return Err(warning(SESSION_ENDED)),
        found = oauth::discover(&net, &server.url, challenge.as_deref(), Site::Any) => found,
    };
    let owners = |no: &oauth::Refusal| crate::run::hidden(no.for_owner(), hide);
    let found = match found {
        Ok(found) => found,
        Err(no) if no.why() == Why::NoSignIn => return Err(refused(stop, otherwise, hide)),
        Err(no) => return Err(error(format!("not signed in: {}", owners(&no)))),
    };
    let host = server.url.host_str().unwrap_or_default();
    let link = found.link_host();
    let request = UiRequest::Select {
        title: format!("Sign in to {name} ({host}) at {link}? (for agent.toml's [[mcp]] {name})"),
        message: found.details(),
        options: vec!["Allow".into(), "Block".into()],
        multiple: false,
    };
    let answer = tokio::select! {
        _ = stop.cancelled() => return Err(warning(SESSION_ENDED)),
        answer = session.ask_or_why(request, UI_TIMEOUT) => answer,
    };
    match answer {
        Ok(answer) if answer.as_str() == Some("Allow") => {}
        Ok(answer) if answer.is_string() => {
            return Err(warning(
                "not signed in: the owner blocked it, so it offers no tools this session",
            ))
        }
        Err(Unanswered::NoFrontend) => return Err(Not::Again),
        Err(Unanswered::SessionEnded) => return Err(warning(SESSION_ENDED)),
        Ok(_) | Err(Unanswered::TimedOut) => return Err(warning(
            "not signed in: nobody answered whether to sign in, so it offers no tools this session",
        )),
    }
    let print = |link: &str| {
        eprintln!(
            "yobuild: to sign in to agent.toml's [[mcp]] {name} ({host}), open this link in a browser on this machine: {link}"
        )
    };
    let ask = oauth::Ask {
        session,
        title: format!("Sign in to {name} at {link} in your browser"),
        timeout: signing.timeout,
        printed: signing.phone.then_some(&print as &(dyn Fn(&str) + Sync)),
    };
    let waiting = || {
        session.notice(
            NoticeLevel::Info,
            format!("agent.toml's [[mcp]] {name}: its sign-in waits for another sign-in to end."),
        )
    };
    let Some(one) = signing.loaded.one_sign_in(stop, waiting).await else {
        return Err(warning(SESSION_ENDED));
    };
    let source = oauth::sign_in(&net, &found, ask, stop)
        .await
        .map_err(|no| {
            let level = match no.why() {
                Why::NoFrontend => return Not::Again,
                Why::Stopped | Why::SessionEnded => return warning(SESSION_ENDED),
                Why::Cancelled | Why::TimedOut => NoticeLevel::Warning,
                _ => NoticeLevel::Error,
            };
            Not::Said(
                level,
                format!(
                    "not signed in: {}: it offers no tools this session",
                    owners(&no)
                ),
            )
        })?;
    drop(one);
    let unkept = Unkept::new(source.clone(), name);
    match connect(
        name,
        &server.url,
        Origin::Manifest,
        &headers,
        Some(source.clone()),
        hide,
        stop,
    )
    .await
    {
        Ok((client, tools)) => offer(signing, server, client, tools, Some(unkept))
            .await
            .map(|said| format!("signed in; {said}")),
        Err(Connect::Refused(no) | Connect::Denied(no) | Connect::SignIn { otherwise: no, .. }) => {
            revoke(signing, unkept, name).await;
            Err(refused(stop, no, hide))
        }
    }
}

/// Its tools judged, its `allow` checked against them (a name it does not
/// offer is a Warning: no tool of that name comes from it), and offered.
async fn offer(
    signing: &Signing,
    server: &SignsIn,
    client: McpClient,
    tools: Vec<yoagent::mcp::McpToolInfo>,
    signed_in: Option<Unkept>,
) -> Result<String, Not> {
    let name = &server.name;
    let names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
    let own = match tools_problem(name, &names) {
        Ok(own) => own,
        Err(e) => {
            if let Some(source) = signed_in.as_ref().and_then(Unkept::source) {
                source.closing();
            }
            close_quietly(&client, name).await;
            if let Some(unkept) = signed_in {
                revoke(signing, unkept, name).await;
            }
            return Err(error(e));
        }
    };
    for allowed in server.allow.iter().filter(|a| !own.contains(a)) {
        signing.session.notice(
            NoticeLevel::Warning,
            format!(
                "agent.toml's [[mcp]] {name} allows `{allowed}`, which it does not offer (it offers: {}): nothing of that name runs",
                clip(&own.join(", "), 200)
            ),
        );
    }
    let source: Option<Arc<Source>> = signed_in.and_then(Unkept::keep);
    let client = Arc::new(tokio::sync::Mutex::new(client));
    let ended: Arc<Mutex<Option<String>>> = Arc::default();
    let hide = Arc::new(server.hide.clone());
    let offered = Ending::tools(&client, tools, name, &ended, &hide, &source);
    let names: Vec<String> = offered.iter().map(|t| t.name().to_owned()).collect();
    signing.loaded.add_server(Server::remote(
        name.to_owned(),
        offered,
        client,
        ended,
        source,
    ));
    Ok(format!(
        "its tools are the agent's from its next step: {}",
        names.join(", ")
    ))
}

/// A sign-in whose server is not offered, revoked; each failure a Warning
/// and logged (at the session's end nobody may be there to read it), and
/// written on stderr then.
async fn revoke(signing: &Signing, unkept: Unkept, name: &str) {
    let Some(source) = unkept.keep() else {
        return;
    };
    source.closing();
    for failure in source.revoke_within(CLOSE_TIME).await {
        let said = format!(
            "agent.toml's [[mcp]] {name}: {failure}: it may stay valid at its provider until it expires."
        );
        tracing::warn!("{said}");
        if signing.stop.is_cancelled() {
            eprintln!("yobuild: {said}");
        }
        signing.session.notice(NoticeLevel::Warning, said);
    }
}

#[cfg(test)]
mod tests;
