//! Loading a remote MCP server `find_plugin` returned (design §14 "Stage
//! 3", points 3–9, and "Stage 4"): nothing of it runs here. The owner is
//! asked to connect to it; it is connected with no credential first when
//! its entry lists no required header, and signed in through the owner's
//! browser when it asks for that ([`crate::run::oauth`]); else asked, for
//! each header it would send that takes a credential, which environment
//! variable holds it — never the value, never the agent's choice — then
//! connected over streamable HTTP with no redirect followed, its tools
//! listed, and the owner asked again with them in front of them.

use std::ffi::OsString;
use std::sync::{Arc, Mutex};

use serde_json::Value as Json;
use yoagent::mcp::{HttpTransport, McpClient, McpError, McpToolAdapter, McpToolInfo, Redirects};
use yoagent::{AgentTool, ToolContext, ToolError, ToolResult};

use super::{
    close_quietly, failed, kept_text, locked, refused, revoke_quietly, second_details,
    tools_problem, Kept, KeptRemote, LoadPlugin, Refused, Server, CLOSE_TIME, START_TIME,
};
use crate::builtins::find_plugin::{Remote, RemoteHeader};
use crate::builtins::Candidate;
use crate::frontend::session::UI_TIMEOUT;
use crate::frontend::{NoticeLevel, UiRequest};
use crate::manifest::ExtendKind;
use crate::run::host::clip;
use crate::run::oauth::{self, Site, Source, Why};

/// How many times the owner is asked for one header's variable.
const CREDENTIAL_ASKS: usize = 3;

/// How yoagent's adapter words a call made once the server ended its
/// session (a 404 while one is held): the server is then gone for this
/// session (it is not reconnected). Matched from its start, so a server's
/// own error text cannot say it.
const SESSION_EXPIRED: &str = "MCP call failed: Transport error: MCP session expired";

/// What a call that found the session ended answers, and why the server
/// is said stopped.
const ENDED: &str = "the server ended its session (HTTP 404): it is not reconnected this session";

/// How the environment is read: `std::env::var_os`, or a test's map.
pub(crate) type Lookup = Arc<dyn Fn(&str) -> Option<OsString> + Send + Sync>;

/// The environment as it is.
pub(crate) fn environment() -> Lookup {
    Arc::new(|name: &str| std::env::var_os(name))
}

/// What one header came to.
enum Taken {
    Sent(Header),
    /// An optional header left off: by the owner (`None`), or after
    /// [`CREDENTIAL_ASKS`] answers not taken (the last one's why).
    LeftOff(Option<String>),
}

/// A header the owner named a variable for, one sent as listed, or one
/// agent.toml writes ([`Header::as_written`]).
pub(super) struct Header {
    name: String,
    /// As the block writes it: `Bearer ${CTX_KEY}`.
    written: String,
    /// Filled in from the environment: what is sent.
    filled: String,
    /// The variable, when one was named, and its value.
    variable: Option<(String, String)>,
}

impl Header {
    /// A header as agent.toml writes it and as `run` filled it in (its
    /// values hidden by the caller's `hide`, not by a variable named).
    pub(super) fn as_written(name: String, written: String, filled: String) -> Header {
        Header {
            name,
            written,
            filled,
            variable: None,
        }
    }
}

/// A connection made: the client and the tools it listed.
pub(super) type Connected = (McpClient, Vec<McpToolInfo>);

/// How a connection did not come about.
pub(super) enum Connect {
    Refused(Refused),
    /// A 401 offering no sign-in, or a 403: what it was given (nothing, at
    /// first) does not let yobuild in.
    Denied(Refused),
    /// A 401 that may offer a sign-in — its `Bearer` challenge, or, with
    /// nothing sent, no challenge at all (its well-known metadata may say
    /// how) — and what is said when it offers none after all.
    SignIn {
        challenge: Option<String>,
        otherwise: Refused,
    },
}

/// How a sign-in did not come about.
enum NotSignedIn {
    /// The server publishes none (`oauth::Why::NoSignIn`): the credential
    /// refusal its 401 would get.
    NoneOffered(Refused),
    Refused(Refused),
}

/// What connecting came to: the connection, the headers sent (the
/// block's), what the second question says was not sent, and the sign-in
/// when it took one.
struct Connection {
    client: McpClient,
    listed: Vec<McpToolInfo>,
    headers: Vec<Header>,
    not_sent: Vec<String>,
    signed_in: Option<Unkept>,
}

/// A sign-in's tokens until its server is kept: revoked if it is not —
/// by [`LoadPlugin::close_unkept`] or `signs_in::revoke`, or, dropped
/// first, on a task of its own.
pub(super) struct Unkept {
    source: Option<Arc<Source>>,
    name: String,
}

impl Unkept {
    pub(super) fn new(source: Arc<Source>, name: &str) -> Unkept {
        Unkept {
            source: Some(source),
            name: name.to_owned(),
        }
    }

    pub(super) fn source(&self) -> Option<&Arc<Source>> {
        self.source.as_ref()
    }

    /// Kept: its tokens are the server's now, revoked when it closes.
    pub(super) fn keep(mut self) -> Option<Arc<Source>> {
        self.source.take()
    }
}

impl Drop for Unkept {
    fn drop(&mut self) {
        let Some(source) = self.source.take() else {
            return;
        };
        let name = std::mem::take(&mut self.name);
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                source.closing();
                revoke_quietly(&source, &name).await;
            });
        }
    }
}

impl LoadPlugin {
    /// Steps 4 to 7 for a remote server: asked, connected — with no
    /// credential first, signed in when it asks for that (design §14
    /// "Stage 4", point 2), else its credentials named — asked again, kept.
    pub(super) async fn load_remote(
        &self,
        candidate: &Candidate,
        remote: &Remote,
        name: &str,
        ctx: &ToolContext,
    ) -> Result<String, Refused> {
        let package = &candidate.package;
        let host = remote.host();
        // 4. Ask: connect to it?
        let title = format!(
            "Connect to the remote MCP server {package} at {host} as {name}? (asked by the agent)"
        );
        self.ask(
            title,
            first_details(candidate, remote, name),
            package,
            "connect to it",
            ctx,
        )
        .await?;
        // 5–6. Connected, its tools listed and judged.
        let Connection {
            client,
            listed,
            headers,
            not_sent,
            signed_in,
        } = self.connection(candidate, remote, name, ctx).await?;
        let hide = Arc::new(credentials_hidden(&headers));
        let names: Vec<String> = listed.iter().map(|t| t.name.clone()).collect();
        let own = match tools_problem(name, &names) {
            Ok(own) => own,
            Err(e) => {
                self.close_unkept(&client, name, signed_in).await;
                return Err(failed(format!("{package}: {e}")));
            }
        };
        // 7. Ask: keep it?
        let title = format!(
            "Keep the remote MCP server {name}? It offers: {}",
            clip(&own.join(", "), 200)
        );
        let mut details = second_details(
            "the server",
            listed.iter().map(|t| {
                (
                    format!("{name}__{}", t.name),
                    t.description.clone().unwrap_or_default(),
                    t.input_schema.clone(),
                )
            }),
        );
        for said in &not_sent {
            details.push_str(&format!("\nNot sent: {said}."));
        }
        if signed_in.is_some() {
            details.push_str("\nSigned in: its token is kept for this session only, never shown.");
        }
        if let Err(no) = self.ask(title, details, package, "keep it", ctx).await {
            // Not kept: its session ended (yoagent's `close` sends DELETE),
            // its sign-in revoked.
            self.close_unkept(&client, name, signed_in).await;
            return Err(no);
        }
        let source = signed_in.and_then(Unkept::keep);
        let client = Arc::new(tokio::sync::Mutex::new(client));
        let ended: Arc<Mutex<Option<String>>> = Arc::default();
        let tools = Ending::tools(&client, listed, name, &ended, &hide, &source);
        let tool_names: Vec<String> = tools.iter().map(|t| t.name().to_owned()).collect();
        let kept = Kept {
            kind: ExtendKind::Remote,
            name: name.to_owned(),
            package: package.clone(),
            version: String::new(),
            args: Vec::new(),
            tools: own,
            stopped: None,
            remote: Some(KeptRemote {
                url: remote.url().to_string(),
                host: host.to_owned(),
                headers: headers
                    .iter()
                    .map(|h| (h.name.clone(), h.written.clone()))
                    .collect(),
                signed_in: source.is_some(),
            }),
        };
        self.session.notice(
            NoticeLevel::Info,
            format!(
                "The agent connected to {package} at {host} as {name}, for this session. {}",
                kept_text(std::slice::from_ref(&kept), &self.files)
            ),
        );
        self.loaded.add(
            kept,
            Server::remote(name.to_owned(), tools, client, ended, source),
        );
        Ok(format!(
            "Connected to {package} at {host} as {name}, for this session. Its tools are yours from your next step: {}. Each call asks the owner, and sends its arguments to {host}.",
            tool_names.join(", ")
        ))
    }

    /// Steps 5 and 6 (design §14 "Stage 4", point 2): a server whose entry
    /// lists no required header is connected with no credential first — a
    /// 401 offering a sign-in goes to it; one answering is kept as it is
    /// when it has no header to send, else closed before its optional
    /// headers are asked (a transport's headers are fixed when built); one
    /// that lets nothing in unsigned and publishes no sign-in is asked its
    /// optional headers too, when it has any. Then its credentials are
    /// asked and it is connected with them; a 401 with a `Bearer` challenge
    /// then goes to the sign-in, its `Authorization` header left to it.
    async fn connection(
        &self,
        candidate: &Candidate,
        remote: &Remote,
        name: &str,
        ctx: &ToolContext,
    ) -> Result<Connection, Refused> {
        let package = &candidate.package;
        let host = remote.host();
        let sent: Vec<&RemoteHeader> = remote.headers().iter().filter(|h| h.sent()).collect();
        if !sent.iter().any(|h| h.required) {
            match self.connect(package, remote, &[], None, &[], ctx).await {
                Ok((client, listed)) if sent.is_empty() => {
                    return Ok(Connection {
                        client,
                        listed,
                        headers: Vec::new(),
                        not_sent: Vec::new(),
                        signed_in: None,
                    })
                }
                Ok((client, _)) => close_quietly(&client, name).await,
                Err(Connect::SignIn {
                    challenge,
                    otherwise,
                }) => match self
                    .sign_in(package, remote, name, challenge, otherwise, &[], None, ctx)
                    .await
                {
                    Ok(((client, listed), unkept)) => {
                        return Ok(Connection {
                            client,
                            listed,
                            headers: Vec::new(),
                            not_sent: Vec::new(),
                            signed_in: Some(unkept),
                        })
                    }
                    // None published: its optional headers may hold the key.
                    Err(NotSignedIn::NoneOffered(_)) if !sent.is_empty() => {}
                    Err(NotSignedIn::NoneOffered(no) | NotSignedIn::Refused(no)) => return Err(no),
                },
                Err(Connect::Denied(_)) if !sent.is_empty() => {}
                Err(Connect::Denied(no) | Connect::Refused(no)) => return Err(no),
            }
        }
        // Each credential, by the variable that holds it.
        let mut headers = Vec::new();
        let mut not_sent = Vec::new();
        for header in sent {
            match self.header(header, name, host, ctx).await? {
                Taken::Sent(header) => headers.push(header),
                Taken::LeftOff(None) => {}
                Taken::LeftOff(Some(why)) => {
                    let said = format!(
                        "its optional header `{}` is not sent: no usable variable was named after {CREDENTIAL_ASKS} tries (the last: {why})",
                        clip(&header.name, 64)
                    );
                    self.session.notice(
                        NoticeLevel::Warning,
                        format!("The agent's plugin `{package}`: {said}."),
                    );
                    not_sent.push(said);
                }
            }
        }
        let hide = credentials_hidden(&headers);
        match self
            .connect(package, remote, &headers, None, &hide, ctx)
            .await
        {
            Ok((client, listed)) => Ok(Connection {
                client,
                listed,
                headers,
                not_sent,
                signed_in: None,
            }),
            Err(Connect::SignIn {
                challenge,
                otherwise,
            }) => {
                // The sign-in gives the `Authorization` header: the one
                // the owner named, refused, is said and left off.
                let replaced = headers
                    .iter()
                    .find(|h| h.name.eq_ignore_ascii_case("authorization"))
                    .map(|h| match &h.variable {
                        Some((variable, _)) => format!(
                            "its `Authorization` header from `${{{variable}}}`, refused: signed in instead"
                        ),
                        None => "its `Authorization` header as listed, refused: signed in instead"
                            .to_owned(),
                    });
                headers.retain(|h| !h.name.eq_ignore_ascii_case("authorization"));
                not_sent.extend(replaced.clone());
                match self
                    .sign_in(
                        package, remote, name, challenge, otherwise, &headers, replaced, ctx,
                    )
                    .await
                {
                    Ok(((client, listed), unkept)) => Ok(Connection {
                        client,
                        listed,
                        headers,
                        not_sent,
                        signed_in: Some(unkept),
                    }),
                    Err(NotSignedIn::NoneOffered(no) | NotSignedIn::Refused(no)) => Err(no),
                }
            }
            Err(Connect::Denied(no) | Connect::Refused(no)) => Err(no),
        }
    }

    /// The sign-in (design §14 "Stage 4", points 3–9): discovered and
    /// checked before anyone is asked — a server that publishes none is
    /// [`NotSignedIn::NoneOffered`], `otherwise` — then the owner asked to
    /// sign in (Block remembered), the sign-in in their browser, and the
    /// server connected with its token and `headers`. What the agent is
    /// told of a sign-in that did not happen is fixed words; the owner's
    /// notice says why. `note` is said first in the question (an
    /// `Authorization` header the sign-in replaces).
    #[allow(clippy::too_many_arguments)]
    async fn sign_in(
        &self,
        package: &str,
        remote: &Remote,
        name: &str,
        challenge: Option<String>,
        otherwise: Refused,
        headers: &[Header],
        note: Option<String>,
        ctx: &ToolContext,
    ) -> Result<(Connected, Unkept), NotSignedIn> {
        let net = match self.net.get() {
            Some(net) => net.clone(),
            None => match oauth::Net::checked() {
                Ok(net) => self.net.get_or_init(|| net).clone(),
                Err(e) if challenge.is_none() => {
                    tracing::warn!("{package}: no sign-in looked for: {e}");
                    return Err(NotSignedIn::NoneOffered(otherwise));
                }
                Err(e) => {
                    return Err(NotSignedIn::Refused(failed(format!(
                        "{package}'s sign-in: {e}"
                    ))))
                }
            },
        };
        let found = tokio::select! {
            _ = ctx.cancel.cancelled() => {
                return Err(NotSignedIn::Refused(refused("the run was stopped while its sign-in was looked up: it may be proposed again")))
            }
            found = oauth::discover(&net, remote.url(), challenge.as_deref(), Site::Own) => found,
        };
        let found = match found {
            Ok(found) => found,
            Err(no) if no.why() == Why::NoSignIn => {
                return Err(NotSignedIn::NoneOffered(otherwise))
            }
            Err(no) => return Err(NotSignedIn::Refused(self.sign_in_refused(package, &no))),
        };
        let host = remote.host();
        let link = found.link_host();
        let details = match &note {
            Some(note) => format!("{}.\n{}", capitalized(note), found.details()),
            None => found.details(),
        };
        self.ask(
            format!("Sign in to {package} ({host}) at {link}? (asked by the agent)"),
            details,
            package,
            "sign in to it",
            ctx,
        )
        .await
        .map_err(NotSignedIn::Refused)?;
        let print = |link: &str| {
            eprintln!(
                "yobuild: to sign in to {package} ({host}), open this link in a browser on this machine: {link}"
            )
        };
        let ask = oauth::Ask {
            session: &self.session,
            title: format!("Sign in to {package} at {link} in your browser"),
            timeout: self.sign_in_timeout,
            printed: self.phone.then_some(&print as &(dyn Fn(&str) + Sync)),
        };
        // One sign-in at a time in a session: a second waits for the first.
        let waiting = || {
            self.session.notice(
                NoticeLevel::Info,
                format!("{package}'s sign-in waits for another sign-in to end."),
            )
        };
        let Some(one) = self.loaded.one_sign_in(&ctx.cancel, waiting).await else {
            return Err(NotSignedIn::Refused(refused(
                "the run was stopped while its sign-in waited for another: it may be proposed again",
            )));
        };
        let source = oauth::sign_in(&net, &found, ask, &ctx.cancel)
            .await
            .map_err(|no| NotSignedIn::Refused(self.sign_in_refused(package, &no)))?;
        drop(one);
        let unkept = Unkept::new(source.clone(), name);
        let hide = credentials_hidden(headers);
        match self
            .connect(package, remote, headers, Some(source), &hide, ctx)
            .await
        {
            Ok(connected) => Ok((connected, unkept)),
            Err(
                Connect::Refused(no) | Connect::Denied(no) | Connect::SignIn { otherwise: no, .. },
            ) => {
                self.revoke_said(unkept).await;
                // What the server said is the owner's to read.
                Err(NotSignedIn::Refused(Refused {
                    agent: Some(format!(
                        "{package}: it did not connect once signed in (the owner was told why)"
                    )),
                    ..no
                }))
            }
        }
    }

    /// A sign-in that did not happen, as `load_plugin` refuses it: the
    /// owner's reason in the notice, fixed words for the agent. The owner's
    /// Cancel is remembered as a Block. Nobody there, a stop or the
    /// session's end are Warnings; anything else, refused after the owner
    /// allowed it to connect, an Error.
    fn sign_in_refused(&self, package: &str, no: &oauth::Refusal) -> Refused {
        let (level, after) = match no.why() {
            Why::Cancelled => {
                locked(&self.state).blocked.insert(package.to_owned());
                (
                    NoticeLevel::Warning,
                    format!(": do not propose {package} again this session"),
                )
            }
            Why::TimedOut | Why::NoFrontend | Why::Stopped | Why::SessionEnded => {
                (NoticeLevel::Warning, ": it may be proposed again".into())
            }
            Why::NoSignIn
            | Why::Discovery
            | Why::NoPkce
            | Why::NoRegistration
            | Why::OtherSite
            | Why::Registration
            | Why::Callback
            | Why::Exchange
            | Why::Local => (NoticeLevel::Error, String::new()),
        };
        Refused {
            message: format!("{package}'s sign-in: {}{after}", no.for_owner()),
            level,
            agent: Some(format!("{package}: {}{after}", no.for_agent())),
        }
    }

    /// Closes a server not kept, then revokes its sign-in.
    async fn close_unkept(&self, client: &McpClient, name: &str, signed_in: Option<Unkept>) {
        if let Some(source) = signed_in.as_ref().and_then(Unkept::source) {
            source.closing();
        }
        close_quietly(client, name).await;
        if let Some(unkept) = signed_in {
            self.revoke_said(unkept).await;
        }
    }

    /// Revokes a sign-in whose server is not kept, saying what failed as a
    /// Warning (a token that outlives the session at its provider).
    async fn revoke_said(&self, mut unkept: Unkept) {
        let Some(source) = unkept.source.take() else {
            return;
        };
        source.closing();
        for failure in source.revoke_within(CLOSE_TIME).await {
            self.session.notice(
                NoticeLevel::Warning,
                format!(
                    "The agent's plugin `{}`: {failure}: it may stay valid at its provider until it expires.",
                    unkept.name
                ),
            );
        }
    }

    /// Step 5 for one header: a fixed one as listed; else the variable the
    /// owner names, asked at most [`CREDENTIAL_ASKS`] times. No answer is
    /// ever repeated where the agent or a notice would read it: an owner
    /// may paste the key itself.
    async fn header(
        &self,
        header: &RemoteHeader,
        name: &str,
        host: &str,
        ctx: &ToolContext,
    ) -> Result<Taken, Refused> {
        // Checked when found (`find_plugin::remote_of`): a defence only.
        let cannot = || {
            failed(format!(
                "its header `{}` cannot be filled",
                clip(&header.name, 64)
            ))
        };
        if header.fixed() {
            let value = header.value("UNUSED").ok_or_else(cannot)?;
            return Ok(Taken::Sent(Header {
                name: header.name.clone(),
                written: value.as_str().to_owned(),
                filled: value.as_str().to_owned(),
                variable: None,
            }));
        }
        let mut why: Option<String> = None;
        for _ in 0..CREDENTIAL_ASKS {
            let request = UiRequest::Input {
                title: format!(
                    "Which environment variable holds the `{}` header for {name}? (sent to {host} with every request)",
                    clip(&header.name, 64)
                ),
                message: credential_message(header, why.as_deref()),
                placeholder: "VARIABLE_NAME".into(),
                value: String::new(),
            };
            let answer = tokio::select! {
                _ = ctx.cancel.cancelled() => return Err(refused("the run was stopped while the owner was asked")),
                answer = self.session.ask(request, UI_TIMEOUT) => answer,
            };
            let Some(answer) = answer.as_str().map(str::trim) else {
                return Err(refused(format!(
                    "nobody answered which variable holds its `{}` header: it may be proposed again",
                    clip(&header.name, 64)
                )));
            };
            if answer.is_empty() {
                return match header.required {
                    true => Err(refused(format!(
                        "not connected: you named no variable for its required header `{}` (it may be proposed again)",
                        clip(&header.name, 64)
                    ))),
                    false => Ok(Taken::LeftOff(None)),
                };
            }
            let secret = match credential(answer, &self.keys, &*self.lookup) {
                Ok(secret) => secret,
                Err(problem) => {
                    why = Some(problem);
                    continue;
                }
            };
            let value = header.value(answer).ok_or_else(cannot)?;
            let filled = value
                .resolve_with(|_| Some(secret.clone()))
                .map_err(|e| failed(e.to_string()))?;
            if reqwest::header::HeaderValue::from_str(&filled).is_err() {
                why = Some(
                    "that variable's value cannot be sent in a header (visible ASCII only: no line break, no other character)"
                        .into(),
                );
                continue;
            }
            return Ok(Taken::Sent(Header {
                name: header.name.clone(),
                written: value.as_str().to_owned(),
                filled,
                variable: Some((answer.to_owned(), secret)),
            }));
        }
        let last = why.unwrap_or_default();
        match header.required {
            true => Err(refused(format!(
                "not connected: no usable variable was named for its required header `{}` after {CREDENTIAL_ASKS} tries (the last: {last})",
                clip(&header.name, 64)
            ))),
            false => Ok(Taken::LeftOff(Some(last))),
        }
    }

    /// Step 6 for `remote` ([`connect`], its registry entry's headers
    /// telling a 401's words).
    async fn connect(
        &self,
        package: &str,
        remote: &Remote,
        headers: &[Header],
        source: Option<Arc<Source>>,
        hide: &[(String, String)],
        ctx: &ToolContext,
    ) -> Result<Connected, Connect> {
        let listed = remote.headers().iter().any(|h| h.sent());
        connect(
            package,
            remote.url(),
            Origin::Registry { listed },
            headers,
            source,
            hide,
            &ctx.cancel,
        )
        .await
    }
}

/// Connected to `url` over streamable HTTP, following no redirect,
/// `initialize` and `tools/list` each within [`START_TIME`], with
/// `headers` and, signed in, `source`'s token. What fails is said with
/// each credential as its `${NAME}`, each token as `${oauth}`.
#[allow(clippy::too_many_arguments)]
pub(super) async fn connect(
    package: &str,
    url: &url::Url,
    origin: Origin,
    headers: &[Header],
    source: Option<Arc<Source>>,
    hide: &[(String, String)],
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<Connected, Connect> {
    let mut hide = hide.to_vec();
    if let Some(source) = &source {
        hide.extend(source.hidden());
        hide.sort_by_key(|(value, _)| std::cmp::Reverse(value.len()));
    }
    let hidden = |text: &str| super::super::hidden(text, &hide);
    let given = Given {
        signed_in: source.is_some(),
        sent: !headers.is_empty(),
        origin,
    };
    let did_not = |e: &McpError| {
        let no = failed(connect_failed(package, e, given, &hidden));
        match e.http_status() {
            Some(401) if !given.signed_in => {
                if oauth::may_sign_in(e.www_authenticate(), given.sent) {
                    Connect::SignIn {
                        challenge: e.www_authenticate().map(str::to_owned),
                        otherwise: no,
                    }
                } else {
                    Connect::Denied(no)
                }
            }
            Some(401 | 403) => Connect::Denied(no),
            _ => Connect::Refused(no),
        }
    };
    let closing = source.clone();
    let mut transport = HttpTransport::new(url.as_str())
        .and_then(|t| t.with_redirects(Redirects::None))
        .map_err(|e| did_not(&e))?;
    for header in headers {
        transport = transport
            .with_header(&header.name, &header.filled)
            .map_err(|e| did_not(&e))?;
    }
    if let Some(source) = source {
        transport = transport
            .with_authorization(source)
            .map_err(|e| did_not(&e))?;
    }
    let mut client = McpClient::from_transport(Box::new(transport))
        .with_call_timeout(Some(McpToolAdapter::DEFAULT_CALL_TIMEOUT));
    let started = async {
        match tokio::time::timeout(START_TIME, client.initialize()).await {
            Err(_) => {
                return Err(Err(format!(
                    "no answer to `initialize` within {} s",
                    START_TIME.as_secs()
                )))
            }
            Ok(Err(e)) => return Err(Ok(e)),
            Ok(Ok(_)) => {}
        }
        match tokio::time::timeout(START_TIME, client.list_tools()).await {
            Err(_) => Err(Err(format!(
                "no answer to `tools/list` within {} s",
                START_TIME.as_secs()
            ))),
            Ok(Err(e)) => Err(Ok(e)),
            Ok(Ok(listed)) => Ok(listed),
        }
    };
    let listed = tokio::select! {
        _ = cancel.cancelled() => Err(Connect::Refused(refused("the run was stopped while it connected"))),
        listed = started => listed.map_err(|e| match e {
            Ok(e) => did_not(&e),
            Err(late) => Connect::Refused(failed(format!("{package} did not connect: {late}"))),
        }),
    };
    if listed.is_err() {
        // No refresh for its `DELETE`: the sign-in is revoked after.
        if let Some(source) = &closing {
            source.closing();
        }
        close_quietly(&client, package).await;
    }
    listed.map(|listed| (client, listed))
}

/// Where a server comes from, for the words of a 401 refusing it.
#[derive(Clone, Copy)]
pub(super) enum Origin {
    /// Found in the MCP registry: whether its entry lists a header it
    /// would send.
    Registry { listed: bool },
    /// agent.toml's `[[mcp]]`.
    Manifest,
}

/// What a connection was given, for the words of its refusal.
#[derive(Clone, Copy)]
struct Given {
    signed_in: bool,
    /// A header was sent.
    sent: bool,
    origin: Origin,
}

/// Why a connection failed, in words the owner can act on: a 401 says
/// what it was given (nothing, a credential, a sign-in), a 403 whether it
/// was given anything; anything else as yoagent says it, with what was
/// filled in hidden.
fn connect_failed(
    package: &str,
    e: &McpError,
    given: Given,
    hidden: &dyn Fn(&str) -> String,
) -> String {
    let said = e.to_string();
    let said = hidden(said.strip_prefix("Transport error: ").unwrap_or(&said));
    let said = clip(&said, 200);
    let refused = |status: u16, what: &str| {
        format!("{package} refused yobuild's request (HTTP {status}){what} ({said})")
    };
    match e.http_status() {
        Some(401) if given.signed_in => refused(401, " though it was signed in"),
        Some(401) if given.sent => format!(
            "{package} refused the credential it was sent (HTTP 401) and offers no sign-in ({said})"
        ),
        Some(401) => match given.origin {
            Origin::Registry { listed: true } => refused(
                401,
                " and offers no sign-in: it needs a credential yobuild was not given",
            ),
            Origin::Registry { listed: false } => refused(
                401,
                " and offers no sign-in: it needs a credential its registry entry lists no header for",
            ),
            Origin::Manifest => refused(
                401,
                " and offers no sign-in: it needs a credential, which agent.toml gives as a header in place of `auth = \"oauth\"`",
            ),
        },
        Some(403) if !given.signed_in && !given.sent => {
            refused(403, ": it does not let yobuild in without a credential")
        }
        Some(403) => refused(
            403,
            ": it does not take the credential or sign-in it was given",
        ),
        _ => format!("{package} did not connect: {}", clip(&said, 300)),
    }
}

/// `text` with its first letter capitalized.
fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The credential the variable `answer` holds, or why it cannot be taken:
/// not a plain name, a credential yoagent reads (`keys`), not set here or
/// empty, not text. Only a name of `keys` — never an answer otherwise —
/// is repeated: an owner may paste the key itself.
fn credential(
    answer: &str,
    keys: &[String],
    lookup: &dyn Fn(&str) -> Option<OsString>,
) -> Result<String, String> {
    if !crate::manifest::identifier(answer) {
        return Err(
            "that is not a variable's name (letters, digits and `_`): name the variable that holds the key, never the key"
                .into(),
        );
    }
    if keys.iter().any(|k| k == answer) {
        return Err(format!(
            "`{answer}` is a credential yoagent reads: hand it over in agent.toml if you mean to"
        ));
    }
    match lookup(answer) {
        Some(value) if !value.is_empty() => value
            .into_string()
            .map_err(|_| "that variable's value is not text (UTF-8)".into()),
        _ => Err("no variable of that name is set here, or it is empty".into()),
    }
}

/// What a credential named for `headers` is shown as wherever text from
/// the server is said: each value, whatever its length, as its `${NAME}`,
/// longest first.
fn credentials_hidden(headers: &[Header]) -> Vec<(String, String)> {
    let mut hide: Vec<(String, String)> = headers
        .iter()
        .filter_map(|h| {
            let (variable, value) = h.variable.as_ref()?;
            Some((value.clone(), format!("${{{variable}}}")))
        })
        .filter(|(value, _)| !value.is_empty())
        .collect();
    hide.sort_by_key(|(value, _)| std::cmp::Reverse(value.len()));
    hide
}

/// The credential question's text: the header as the entry describes it,
/// how its value is made, whether it may be left empty, and why the last
/// answer was not taken.
fn credential_message(header: &RemoteHeader, why: Option<&str>) -> String {
    let mut lines = Vec::new();
    if let Some(why) = why {
        lines.push(format!("Not taken: {why}. Name another."));
    }
    if !header.description.is_empty() {
        lines.push(format!("The server says of it: {}", header.description));
    }
    lines.push(match &header.template {
        Some(t) => format!(
            "Its value: {} — your variable's value in place of the {{…}}.",
            clip(t, 120)
        ),
        None => "Its value: your variable's value, as it is.".into(),
    });
    lines.push(
        "Name the variable, never its value: the value is read from yobuild's environment and never shown, logged, or given to the agent."
            .into(),
    );
    lines.push(match header.required {
        true => "Without it, it is not connected.".into(),
        false => "Leave it empty to send none.".into(),
    });
    lines.join("\n")
}

/// The first question's details: what the agent needs, what the entry
/// says (its claims), the address and each header, and what connecting
/// means.
fn first_details(candidate: &Candidate, remote: &Remote, name: &str) -> String {
    let mut lines = vec![
        format!("The agent needs: {}", candidate.need),
        format!(
            "{} at {}, its tools as {name}__<tool>. What the MCP registry says is the entry's own claim:",
            candidate.package,
            clip(remote.url().as_str(), 200)
        ),
        format!("  {}", candidate.description),
        format!(
            "  version {}, published {}",
            remote.version().unwrap_or("not given"),
            candidate.published.as_deref().unwrap_or("on a date not given")
        ),
    ];
    if let Some(repository) = &candidate.repository {
        lines.push(format!("  repository (claimed by the entry): {repository}"));
    }
    if remote.headers().is_empty() {
        lines.push("Headers: none listed.".into());
    }
    for h in remote.headers() {
        let what = match (h.required, h.secret, h.fixed()) {
            (true, _, true) => "required, fixed: sent as listed",
            (false, _, true) => "optional, fixed: left off",
            (true, true, false) => "required, secret: you will be asked which variable holds it",
            (true, false, false) => "required: you will be asked which variable holds it",
            (false, true, false) => "optional, secret: you will be asked which variable holds it",
            (false, false, false) => "optional: left off",
        };
        let about = match h.description.as_str() {
            "" => String::new(),
            d => format!(" — {d}"),
        };
        lines.push(format!("Header `{}` ({what}){about}", clip(&h.name, 64)));
    }
    if !remote.others().is_empty() {
        lines.push(format!(
            "Its other addresses (not used): {}",
            remote.others().join(", ")
        ));
    }
    lines.push(format!(
        "Nothing of it runs here. Each call of its tools sends its arguments to {}, and what comes back is that server's to say: what it does with what it gets is not seen here, and it can change any day. Its tools are not known until it answers: you will be asked again once they are.",
        remote.host()
    ));
    lines.join("\n")
}

/// The name a remote server gets from its registry name
/// (`namespace/name`): the name without the `mcp`/`server` words, as a
/// package's ([`super::default_name`]); else the namespace's last label —
/// `io.github.upstash/context7` → `context7`, `com.notion/mcp` →
/// `notion`. Dots become `-` (a tool's prefix takes none).
pub(super) fn default_name(registry: &str) -> String {
    let (space, own) = registry.split_once('/').unwrap_or(("", registry));
    let own = super::default_name(own);
    let name = match own.as_str() {
        "" | "mcp" | "server" | "mcp-server" => {
            space.rsplit('.').next().unwrap_or(space).to_owned()
        }
        _ => own,
    };
    name.replace('.', "-")
}

/// An HTTP server's tool, loaded or agent.toml's signed in: as yoagent's
/// adapter, but a call that finds the server ended the session marks the
/// server gone, since every later call would fail too (it is not
/// reconnected).
pub(super) struct Ending {
    inner: McpToolAdapter,
    ended: Arc<Mutex<Option<String>>>,
    /// Its credentials, shown as their names in what a call's error says
    /// (a server may echo the key it refused).
    hide: Arc<Vec<(String, String)>>,
    /// Its sign-in: each token it handed out shown as `${oauth}` too.
    signed_in: Option<Arc<Source>>,
}

impl Ending {
    /// The tools `listed` by `client`, each `<name>__<tool>`, sharing
    /// `ended`, `hide` and the sign-in.
    pub(super) fn tools(
        client: &Arc<tokio::sync::Mutex<McpClient>>,
        listed: Vec<McpToolInfo>,
        name: &str,
        ended: &Arc<Mutex<Option<String>>>,
        hide: &Arc<Vec<(String, String)>>,
        signed_in: &Option<Arc<Source>>,
    ) -> Vec<Arc<dyn AgentTool>> {
        listed
            .into_iter()
            .map(|info| {
                Arc::new(Ending {
                    inner: McpToolAdapter::new(client.clone(), info)
                        .with_prefix(name.to_owned())
                        .with_call_timeout(Some(McpToolAdapter::DEFAULT_CALL_TIMEOUT)),
                    ended: ended.clone(),
                    hide: hide.clone(),
                    signed_in: signed_in.clone(),
                }) as Arc<dyn AgentTool>
            })
            .collect()
    }
}

#[async_trait::async_trait]
impl AgentTool for Ending {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn label(&self) -> &str {
        self.inner.label()
    }
    fn description(&self) -> &str {
        self.inner.description()
    }
    fn parameters_schema(&self) -> Json {
        self.inner.parameters_schema()
    }
    async fn execute(&self, params: Json, ctx: ToolContext) -> Result<ToolResult, ToolError> {
        match self.inner.execute(params, ctx).await {
            Err(ToolError::Failed(why)) if why.starts_with(SESSION_EXPIRED) => {
                locked(&self.ended).get_or_insert_with(|| ENDED.into());
                Err(ToolError::Failed(ENDED.into()))
            }
            Err(ToolError::Failed(why)) => {
                let mut hide = self.hide.to_vec();
                if let Some(source) = &self.signed_in {
                    hide.extend(source.hidden());
                    hide.sort_by_key(|(value, _)| std::cmp::Reverse(value.len()));
                }
                Err(ToolError::Failed(super::super::hidden(&why, &hide)))
            }
            out => out,
        }
    }
}

#[cfg(test)]
mod tests;
