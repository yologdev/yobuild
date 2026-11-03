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
use yoagent::mcp::{McpClient, McpToolAdapter};
use yoagent::AgentTool;

use super::remote::{connect, Connect, Ending, Header, Unkept};
use super::{close_quietly, tools_problem, Loaded, Server, CLOSE_TIME};
use crate::frontend::session::UI_TIMEOUT;
use crate::frontend::{NoticeLevel, Session, UiRequest};
use crate::run::host::clip;
use crate::run::oauth::{self, Site, Source, Why};

/// A manifest server that signs in, as `run` filled it in.
pub(crate) struct SignsIn {
    pub name: String,
    pub url: url::Url,
    /// Each header: its name, its value as written, and as filled.
    pub headers: Vec<(String, String, String)>,
    /// What each filled value is shown as (`run::hidden`).
    pub hide: Vec<(String, String)>,
    pub allow: Vec<String>,
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

/// Signs in each of `servers` in turn, each outcome a notice.
pub(crate) async fn sign_in_all(signing: Signing, servers: Vec<SignsIn>) {
    for server in servers {
        if signing.stop.is_cancelled() {
            return;
        }
        let (level, said) = match sign_in(&signing, &server).await {
            Ok(said) => (NoticeLevel::Info, said),
            Err(no) => no,
        };
        signing.session.notice(
            level,
            format!("agent.toml's [[mcp]] {}: {said}", server.name),
        );
    }
}

/// What did not come about: the notice's level and words.
type Not = (NoticeLevel, String);

fn error(said: impl Into<String>) -> Not {
    (NoticeLevel::Error, said.into())
}

fn warning(said: impl Into<String>) -> Not {
    (NoticeLevel::Warning, said.into())
}

/// One server: connected as it is first (one answering needs no sign-in),
/// then — its 401 offering one — discovered (`Site::Any`), the owner
/// asked, signed in, connected with its token, its tools judged and its
/// `allow` checked, and offered.
async fn sign_in(signing: &Signing, server: &SignsIn) -> Result<String, Not> {
    let Signing { session, stop, .. } = signing;
    let name = &server.name;
    let headers: Vec<Header> = server
        .headers
        .iter()
        .map(|(header, written, filled)| Header {
            name: header.clone(),
            written: written.clone(),
            filled: filled.clone(),
            variable: None,
        })
        .collect();
    let listed = !headers.is_empty();
    let (challenge, otherwise) = match connect(
        name, &server.url, listed, &headers, None, &server.hide, stop,
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
        Err(Connect::Denied(no) | Connect::Refused(no)) => return Err((no.level, no.message)),
    };
    let net = match &signing.net {
        Some(net) => net.clone(),
        None => oauth::Net::checked().map_err(error)?,
    };
    let found = tokio::select! {
        _ = stop.cancelled() => return Err(warning("not signed in: the session ended")),
        found = oauth::discover(&net, &server.url, challenge.as_deref(), Site::Any) => found,
    };
    let found = match found {
        Ok(found) => found,
        Err(no) if no.why() == Why::NoSignIn => return Err((otherwise.level, otherwise.message)),
        Err(no) => return Err(error(format!("not signed in: {}", no.for_owner()))),
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
        _ = stop.cancelled() => return Err(warning("not signed in: the session ended")),
        answer = session.ask(request, UI_TIMEOUT) => answer,
    };
    match answer.as_str() {
        Some("Allow") => {}
        Some(_) => {
            return Err(warning(
                "not signed in: the owner blocked it, so it offers no tools this session",
            ))
        }
        None => return Err(warning(
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
    let one = signing.loaded.one_sign_in().await;
    let source = oauth::sign_in(&net, &found, ask, stop)
        .await
        .map_err(|no| {
            let level = match no.why() {
                Why::Cancelled
                | Why::TimedOut
                | Why::NoFrontend
                | Why::Stopped
                | Why::SessionEnded => NoticeLevel::Warning,
                _ => NoticeLevel::Error,
            };
            (
                level,
                format!(
                    "not signed in: {}: it offers no tools this session",
                    no.for_owner()
                ),
            )
        })?;
    drop(one);
    let unkept = Unkept::new(source.clone(), name);
    match connect(
        name,
        &server.url,
        listed,
        &headers,
        Some(source.clone()),
        &server.hide,
        stop,
    )
    .await
    {
        Ok((client, tools)) => offer(signing, server, client, tools, Some(unkept))
            .await
            .map(|said| format!("signed in; {said}")),
        Err(Connect::Refused(no) | Connect::Denied(no) | Connect::SignIn { otherwise: no, .. }) => {
            revoke(signing, unkept, name).await;
            Err((no.level, no.message))
        }
    }
}

/// Its tools judged, its `allow` checked against them (a name it does not
/// offer is a Warning: it asks, as any tool does), and offered.
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
    let offered: Vec<Arc<dyn AgentTool>> = tools
        .into_iter()
        .map(|info| {
            Arc::new(Ending {
                inner: McpToolAdapter::new(client.clone(), info)
                    .with_prefix(name.to_owned())
                    .with_call_timeout(Some(McpToolAdapter::DEFAULT_CALL_TIMEOUT)),
                ended: ended.clone(),
                hide: hide.clone(),
                signed_in: source.clone(),
            }) as Arc<dyn AgentTool>
        })
        .collect();
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

/// A sign-in whose server is not offered, revoked; each failure a Warning.
async fn revoke(signing: &Signing, unkept: Unkept, name: &str) {
    let Some(source) = unkept.keep() else {
        return;
    };
    source.closing();
    for failure in source.revoke_within(CLOSE_TIME).await {
        signing.session.notice(
            NoticeLevel::Warning,
            format!(
                "agent.toml's [[mcp]] {name}: {failure}: it may stay valid at its provider until it expires."
            ),
        );
    }
}

#[cfg(test)]
mod tests;
