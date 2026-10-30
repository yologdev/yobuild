//! Loading a remote MCP server `find_plugin` returned (design §14 "Stage
//! 3", points 3–9): nothing of it runs here. The owner is asked to connect
//! to it, then for each header it would send that takes a credential, which
//! environment variable holds it — never the value, never the agent's
//! choice — then connected over streamable HTTP with no redirect followed,
//! its tools listed, and the owner asked again with them in front of them.

use std::sync::{Arc, Mutex};

use serde_json::Value as Json;
use yoagent::mcp::{HttpTransport, McpClient, McpError, McpToolAdapter, McpToolInfo, Redirects};
use yoagent::{AgentTool, ToolContext, ToolError, ToolResult};

use super::{
    failed, kept_text, locked, refused, second_details, tools_problem, Kept, KeptRemote,
    LoadPlugin, Refused, Server, START_TIME,
};
use crate::builtins::find_plugin::{Remote, RemoteHeader};
use crate::builtins::Candidate;
use crate::frontend::session::UI_TIMEOUT;
use crate::frontend::{NoticeLevel, UiRequest};
use crate::manifest::ExtendKind;
use crate::run::host::clip;

/// How many times the owner is asked for one header's variable.
const CREDENTIAL_ASKS: usize = 3;

/// What yoagent says of a call made once the server ended its session: the
/// server is then gone for this session (it is not reconnected).
const SESSION_EXPIRED: &str = "MCP session expired";

/// A header the owner named a variable for, or one sent as listed.
struct Header {
    name: String,
    /// As the block writes it: `Bearer ${CTX_KEY}`.
    written: String,
    /// Filled in from the environment: what is sent.
    filled: String,
    /// The variable, when one was named, and its value.
    variable: Option<(String, String)>,
}

impl LoadPlugin {
    /// Steps 4 to 7 for a remote server: asked, its credentials named,
    /// connected, asked again, kept.
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
        // 5. Each credential, by the variable that holds it.
        let mut headers = Vec::new();
        for header in remote.headers().iter().filter(|h| h.sent()) {
            if let Some(header) = self.header(header, name, host, ctx).await? {
                headers.push(header);
            }
        }
        // 6. Connected, its tools listed and judged.
        let (client, listed) = self.connect(package, remote, &headers, ctx).await?;
        let names: Vec<String> = listed.iter().map(|t| t.name.clone()).collect();
        let own = match tools_problem(name, &names) {
            Ok(own) => own,
            Err(e) => {
                let _ = client.close().await;
                return Err(failed(format!("{package}: {e}")));
            }
        };
        // 7. Ask: keep it?
        let title = format!(
            "Keep the remote MCP server {name}? It offers: {}",
            clip(&own.join(", "), 200)
        );
        let details = second_details(
            "the server",
            listed.iter().map(|t| {
                (
                    format!("{name}__{}", t.name),
                    t.description.clone().unwrap_or_default(),
                    t.input_schema.clone(),
                )
            }),
        );
        if let Err(no) = self.ask(title, details, package, "keep it", ctx).await {
            // Not kept: its session ended (yoagent's `close` sends DELETE).
            let _ = client.close().await;
            return Err(no);
        }
        let client = Arc::new(tokio::sync::Mutex::new(client));
        let ended: Arc<Mutex<Option<String>>> = Arc::default();
        let tools: Vec<Arc<dyn AgentTool>> = listed
            .into_iter()
            .map(|info| {
                Arc::new(Ending {
                    inner: McpToolAdapter::new(client.clone(), info)
                        .with_prefix(name.to_owned())
                        .with_call_timeout(Some(McpToolAdapter::DEFAULT_CALL_TIMEOUT)),
                    ended: ended.clone(),
                }) as Arc<dyn AgentTool>
            })
            .collect();
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
            }),
        };
        self.session.notice(
            NoticeLevel::Info,
            format!(
                "The agent connected to {package} at {host} as {name}, for this session. {}",
                kept_text(std::slice::from_ref(&kept), &self.files)
            ),
        );
        self.loaded
            .add(kept, Server::remote(name.to_owned(), tools, client, ended));
        Ok(format!(
            "Connected to {package} at {host} as {name}, for this session. Its tools are yours from your next step: {}. Each call asks the owner, and sends its arguments to {host}.",
            tool_names.join(", ")
        ))
    }

    /// Step 5 for one header: a fixed one as listed; else the variable the
    /// owner names, asked at most [`CREDENTIAL_ASKS`] times. `None`: an
    /// optional header left off.
    async fn header(
        &self,
        header: &RemoteHeader,
        name: &str,
        host: &str,
        ctx: &ToolContext,
    ) -> Result<Option<Header>, Refused> {
        let cannot = || {
            failed(format!(
                "its header `{}` cannot be filled",
                clip(&header.name, 64)
            ))
        };
        if header.fixed() {
            let value = header.value("UNUSED").ok_or_else(cannot)?;
            return Ok(Some(Header {
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
                    false => Ok(None),
                };
            }
            match credential_problem(answer, &self.keys, &|n| super::super::key_is_set(n)) {
                Some(problem) => why = Some(problem),
                None => {
                    let value = header.value(answer).ok_or_else(cannot)?;
                    let secret = std::env::var(answer).unwrap_or_default();
                    let filled = value
                        .resolve_with(|_| Some(secret.clone()))
                        .map_err(|e| refused(e.to_string()))?;
                    return Ok(Some(Header {
                        name: header.name.clone(),
                        written: value.as_str().to_owned(),
                        filled,
                        variable: Some((answer.to_owned(), secret)),
                    }));
                }
            }
        }
        let last = why.unwrap_or_default();
        match header.required {
            true => Err(refused(format!(
                "not connected: no variable named for its required header `{}` after {CREDENTIAL_ASKS} tries ({last})",
                clip(&header.name, 64)
            ))),
            false => Ok(None),
        }
    }

    /// Step 6: connected over streamable HTTP, following no redirect,
    /// `initialize` and `tools/list` each within [`START_TIME`]. What
    /// fails is said with each credential as its `${NAME}`.
    async fn connect(
        &self,
        package: &str,
        remote: &Remote,
        headers: &[Header],
        ctx: &ToolContext,
    ) -> Result<(McpClient, Vec<McpToolInfo>), Refused> {
        // Each credential's value shown as its `${NAME}`.
        let hide = super::super::hide(
            "",
            "",
            headers.iter().filter_map(|h| {
                let (variable, value) = h.variable.as_ref()?;
                Some((value.clone(), format!("${{{variable}}}")))
            }),
        );
        let hidden = |text: &str| super::super::hidden(text, &hide);
        let did_not = |e: &McpError| failed(connect_failed(package, e, &hidden));
        let mut transport = HttpTransport::new(remote.url().as_str())
            .and_then(|t| t.with_redirects(Redirects::None))
            .map_err(|e| did_not(&e))?;
        for header in headers {
            transport = transport
                .with_header(&header.name, &header.filled)
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
            _ = ctx.cancel.cancelled() => Err(refused("the run was stopped while it connected")),
            listed = started => listed.map_err(|e| match e {
                Ok(e) => did_not(&e),
                Err(late) => failed(format!("{package} did not connect: {late}")),
            }),
        };
        if listed.is_err() {
            let _ = client.close().await;
        }
        listed.map(|listed| (client, listed))
    }
}

/// Why a connection failed, in words the owner can act on: a 401 or 403
/// wants a credential or a sign-in yobuild cannot do yet; anything else as
/// yoagent says it, with what was filled in hidden.
fn connect_failed(package: &str, e: &McpError, hidden: &dyn Fn(&str) -> String) -> String {
    let said = e.to_string();
    let said = hidden(said.strip_prefix("Transport error: ").unwrap_or(&said));
    match e.http_status() {
        Some(status @ (401 | 403)) => format!(
            "{package} refused yobuild's request (HTTP {status}): it needs a credential, or a sign-in (OAuth) yobuild cannot do yet ({})",
            clip(&said, 200)
        ),
        _ => format!("{package} did not connect: {}", clip(&said, 300)),
    }
}

/// Why `answer` is not a variable a credential may come from: not a plain
/// name, a credential yoagent reads (`keys`), or not set here (`is_set`:
/// set and not empty).
fn credential_problem(
    answer: &str,
    keys: &[String],
    is_set: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let shown = clip(answer, 64);
    if !crate::manifest::identifier(answer) {
        return Some(format!(
            "`{shown}` is not a variable's name (letters, digits and `_`)"
        ));
    }
    if keys.iter().any(|k| k == answer) {
        return Some(format!(
            "`{shown}` is a credential yoagent reads: hand it over in agent.toml if you mean to"
        ));
    }
    if !is_set(answer) {
        return Some(format!("`{shown}` is not set here, or is empty"));
    }
    None
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

/// A remote server's tool: as yoagent's adapter, but a call that finds the
/// server ended the session marks the server gone, since every later call
/// would fail too (it is not reconnected).
struct Ending {
    inner: McpToolAdapter,
    ended: Arc<Mutex<Option<String>>>,
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
        let out = self.inner.execute(params, ctx).await;
        if let Err(ToolError::Failed(why)) = &out {
            if why.contains(SESSION_EXPIRED) {
                locked(&self.ended).get_or_insert_with(|| {
                    "the server ended its session (HTTP 404): it is not reconnected".into()
                });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests;
