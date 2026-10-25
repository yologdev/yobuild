//! `load_plugin` (`[extend]`, design §14): the agent proposes one MCP stdio
//! server that `find_plugin` returned this session; yobuild installs it
//! without running anything, asks the owner, starts it and lists its tools,
//! asks again with the tools in front of them, and only then offers them —
//! through [`Loaded`], a tool source yoagent asks again at the next turn
//! (yoagent 0.25.14's `generation`). Every loaded tool asks, call by call
//! (the gate's [`Gate::with_asked`](crate::builtins::Gate::with_asked)).
//! For the session only: nothing is written to `agent.toml` or its lock.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value as Json};
use yoagent::mcp::types::{JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};
use yoagent::mcp::{McpClient, McpError, McpToolAdapter, McpToolInfo, McpTransport};
use yoagent::tool_source::ToolSource;
use yoagent::{AgentTool, Content, ToolContext, ToolError, ToolResult};

use crate::builtins::{find_plugin, Candidate, FindPlugin};
use crate::frontend::session::UI_TIMEOUT;
use crate::frontend::{NoticeLevel, Session, UiRequest};
use crate::manifest::{Extend, Manifest};
use crate::run::host::clip;

/// Starting the server, and listing its tools: each.
const START_TIME: Duration = Duration::from_secs(30);
/// Each npm call of the install.
const NPM_TIME: Duration = Duration::from_secs(300);
const DESCRIPTION_CHARS: usize = 300;
const SCHEMA_CHARS: usize = 400;
const DAY: i64 = 24 * 60 * 60;

/// What the owner sees a loaded server as, and what the end of the session
/// says to keep it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Kept {
    pub name: String,
    pub package: String,
    pub version: String,
    pub args: Vec<String>,
    /// As the server names them (the agent sees `<name>__<tool>`).
    pub tools: Vec<String>,
}

impl Kept {
    /// The `[[mcp]]` block that would keep it.
    pub fn block(&self) -> String {
        let quoted = |items: &[String]| {
            items
                .iter()
                .map(|t| Json::String(t.clone()).to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut block = format!(
            "[[mcp]]\nname = {}\nnpm  = {}\n",
            Json::String(self.name.clone()),
            Json::String(format!("{}@{}", self.package, self.version))
        );
        if !self.args.is_empty() {
            block.push_str(&format!("args = [{}]\n", quoted(&self.args)));
        }
        block.push_str(&format!(
            "# allow = [{}]   # tools that would run without asking\n",
            quoted(&self.tools)
        ));
        block
    }
}

/// What the session loaded, said when it ends (on stderr) and as each is
/// kept (an Info notice): the `[[mcp]]` blocks that would keep them.
pub(crate) fn kept_text(kept: &[Kept]) -> String {
    let mut text = String::from("Loaded this session: ");
    text.push_str(
        &kept
            .iter()
            .map(|k| {
                format!(
                    "{}@{} as {} (tools: {})",
                    k.package,
                    k.version,
                    k.name,
                    k.tools.join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join("; "),
    );
    text.push_str(match kept.len() {
        1 => ". To keep it, add to agent.toml:\n",
        _ => ". To keep them, add to agent.toml:\n",
    });
    for k in kept {
        text.push('\n');
        text.push_str(&k.block());
    }
    text.push_str(match kept.len() {
        1 => "\nThe next run resolves its dependencies again (the lock's `mcp` set, without the age limit) and writes agent.lock, which --locked needs.",
        _ => "\nThe next run resolves their dependencies again (the lock's `mcp` set, without the age limit) and writes agent.lock, which --locked needs.",
    });
    text
}

/// A server the owner kept, running.
struct Server {
    kept: Kept,
    tools: Vec<Arc<dyn AgentTool>>,
    client: Arc<tokio::sync::Mutex<McpClient>>,
    /// Why it stopped, once a call found it gone ([`Watched`]).
    closed: Arc<Mutex<Option<String>>>,
}

#[derive(Default)]
struct Shared {
    servers: Mutex<Vec<Server>>,
    /// Every server kept this session, stopped ones too.
    kept: Mutex<Vec<Kept>>,
    generation: AtomicU64,
}

/// The servers loaded this session, as a tool source: their tools, and a
/// generation bumped on each load and each server that stops, so yoagent
/// offers the change from the next turn. Clones share it. Dropped, the
/// servers are stopped (each `kill_on_drop`).
#[derive(Clone, Default)]
pub(crate) struct Loaded(Arc<Shared>);

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Loaded {
    /// Whether `tool` is one of a loaded server's (stopped ones too): the
    /// gate asks it, call by call.
    pub fn asks(&self, tool: &str) -> bool {
        locked(&self.0.kept).iter().any(|k| {
            tool.strip_prefix(&k.name)
                .and_then(|t| t.strip_prefix("__"))
                .is_some_and(|t| k.tools.iter().any(|own| own == t))
        })
    }

    /// Every server kept this session, stopped ones too.
    pub fn kept(&self) -> Vec<Kept> {
        locked(&self.0.kept).clone()
    }

    fn add(&self, server: Server) {
        locked(&self.0.kept).push(server.kept.clone());
        locked(&self.0.servers).push(server);
        self.0.generation.fetch_add(1, Ordering::SeqCst);
    }

    /// The servers found stopped since the last look, each with why; their
    /// tools are gone from the next turn.
    pub fn stopped(&self) -> Vec<(String, String)> {
        let mut servers = locked(&self.0.servers);
        let mut out = Vec::new();
        servers.retain(|s| match locked(&s.closed).clone() {
            Some(why) => {
                out.push((s.kept.name.clone(), why));
                false
            }
            None => true,
        });
        if !out.is_empty() {
            self.0.generation.fetch_add(1, Ordering::SeqCst);
        }
        out
    }

    /// Stops every server.
    pub async fn close(&self) {
        let servers = std::mem::take(&mut *locked(&self.0.servers));
        for server in servers {
            if let Err(e) = server.client.lock().await.close().await {
                tracing::warn!("stopping the loaded MCP server {}: {e}", server.kept.name);
            }
        }
    }
}

#[async_trait::async_trait]
impl ToolSource for Loaded {
    async fn tools(&self) -> Vec<Arc<dyn AgentTool>> {
        locked(&self.0.servers)
            .iter()
            .flat_map(|s| s.tools.iter().cloned())
            .collect()
    }

    fn generation(&self) -> u64 {
        self.0.generation.load(Ordering::SeqCst)
    }
}

/// yoagent's stdio transport, noting why the server is gone when a call
/// finds it so (its exit status and last stderr): yoagent keeps the child
/// to itself, so a server that dies is seen at its next call.
struct Watched {
    inner: yoagent::mcp::StdioTransport,
    closed: Arc<Mutex<Option<String>>>,
}

impl Watched {
    fn see<T>(&self, out: &Result<T, McpError>) {
        if let Some(why) = gone(out) {
            locked(&self.closed).get_or_insert(why);
        }
    }
}

/// Why a call's result says the server is gone, if it does.
fn gone<T>(out: &Result<T, McpError>) -> Option<String> {
    match out {
        Err(McpError::ConnectionClosed) => Some("its connection closed".to_owned()),
        // "Connection closed: MCP server '<program>' exited with …": the
        // program is `/usr/bin/env` (the kept environment), so not named.
        Err(McpError::Transport(m)) if m.starts_with("Connection closed") => {
            let rest = m
                .strip_prefix("Connection closed: MCP server '")
                .and_then(|r| r.split_once("' "))
                .map_or(m.as_str(), |(_, rest)| rest);
            Some(clip(&format!("it {rest}"), 400))
        }
        _ => None,
    }
}

#[async_trait::async_trait]
impl McpTransport for Watched {
    async fn send(&self, request: JsonRpcRequest) -> Result<JsonRpcResponse, McpError> {
        let out = self.inner.send(request).await;
        self.see(&out);
        out
    }

    async fn notify(&self, notification: JsonRpcNotification) -> Result<(), McpError> {
        let out = self.inner.notify(notification).await;
        self.see(&out);
        out
    }

    async fn close(&self) -> Result<(), McpError> {
        self.inner.close().await
    }
    // Notifications are not reported: the tool list is fixed once the owner
    // has seen it (a `list_changed` is ignored).
}

/// What `load_plugin` refuses, and how the owner hears of it.
struct Refused {
    message: String,
    /// An Error notice (it failed after the owner allowed it), else a
    /// Warning.
    after_allow: bool,
}

fn refused(message: impl Into<String>) -> Refused {
    Refused {
        message: message.into(),
        after_allow: false,
    }
}

fn failed(message: impl Into<String>) -> Refused {
    Refused {
        message: message.into(),
        after_allow: true,
    }
}

#[derive(Default)]
struct State {
    /// A load in progress.
    busy: bool,
    attempts: u32,
    /// Packages the owner blocked: not proposed again this session.
    blocked: BTreeSet<String>,
}

/// The `load_plugin` tool. Clones share the session's state.
#[derive(Clone)]
pub(crate) struct LoadPlugin {
    session: Session,
    extend: Extend,
    find: FindPlugin,
    loaded: Loaded,
    once: bool,
    /// The manifest's plugins' and servers' names.
    taken: Arc<BTreeSet<String>>,
    home: Option<PathBuf>,
    cwd: PathBuf,
    /// The credentials set here, taken out of its environment.
    withheld: Arc<Vec<String>>,
    state: Arc<Mutex<State>>,
}

/// Clears `busy` however a load ends (an aborted run drops it).
struct Busy(Arc<Mutex<State>>);

impl Drop for Busy {
    fn drop(&mut self) {
        locked(&self.0).busy = false;
    }
}

/// Sets `stop` when dropped: npm, still running in its thread, is killed.
struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

impl LoadPlugin {
    pub const TOOL: &'static str = "load_plugin";

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session: Session,
        manifest: &Manifest,
        extend: Extend,
        find: FindPlugin,
        loaded: Loaded,
        once: bool,
        home: Option<PathBuf>,
        cwd: &Path,
    ) -> Self {
        let taken = manifest
            .plugins
            .iter()
            .map(|p| p.name.clone())
            .chain(manifest.mcp.iter().map(|m| m.name.clone()))
            .collect();
        Self {
            session,
            extend,
            find,
            loaded,
            once,
            taken: Arc::new(taken),
            home,
            cwd: cwd.to_path_buf(),
            withheld: Arc::new(super::withheld(manifest)),
            state: Arc::default(),
        }
    }

    /// Step 1, the limits: the candidate, and the call counted.
    fn limits(&self, package: &str, version: &str) -> Result<(Candidate, Busy), Refused> {
        if self.once {
            return Err(refused(
                "not under --once: nobody is there to allow it (nothing was counted)",
            ));
        }
        let mut state = locked(&self.state);
        if state.busy {
            return Err(refused(
                "another load is in progress: wait for its result, then call again",
            ));
        }
        let candidates = self.find.candidates();
        let Some(candidate) = candidates.into_iter().find(|c| c.package == package) else {
            return Err(refused(format!(
                "{package} is not among this session's candidates: only a package find_plugin returned can be loaded"
            )));
        };
        match &candidate.version {
            Err(why) => {
                return Err(refused(format!(
                    "{package} is not loadable: {why} (min_age_days is {})",
                    self.extend.min_age_days
                )))
            }
            Ok(v) if v != version => {
                return Err(refused(format!(
                    "{package}@{version} is not the version find_plugin offered: {package}@{v}"
                )))
            }
            Ok(_) => {}
        }
        if self.loaded.kept().iter().any(|k| k.package == package) {
            return Err(refused(format!("{package} is loaded already this session")));
        }
        if state.blocked.contains(package) {
            return Err(refused(format!(
                "the owner blocked {package} earlier this session"
            )));
        }
        if state.attempts >= self.extend.max_attempts {
            return Err(refused(format!(
                "this session's load_plugin calls are spent ([extend] max_attempts {})",
                self.extend.max_attempts
            )));
        }
        state.attempts += 1;
        let kept = self.loaded.kept().len();
        if kept >= self.extend.max_loads as usize {
            return Err(refused(format!(
                "this session has loaded {kept} servers, as many as it may ([extend] max_loads {})",
                self.extend.max_loads
            )));
        }
        state.busy = true;
        Ok((candidate, Busy(self.state.clone())))
    }

    /// Step 2, the name: given, or the package's.
    fn name(&self, package: &str, given: Option<&str>) -> Result<String, Refused> {
        let name = match given {
            Some(name) => name.to_owned(),
            None => default_name(package),
        };
        let pass_another = |why: String| {
            refused(format!(
                "{why}: pass another `name` (letters, digits, `-`, `_`)"
            ))
        };
        if let Some(why) = name_problem(&name) {
            return Err(pass_another(format!("the name {name:?} {why}")));
        }
        if self.taken.contains(&name) || self.loaded.kept().iter().any(|k| k.name == name) {
            return Err(pass_another(format!(
                "the name {name:?} is in use by another plugin or server"
            )));
        }
        Ok(name)
    }

    /// Steps 2 to 7; the result's text.
    async fn load(&self, params: &Json, ctx: &ToolContext) -> Result<String, Refused> {
        let text = |key: &str| -> Result<Option<String>, Refused> {
            match &params[key] {
                Json::Null => Ok(None),
                Json::String(s) => Ok(Some(s.clone())),
                other => Err(refused(format!(
                    "`{key}` must be a string, not {}",
                    clip(&other.to_string(), 40)
                ))),
            }
        };
        let package = text("package")?.ok_or_else(|| refused("`package` is required"))?;
        let version = text("version")?.ok_or_else(|| refused("`version` is required"))?;
        let given = text("name")?;
        let args: Vec<String> = match &params["args"] {
            Json::Null => Vec::new(),
            Json::Array(items) => items
                .iter()
                .map(|a| match a {
                    Json::String(s) => Ok(s.clone()),
                    other => Err(refused(format!(
                        "each of `args` must be a string, not {}",
                        clip(&other.to_string(), 40)
                    ))),
                })
                .collect::<Result<_, _>>()?,
            other => {
                return Err(refused(format!(
                    "`args` must be a list of strings, not {}",
                    clip(&other.to_string(), 40)
                )))
            }
        };
        let (candidate, _busy) = self.limits(&package, &version)?;
        let name = self.name(&package, given.as_deref())?;
        // 3. What it is handed.
        crate::manifest::loaded_arguments(&args).map_err(refused)?;
        if let Some(env) = candidate.required_env.as_ref().filter(|e| !e.is_empty()) {
            return Err(refused(format!(
                "the MCP registry lists environment variables it requires ({}): nothing hands a loaded server one",
                env.join(", ")
            )));
        }
        // 4. Installed, running nothing.
        let (dir, scripts) = self.install(&package, &version, ctx).await?;
        let code = dir.join("node_modules").join(&package);
        let bin = bin(&code, &package).map_err(|e| refused(format!("{package}: {e}")))?;
        let reads = self.reads(&code);
        // 5. Ask: start it?
        let details = first_details(
            &candidate, &version, &name, &args, &self.cwd, &scripts, &reads,
        );
        let title =
            format!("Start the MCP server {package}@{version} as {name}? (asked by the agent)");
        self.ask(title, details, &package, "start it", ctx).await?;
        // 6. Started, its tools listed and judged.
        let closed: Arc<Mutex<Option<String>>> = Arc::default();
        let (client, listed) = self
            .start(&bin, &args, closed.clone(), ctx)
            .await
            .map_err(|e| failed(format!("{package} did not start: {e}")))?;
        let own = tools_problem(&name, &listed).map_err(|e| failed(format!("{package}: {e}")))?;
        // 7. Ask: keep it?
        let title = format!(
            "Keep the MCP server {name}? It offers: {}",
            clip(&own.join(", "), 200)
        );
        self.ask(
            title,
            second_details(&name, &listed),
            &package,
            "keep it",
            ctx,
        )
        .await?;
        let client = Arc::new(tokio::sync::Mutex::new(client));
        let tools: Vec<Arc<dyn AgentTool>> = listed
            .into_iter()
            .map(|info| {
                Arc::new(
                    McpToolAdapter::new(client.clone(), info)
                        .with_prefix(name.clone())
                        .with_call_timeout(Some(McpToolAdapter::DEFAULT_CALL_TIMEOUT)),
                ) as Arc<dyn AgentTool>
            })
            .collect();
        let names: Vec<String> = tools.iter().map(|t| t.name().to_owned()).collect();
        let kept = Kept {
            name: name.clone(),
            package: package.clone(),
            version: version.clone(),
            args,
            tools: own,
        };
        self.session.notice(
            NoticeLevel::Info,
            format!(
                "The agent loaded {package}@{version} as {name}, for this session. {}",
                kept_text(std::slice::from_ref(&kept))
            ),
        );
        self.loaded.add(Server {
            kept,
            tools,
            client,
            closed,
        });
        Ok(format!(
            "Loaded {package}@{version} as {name}, for this session. Its tools are yours from your next step: {}. Each call asks the owner.",
            names.join(", ")
        ))
    }

    /// Step 4: a one-entry `mcp` set, every package in its tree published
    /// `min_age_days` ago, installed without its scripts. Stopped (npm
    /// killed, the partial folder removed) when the run is.
    async fn install(
        &self,
        package: &str,
        version: &str,
        ctx: &ToolContext,
    ) -> Result<(PathBuf, Vec<String>), Refused> {
        let before = match self.extend.min_age_days {
            0 => None,
            days => {
                let now = find_plugin::now().ok_or_else(|| {
                    refused("this machine's clock reads before 1970: no age can be told")
                })?;
                Some(find_plugin::iso(now - i64::from(days) * DAY))
            }
        };
        let stop = Arc::new(AtomicBool::new(false));
        let _stop = StopOnDrop(stop.clone());
        let (home, package_, version_) =
            (self.home.clone(), package.to_owned(), version.to_owned());
        let installing = tokio::task::spawn_blocking(move || {
            let how = crate::install::Npm {
                limit: NPM_TIME,
                stop: Some(&stop),
            };
            crate::install::install_one(
                home.as_deref(),
                &package_,
                &version_,
                before.as_deref(),
                &how,
            )
            .map_err(|e| e.to_string())
        });
        let out = tokio::select! {
            _ = ctx.cancel.cancelled() => return Err(refused("the run was stopped while it installed")),
            out = installing => out,
        };
        match out {
            Ok(Ok(done)) => Ok(done),
            Ok(Err(e)) => Err(refused(format!(
                "{package}@{version} could not be installed: {e}"
            ))),
            Err(e) => Err(refused(format!("the install's task failed: {e}"))),
        }
    }

    /// What its code reads that it would not get: set here, and not in the
    /// kept environment (a credential yoagent reads among them).
    fn reads(&self, code: &Path) -> Vec<String> {
        let scan = crate::environment::names_read(code);
        scan.names
            .into_iter()
            .filter(|name| std::env::var_os(name).is_some_and(|v| !v.is_empty()))
            .filter(|name| self.withheld.contains(name) || !crate::environment::kept(name.as_ref()))
            .collect()
    }

    /// Asks the owner Allow/Block. Block remembers the package; nobody
    /// answering does not.
    async fn ask(
        &self,
        title: String,
        details: String,
        package: &str,
        what: &str,
        ctx: &ToolContext,
    ) -> Result<(), Refused> {
        let request = UiRequest::Select {
            title,
            message: details,
            options: vec!["Allow".into(), "Block".into()],
            multiple: false,
        };
        let answer = tokio::select! {
            _ = ctx.cancel.cancelled() => return Err(refused("the run was stopped while the owner was asked")),
            answer = self.session.ask(request, UI_TIMEOUT) => answer,
        };
        match answer.as_str() {
            Some("Allow") => Ok(()),
            Some(_) => {
                locked(&self.state).blocked.insert(package.to_owned());
                Err(refused(format!(
                    "the owner blocked it ({what}): do not propose {package} again this session"
                )))
            }
            None => Err(refused(format!(
                "nobody answered whether to {what} (no one there, the question timed out, or it was dismissed): it may be proposed again"
            ))),
        }
    }

    /// Step 6: the server started with the kept environment, `initialize`
    /// and `tools/list` each within [`START_TIME`].
    async fn start(
        &self,
        bin: &Path,
        args: &[String],
        closed: Arc<Mutex<Option<String>>>,
        ctx: &ToolContext,
    ) -> Result<(McpClient, Vec<McpToolInfo>), String> {
        let (program, argv, env) = super::without(bin, args, &HashMap::new(), &self.withheld)
            .map_err(|e| e.to_string())?;
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let inner = yoagent::mcp::StdioTransport::new(&program, &argv, Some(env))
            .await
            .map_err(|e| e.to_string())?;
        let mut client = McpClient::from_transport(Box::new(Watched { inner, closed }))
            .with_call_timeout(Some(McpToolAdapter::DEFAULT_CALL_TIMEOUT));
        let started = async {
            tokio::time::timeout(START_TIME, client.initialize())
                .await
                .map_err(|_| {
                    format!(
                        "no answer to `initialize` within {} s",
                        START_TIME.as_secs()
                    )
                })?
                .map_err(|e| e.to_string())?;
            tokio::time::timeout(START_TIME, client.list_tools())
                .await
                .map_err(|_| {
                    format!(
                        "no answer to `tools/list` within {} s",
                        START_TIME.as_secs()
                    )
                })?
                .map_err(|e| e.to_string())
        };
        let listed = tokio::select! {
            _ = ctx.cancel.cancelled() => Err("the run was stopped while it started".to_owned()),
            listed = started => listed,
        };
        match listed {
            Ok(listed) => Ok((client, listed)),
            Err(e) => {
                let _ = client.close().await;
                Err(e)
            }
        }
    }
}

/// The name a package's server gets: without its scope, a leading
/// `server-` or `mcp-server-`, a trailing `-mcp` or `-mcp-server`; when
/// nothing is left (`@eslint/mcp`), the scope.
fn default_name(package: &str) -> String {
    let (scope, base) = match package.strip_prefix('@').and_then(|p| p.split_once('/')) {
        Some((scope, base)) => (Some(scope), base),
        None => (None, package),
    };
    let mut name = base;
    for prefix in ["mcp-server-", "server-"] {
        if let Some(rest) = name.strip_prefix(prefix) {
            name = rest;
            break;
        }
    }
    for suffix in ["-mcp-server", "-mcp"] {
        if let Some(rest) = name.strip_suffix(suffix) {
            name = rest;
            break;
        }
    }
    match (name, scope) {
        ("" | "mcp" | "server" | "mcp-server", Some(scope)) => scope.to_owned(),
        ("", None) => base.to_owned(),
        (name, _) => name.to_owned(),
    }
}

/// Why `name` cannot prefix a server's tools, if it cannot: letters,
/// digits, `-` and `_`, no `__`, short enough for `<name>__<tool>`.
fn name_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return Some("is empty");
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Some("takes only letters, digits, `-` and `_`");
    }
    if name.contains("__") {
        return Some("holds `__`, which parts a server's name from its tools'");
    }
    if name.len() + 3 > 64 {
        return Some("is too long: `<name>__<tool>` must fit in 64 characters");
    }
    None
}

/// The tools as the server names them, once each can be offered as
/// `<name>__<tool>`; else why not (none listed, a name a model cannot take).
fn tools_problem(name: &str, listed: &[McpToolInfo]) -> Result<Vec<String>, String> {
    if listed.is_empty() {
        return Err("it lists no tools".into());
    }
    let mut own = Vec::new();
    for tool in listed {
        let full = format!("{name}__{}", tool.name);
        if !super::tool_name(&full) {
            return Err(match super::tool_name(&tool.name) {
                true => format!(
                    "with the prefix {name}__ its tool {:?} is {} characters, more than 64: load it under a shorter `name`",
                    tool.name,
                    full.len()
                ),
                false => format!(
                    "its tool {:?} cannot be offered to a model (names take up to 64 letters, digits, _ and -)",
                    clip(&tool.name, 80)
                ),
            });
        }
        if own.contains(&tool.name) {
            return Err(format!("it lists the tool {:?} twice", tool.name));
        }
        own.push(tool.name.clone());
    }
    Ok(own)
}

/// The program the package runs: its one `bin`, or of several the one
/// named like the package.
fn bin(code: &Path, package: &str) -> Result<PathBuf, String> {
    match super::bin_of(code, package, None) {
        Err(e) if e.contains("several bins") => {
            let unscoped = package.rsplit('/').next().unwrap_or(package);
            super::bin_of(code, package, Some(unscoped))
                .map_err(|_| format!("{e}, and none is named {unscoped}"))
        }
        other => other,
    }
}

/// An argument naming a path outside the agent's folder (`/x`, `~/x`,
/// `../x`, or such a path after `=`).
fn outside(arg: &str, cwd: &Path) -> bool {
    let value = arg.split_once('=').map_or(arg, |(_, v)| v);
    let path = match value.strip_prefix('~') {
        Some(_) => return true,
        None if value.starts_with('/') => PathBuf::from(value),
        None if value == ".." || value.starts_with("../") || value.contains("/../") => {
            cwd.join(value)
        }
        None => return false,
    };
    let mut parts: Vec<std::path::Component> = Vec::new();
    for part in path.components() {
        match part {
            std::path::Component::ParentDir => {
                parts.pop();
            }
            std::path::Component::CurDir => {}
            other => parts.push(other),
        }
    }
    !parts.iter().collect::<PathBuf>().starts_with(cwd)
}

/// The first question's details: what the agent needs, what the sources
/// say (their claims), what it is handed, what was found without running
/// it, and the two warnings.
fn first_details(
    candidate: &Candidate,
    version: &str,
    name: &str,
    args: &[String],
    cwd: &Path,
    scripts: &[String],
    reads: &[String],
) -> String {
    let mut lines = vec![
        format!("The agent needs: {}", candidate.need),
        format!(
            "{}@{version}, its tools as {name}__<tool>. What the sources say is the package's own claim:",
            candidate.package
        ),
        format!("  {}", candidate.description),
    ];
    if let Some(published) = &candidate.published {
        lines.push(format!("  published {published}"));
    }
    lines.push(format!(
        "  licence: {}",
        candidate.licence.as_deref().unwrap_or("none given")
    ));
    if let Some(repository) = &candidate.repository {
        lines.push(format!("  repository (claimed): {repository}"));
    }
    lines.push(format!(
        "  provenance: {}",
        match candidate.provenance {
            Some(true) => "an attestation is published with it (not verified)",
            Some(false) => "none",
            None => "unknown",
        }
    ));
    lines.push(format!(
        "  weekly downloads: {}",
        match &candidate.downloads {
            Ok(n) => n.to_string(),
            Err(why) => format!("unknown ({why})"),
        }
    ));
    lines.push(match &candidate.required_args {
        None => "  its needs: not listed (found by npm only)".into(),
        Some(a) if a.is_empty() => "  required arguments (MCP registry): none".into(),
        Some(a) => format!("  required arguments (MCP registry): {}", a.join(", ")),
    });
    lines.push(match args.is_empty() {
        true => "Arguments: none".into(),
        false => format!(
            "Arguments: {}",
            args.iter()
                .map(|a| match outside(a, cwd) {
                    true => format!("{a:?} (outside the agent's folder)"),
                    false => format!("{a:?}"),
                })
                .collect::<Vec<_>>()
                .join(" ")
        ),
    });
    lines.push(match scripts.is_empty() {
        true => "Install scripts: none (none are run)".into(),
        false => format!(
            "Install scripts it would have run (not run): {}",
            scripts.join(", ")
        ),
    });
    if !reads.is_empty() {
        lines.push(format!(
            "Its code reads {}, set here, which it will not get (it starts with the kept environment)",
            reads.join(", ")
        ));
    }
    lines.push(
        "Its tools are not known until it starts: you will be asked again once they are.".into(),
    );
    lines.push("It runs with your rights: this is not a sandbox.".into());
    lines.join("\n")
}

/// The second question's details: each tool's description (its own claim)
/// and parameters.
fn second_details(name: &str, listed: &[McpToolInfo]) -> String {
    let mut lines = vec![
        "Its tools, as the server describes them (its own claims); each call will ask you:"
            .to_owned(),
    ];
    for tool in listed {
        lines.push(format!(
            "{name}__{}: {}",
            tool.name,
            clip(
                tool.description.as_deref().unwrap_or("(no description)"),
                DESCRIPTION_CHARS
            )
        ));
        lines.push(format!(
            "  parameters: {}",
            clip(&tool.input_schema.to_string(), SCHEMA_CHARS)
        ));
    }
    lines.join("\n")
}

#[async_trait::async_trait]
impl AgentTool for LoadPlugin {
    fn name(&self) -> &str {
        Self::TOOL
    }
    fn label(&self) -> &str {
        "Load a plugin"
    }
    fn description(&self) -> &str {
        "Propose an MCP server find_plugin returned, at the version it offered, for this session. \
         The owner is asked before it starts, and again with its tools before you get them; its \
         tools are yours from your next step, each call asking the owner. A refusal says why: do \
         not propose a package the owner blocked again."
    }
    fn parameters_schema(&self) -> Json {
        json!({
            "type": "object",
            "properties": {
                "package": { "type": "string", "description": "The npm package, as find_plugin gave it." },
                "version": { "type": "string", "description": "The version find_plugin said could be loaded." },
                "name": { "type": "string", "description": "What to call it (its tools are <name>__<tool>); by default from the package's name." },
                "args": { "type": "array", "items": { "type": "string" }, "description": "The server's own arguments, as its documentation gives them." }
            },
            "required": ["package", "version"]
        })
    }

    async fn execute(&self, params: Json, ctx: ToolContext) -> Result<ToolResult, ToolError> {
        let object = params
            .as_object()
            .ok_or_else(|| ToolError::InvalidArgs("the arguments must be an object".into()))?;
        if let Some(key) = object
            .keys()
            .find(|k| !["package", "version", "name", "args"].contains(&k.as_str()))
        {
            return Err(ToolError::InvalidArgs(format!(
                "unknown key `{key}` (use package, version, name, args)"
            )));
        }
        match self.load(&params, &ctx).await {
            Ok(text) => Ok(ToolResult {
                content: vec![Content::Text { text }],
                details: json!({}),
            }),
            Err(Refused {
                message,
                after_allow,
            }) => {
                let package = params["package"].as_str().unwrap_or("?");
                let level = match after_allow {
                    true => NoticeLevel::Error,
                    false => NoticeLevel::Warning,
                };
                self.session.notice(
                    level,
                    format!(
                        "The agent's plugin `{}` was refused: {message}",
                        clip(package, 80)
                    ),
                );
                Err(ToolError::Failed(format!(
                    "load_plugin refused it: {message}"
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests;
