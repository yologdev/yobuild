//! `load_plugin` (`[extend]`, design §14): the agent proposes one MCP stdio
//! server that `find_plugin` returned this session; yobuild installs it
//! without running anything, asks the owner, starts it and lists its tools,
//! asks again with the tools in front of them, and only then offers them —
//! through [`Loaded`], a tool source yoagent asks again at the next turn
//! (yoagent 0.25.14's `generation`); they leave that list once yoagent's
//! exit watch (0.25.15) says their server has gone. Every loaded tool asks, call by call
//! (the gate's [`Gate::with_asked`](crate::builtins::Gate::with_asked)).
//! For the session only: nothing is written to `agent.toml` or its lock.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value as Json};
use yoagent::mcp::{ExitWatch, McpClient, McpToolAdapter, McpToolInfo, McpTransport};
use yoagent::tool_source::ToolSource;
use yoagent::{AgentTool, Content, ToolContext, ToolError, ToolResult};

use crate::builtins::{find_plugin, Candidate, FindPlugin};
use crate::frontend::session::UI_TIMEOUT;
use crate::frontend::{NoticeLevel, Session, UiRequest};
use crate::manifest::{Extend, ExtendKind, Manifest};
use crate::run::host::clip;

/// Starting the server, and listing its tools: each.
const START_TIME: Duration = Duration::from_secs(30);
/// Each npm call of the install.
const NPM_TIME: Duration = Duration::from_secs(300);
const DESCRIPTION_CHARS: usize = 300;
const SCHEMA_CHARS: usize = 400;
const DAY: i64 = 24 * 60 * 60;

/// A server the owner kept this session, as the end of the session says
/// it. Made only by `load` once both questions were allowed: `name` passed
/// [`name_problem`] and `tools` [`tools_problem`] (each once, each
/// `<name>__<tool>` a tool name), which [`Loaded::asks`] relies on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Kept {
    pub name: String,
    pub package: String,
    pub version: String,
    pub args: Vec<String>,
    /// As the server names them (the agent sees `<name>__<tool>`).
    pub tools: Vec<String>,
    /// Why it stopped, once it was seen to.
    pub stopped: Option<String>,
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

/// The manifest's file and its lock's, as the owner names them
/// (`agent.toml`, `agent.lock`; `handoff.toml`, `handoff.lock`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Files {
    pub manifest: String,
    pub lock: String,
}

/// What the session loaded, said when it ends (on stderr) and as each is
/// kept (an Info notice): the `[[mcp]]` blocks that would keep them, a
/// server that stopped marked so.
pub(crate) fn kept_text(kept: &[Kept], files: &Files) -> String {
    let mut text = String::from("Loaded this session: ");
    text.push_str(
        &kept
            .iter()
            .map(|k| {
                let stopped = match &k.stopped {
                    Some(why) => format!(", stopped this session: {why}"),
                    None => String::new(),
                };
                format!(
                    "{}@{} as {} (tools: {}{stopped})",
                    k.package,
                    k.version,
                    k.name,
                    k.tools.join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join("; "),
    );
    let (it, its) = match kept.len() {
        1 => ("it", "its"),
        _ => ("them", "their"),
    };
    text.push_str(&format!(". To keep {it}, add to {}:\n", files.manifest));
    for k in kept {
        text.push('\n');
        text.push_str(&k.block());
    }
    text.push_str(&format!(
        "\nThe next run resolves {its} dependencies again (the lock's `mcp` set, without the age limit) and writes {}, which --locked needs.",
        files.lock
    ));
    text
}

/// How long a server is seen gone before it is said stopped: yoagent's
/// exit text settles within 0.5 s (the last of its stderr read).
const SETTLE: Duration = Duration::from_millis(600);

/// How an exit by SIGINT reads (Rust's `ExitStatus`): a Ctrl+C typed in
/// the terminal reaches the servers too.
const SIGINT: &str = "signal: 2 (SIGINT)";

/// A server the owner kept, running.
struct Server {
    name: String,
    tools: Vec<Arc<dyn AgentTool>>,
    client: Arc<tokio::sync::Mutex<McpClient>>,
    /// yoagent's watch over its process, taken from the transport before
    /// the client was built: read without the `tokio::Mutex` yobuild keeps
    /// the client in (a call in flight holds it).
    exit: ExitWatch,
    /// Once the watch said it had gone: when it first did, and the longest
    /// reason read since (yoagent answers without the stderr tail while a
    /// call or `close` holds the child).
    gone: Mutex<Option<(Instant, String)>>,
}

impl Server {
    fn new(
        name: String,
        tools: Vec<Arc<dyn AgentTool>>,
        client: Arc<tokio::sync::Mutex<McpClient>>,
        exit: ExitWatch,
    ) -> Self {
        Server {
            name,
            tools,
            client,
            exit,
            gone: Mutex::default(),
        }
    }

    /// Reads the watch: whether the server was newly seen gone. A reason
    /// read is kept when longer than the one before.
    fn look(&self) -> bool {
        let Some(why) = self.exit.exited().map(|why| said(&why)) else {
            return false;
        };
        let mut gone = locked(&self.gone);
        match gone.as_mut() {
            None => {
                *gone = Some((Instant::now(), why));
                true
            }
            Some((_, kept)) => {
                if why.chars().count() > kept.chars().count() {
                    *kept = why;
                }
                false
            }
        }
    }

    fn is_gone(&self) -> bool {
        locked(&self.gone).is_some()
    }

    /// When it was first seen gone, and why (the longest reason read).
    fn gone(&self) -> Option<(Instant, String)> {
        locked(&self.gone).clone()
    }
}

#[derive(Default)]
struct Lists {
    /// The servers running (or found gone, until [`Loaded::stopped`]).
    servers: Vec<Server>,
    /// Every server kept this session, stopped ones too: never shrinks,
    /// and holds each server before its tools are offered (one lock).
    kept: Vec<Kept>,
}

#[derive(Default)]
struct Shared {
    lists: Mutex<Lists>,
    /// Moves on each load, each server found gone and the close; only
    /// forward.
    generation: AtomicU64,
}

/// The servers loaded this session, as a tool source: their tools, and a
/// generation bumped on each load and each server found gone, so yoagent
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
        locked(&self.0.lists).kept.iter().any(|k| {
            tool.strip_prefix(&k.name)
                .and_then(|t| t.strip_prefix("__"))
                .is_some_and(|t| k.tools.iter().any(|own| own == t))
        })
    }

    /// Every server kept this session, stopped ones too.
    pub fn kept(&self) -> Vec<Kept> {
        locked(&self.0.lists).kept.clone()
    }

    /// Looks at each running server's exit watch: one newly gone is marked
    /// and moves the generation, so its tools leave at the next turn.
    fn look(&self, lists: &Lists) {
        for server in &lists.servers {
            if server.look() {
                self.0.generation.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    fn add(&self, kept: Kept, server: Server) {
        let mut lists = locked(&self.0.lists);
        lists.kept.push(kept);
        lists.servers.push(server);
        self.0.generation.fetch_add(1, Ordering::SeqCst);
    }

    /// The servers seen gone for at least [`SETTLE`] (so the reason is
    /// whole) and not said yet, each with why, taken out and marked
    /// stopped. Their tools left the list when they were first seen gone
    /// (the generation moved then). `run`'s tick asks every 250 ms, so a
    /// server that dies while idle is said within about a second.
    pub fn stopped(&self) -> Vec<(String, String)> {
        self.take_stopped(|at, _| at.elapsed() >= SETTLE)
    }

    /// At the session's end: every server seen gone, once the reasons of
    /// those seen within [`SETTLE`] have settled (at most that long) — but
    /// after `interrupted`, the owner's Ctrl+C, which reaches them too
    /// (they share yobuild's process group), one first seen after it or
    /// ended by its SIGINT is not said stopped: the session stopped it.
    pub async fn stopped_at_end(&self, interrupted: Option<Instant>) -> Vec<(String, String)> {
        let wait = {
            let lists = locked(&self.0.lists);
            self.look(&lists);
            lists
                .servers
                .iter()
                .filter_map(|s| s.gone())
                .map(|(at, _)| SETTLE.saturating_sub(at.elapsed()))
                .max()
        };
        if let Some(wait) = wait.filter(|w| !w.is_zero()) {
            tokio::time::sleep(wait).await;
        }
        self.take_stopped(|at, why| {
            interrupted.is_none_or(|ctrl_c| at < ctrl_c && !why.contains(SIGINT))
        })
    }

    /// The servers seen gone whose first sighting `say` accepts, taken out
    /// and marked stopped.
    fn take_stopped(&self, say: impl Fn(Instant, &str) -> bool) -> Vec<(String, String)> {
        let mut lists = locked(&self.0.lists);
        self.look(&lists);
        let mut out = Vec::new();
        lists.servers.retain(|s| match s.gone() {
            Some((at, why)) if say(at, &why) => {
                out.push((s.name.clone(), why));
                false
            }
            _ => true,
        });
        for (name, why) in &out {
            if let Some(k) = lists.kept.iter_mut().find(|k| &k.name == name) {
                k.stopped = Some(why.clone());
            }
        }
        out
    }

    /// Stops every server (yoagent's stdio `close` kills it; it returns no
    /// error of its own, but another transport's is logged).
    pub async fn close(&self) {
        let servers = std::mem::take(&mut locked(&self.0.lists).servers);
        self.0.generation.fetch_add(1, Ordering::SeqCst);
        for server in servers {
            if let Err(e) = server.client.lock().await.close().await {
                tracing::warn!("stopping the loaded MCP server {}: {e}", server.name);
            }
        }
    }
}

#[async_trait::async_trait]
impl ToolSource for Loaded {
    /// The running servers' tools: one seen gone is left out at once. A
    /// server's `list_changed` is not heeded: its tools are those the
    /// owner saw at the second question.
    async fn tools(&self) -> Vec<Arc<dyn AgentTool>> {
        let lists = locked(&self.0.lists);
        self.look(&lists);
        lists
            .servers
            .iter()
            .filter(|s| !s.is_gone())
            .flat_map(|s| s.tools.iter().cloned())
            .collect()
    }

    /// Looks first, so a server gone since moves it before yoagent reads it.
    fn generation(&self) -> u64 {
        self.look(&locked(&self.0.lists));
        self.0.generation.load(Ordering::SeqCst)
    }
}

/// yoagent's words for a server that ended or did not start, without the
/// program they name — the kept environment's wrapper (`/usr/bin/env`, or
/// `/bin/sh`), not the server — made one line and cut, the end of a
/// stderr tail kept (where a crash is): "Connection closed: MCP server
/// '<program>' exited with …; last stderr: …" and the exit watch's "MCP
/// server '<program>' exited with …" (or "closed its output (…)", "has
/// gone (…)", "was closed …") → "it exited with …; last stderr: …",
/// "Failed to spawn '<program>': …" → "it could not be started: …".
fn said(message: &str) -> String {
    let message = message.strip_prefix("Transport error: ").unwrap_or(message);
    let message = message
        .strip_prefix("Connection closed: ")
        .unwrap_or(message);
    let text = if let Some(rest) = message
        .strip_prefix("MCP server '")
        .and_then(|r| r.split_once("' "))
        .map(|(_, rest)| rest)
    {
        format!("it {rest}")
    } else if let Some((_, why)) = message
        .strip_prefix("Failed to spawn '")
        .and_then(|r| r.split_once("': "))
    {
        format!("it could not be started: {why}")
    } else {
        message.to_owned()
    };
    match text.split_once("; last stderr:") {
        Some((head, tail)) => {
            let head = clip(head, 150);
            format!(
                "{head}; last stderr: {}",
                clip_start(tail, 400 - head.chars().count())
            )
        }
        None => clip(&text, 400),
    }
}

/// `text` made one line, cut from its start to `max` characters (an
/// ellipsis marking the cut): the end of a stderr tail is kept.
fn clip_start(text: &str, max: usize) -> String {
    let line = clip(text, usize::MAX);
    let count = line.chars().count();
    match count > max {
        true => {
            let kept: String = line.chars().skip(count - max.saturating_sub(1)).collect();
            format!("…{}", kept.trim_start())
        }
        false => line,
    }
}

/// What `load_plugin` refuses, and the notice the owner hears it by.
struct Refused {
    message: String,
    level: NoticeLevel,
}

/// A refusal: a Warning notice.
fn refused(message: impl Into<String>) -> Refused {
    Refused {
        message: message.into(),
        level: NoticeLevel::Warning,
    }
}

/// The server failing once the owner allowed it to start (it did not
/// start, or its tool list is refused): an Error notice.
fn failed(message: impl Into<String>) -> Refused {
    Refused {
        message: message.into(),
        level: NoticeLevel::Error,
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
    files: Files,
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

impl Busy {
    /// The one way to set `busy`: with its guard.
    fn claim(shared: &Arc<Mutex<State>>, state: &mut State) -> Busy {
        state.busy = true;
        Busy(shared.clone())
    }
}

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

    /// `files`: the manifest's and its lock's names, for the block that
    /// keeps a server.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session: Session,
        files: Files,
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
            files,
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

    /// Step 1, the limits: the candidate, the version taken — `version`, or
    /// without one the version `find_plugin` offered — and the call
    /// counted.
    fn limits(
        &self,
        package: &str,
        version: Option<&str>,
    ) -> Result<(Candidate, String, Busy), Refused> {
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
        // A package found as several kinds is loaded as the MCP server it
        // also is; found only as another kind, it is refused as that.
        let candidates = self.find.candidates();
        let mut named = candidates.into_iter().filter(|c| c.package == package);
        let Some(first) = named.next() else {
            return Err(refused(format!(
                "{package} is not among this session's candidates: only a package find_plugin returned can be loaded"
            )));
        };
        let candidate = match first.kind {
            ExtendKind::Mcp => first,
            _ => match named.find(|c| c.kind == ExtendKind::Mcp) {
                Some(mcp) => mcp,
                None => {
                    return Err(refused(format!(
                        "{package} is a {}: load_plugin loads MCP servers only so far (loading pi and DSH packages comes in a later step); to use it, name it in the manifest as a [[plugin]] (nothing was counted)",
                        first.kind.label()
                    )))
                }
            },
        };
        let version = match (&candidate.version, version) {
            (Err(why), _) => {
                return Err(refused(format!("{package} is not loadable: {why}")))
            }
            (Ok(v), Some(version)) if v != version => {
                return Err(refused(format!(
                    "{package}@{version} is not the version find_plugin offered: {package}@{v} (or give no `version`)"
                )))
            }
            (Ok(v), _) => v.clone(),
        };
        if let Some(k) = self
            .loaded
            .kept()
            .into_iter()
            .find(|k| k.package == package)
        {
            return Err(refused(match k.stopped {
                None => format!("{package} is loaded already this session, as {}", k.name),
                Some(why) => format!(
                    "{package} was loaded this session and stopped ({why}): it is not loaded again this session"
                ),
            }));
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
        Ok((candidate, version, Busy::claim(&self.state, &mut state)))
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

    /// The arguments' shapes, then steps 1 to 7; the result's text.
    async fn load(&self, params: &Json, ctx: &ToolContext) -> Result<String, Refused> {
        let object = params
            .as_object()
            .ok_or_else(|| refused("the arguments must be an object"))?;
        if let Some(key) = object
            .keys()
            .find(|k| !["package", "version", "name", "args"].contains(&k.as_str()))
        {
            return Err(refused(format!(
                "unknown key `{}` (use package, version, name, args)",
                clip(key, 40)
            )));
        }
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
        let version = text("version")?;
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
        let (candidate, version, _busy) = self.limits(&package, version.as_deref())?;
        let name = self.name(&package, given.as_deref())?;
        // 3. What it is handed.
        crate::manifest::loaded_arguments(&args).map_err(refused)?;
        if let Some(env) = candidate.required_env.as_ref().filter(|e| !e.is_empty()) {
            return Err(refused(format!(
                "the MCP registry lists environment variables it requires ({}): nothing hands a loaded server one",
                env.join(", ")
            )));
        }
        // 4. Installed, running nothing; its program and what its code
        // reads found by reading files.
        let Installed {
            bin,
            scripts,
            reads,
        } = self.install(&package, &version, ctx).await?;
        // 5. Ask: start it?
        let details = first_details(
            &candidate, &version, &name, &args, &self.cwd, &scripts, &reads,
        );
        let title =
            format!("Start the MCP server {package}@{version} as {name}? (asked by the agent)");
        self.ask(title, details, &package, "start it", ctx).await?;
        // 6. Started, its tools listed and judged.
        let (client, exit, listed) = self.start(&package, &bin, &args, ctx).await?;
        let own = tools_problem(&name, &listed).map_err(|e| failed(format!("{package}: {e}")))?;
        // 7. Ask: keep it?
        let title = format!(
            "Keep the MCP server {name}? It offers: {}",
            clip(&own.join(", "), 200)
        );
        if let Err(no) = self
            .ask(
                title,
                second_details(&name, &listed),
                &package,
                "keep it",
                ctx,
            )
            .await
        {
            // Not kept: stopped now (yoagent's stdio `close` kills it).
            let _ = client.close().await;
            return Err(no);
        }
        // Gone while the owner was asked: not announced as loaded.
        if let Some(why) = exit.exited().map(|why| said(&why)) {
            let _ = client.close().await;
            return Err(failed(format!(
                "{package} stopped while you were asked to keep it: {why}"
            )));
        }
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
            stopped: None,
        };
        self.session.notice(
            NoticeLevel::Info,
            format!(
                "The agent loaded {package}@{version} as {name}, for this session. {}",
                kept_text(std::slice::from_ref(&kept), &self.files)
            ),
        );
        self.loaded
            .add(kept, Server::new(name.clone(), tools, client, exit));
        Ok(format!(
            "Loaded {package}@{version} as {name}, for this session. Its tools are yours from your next step: {}. Each call asks the owner.",
            names.join(", ")
        ))
    }

    /// Step 4: a one-entry `mcp` set, every package in its tree published
    /// `min_age_days` ago, installed without its scripts; then its program
    /// and what its code reads, by reading files — all off the async
    /// threads. Stopped (npm killed, the partial folder removed) when the
    /// run is: the blocking thread may outlive this call by the moment npm
    /// takes to see `stop`, and a later load meanwhile makes its own
    /// `.partial-` folder (`install_set`'s rename decides).
    async fn install(
        &self,
        package: &str,
        version: &str,
        ctx: &ToolContext,
    ) -> Result<Installed, Refused> {
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
        let withheld = self.withheld.clone();
        let installing = tokio::task::spawn_blocking(move || {
            let how = crate::install::NpmRun {
                limit: NPM_TIME,
                stop: Some(&stop),
            };
            let (dir, scripts) = crate::install::install_one(
                home.as_deref(),
                crate::install::Set::Mcp,
                Path::new(""),
                &package_,
                &version_,
                before.as_deref(),
                &how,
            )
            .map_err(|e| format!("{package_}@{version_} could not be installed: {e}"))?;
            let code = dir.join("node_modules").join(&package_);
            let bin = bin(&code, &package_)?;
            Ok::<_, String>(Installed {
                bin,
                scripts,
                reads: reads(&code, &withheld),
            })
        });
        let out = tokio::select! {
            _ = ctx.cancel.cancelled() => return Err(refused("the run was stopped while it installed")),
            out = installing => out,
        };
        match out {
            Ok(Ok(done)) => Ok(done),
            Ok(Err(e)) => Err(refused(e)),
            Err(e) => Err(refused(format!("the install's task failed: {e}"))),
        }
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
    /// and `tools/list` each within [`START_TIME`]. Its failing is an
    /// Error notice; the run stopped meanwhile, a Warning.
    async fn start(
        &self,
        package: &str,
        bin: &Path,
        args: &[String],
        ctx: &ToolContext,
    ) -> Result<(McpClient, ExitWatch, Vec<McpToolInfo>), Refused> {
        let did_not = |e: &dyn std::fmt::Display| {
            failed(format!("{package} did not start: {}", said(&e.to_string())))
        };
        let (program, argv, env) =
            super::without(bin, args, &HashMap::new(), &self.withheld).map_err(|e| did_not(&e))?;
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let inner = yoagent::mcp::StdioTransport::new(&program, &argv, Some(env))
            .await
            .map_err(|e| did_not(&e))?;
        // Without a watch its death would go unseen: refused (yoagent's
        // stdio transport always gives one).
        let Some(exit) = inner.exit_watch() else {
            let _ = inner.close().await;
            return Err(did_not(&"yoagent gives no exit watch for its transport"));
        };
        let mut client = McpClient::from_transport(Box::new(inner))
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
            _ = ctx.cancel.cancelled() => Err(refused("the run was stopped while it started")),
            listed = started => listed.map_err(|e| did_not(&e)),
        };
        if listed.is_err() {
            // yoagent's stdio `close` kills it and returns no error.
            let _ = client.close().await;
        }
        listed.map(|listed| (client, exit, listed))
    }
}

/// The name a package's server gets: without its scope, a leading
/// `server-` or `mcp-server-` and a trailing `-mcp` or `-mcp-server`; when
/// nothing is left, or only `mcp`, `server` or `mcp-server` (`@eslint/mcp`),
/// the scope; unscoped with nothing left, the package's name as it is.
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
/// digits, `-` and `_` (not `.`, which a manifest's name may hold: tool
/// names take none), no `__` nor a `_` at its end (`a_` + `__` + `_x`
/// would read as `a` + `__` + `__x`), short enough for `<name>__<tool>`.
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
    if name.ends_with('_') {
        return Some("ends in `_`, which would run into the `__` before its tools'");
    }
    if name.len() + 3 > 64 {
        return Some("is too long: `<name>__<tool>` must fit in 64 characters");
    }
    None
}

/// The tools as the server names them, once each can be offered as
/// `<name>__<tool>`; else why not (none listed, a name a model cannot take,
/// one listed twice).
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
/// named like the package (`load_plugin` takes no `bin`: a server with
/// several and none so named is refused).
fn bin(code: &Path, package: &str) -> Result<PathBuf, String> {
    match super::bin_of(code, package, None) {
        Err(e) if e.contains("several bins") => {
            let unscoped = package.rsplit('/').next().unwrap_or(package);
            super::bin_of(code, package, Some(unscoped)).map_err(|second| {
                format!(
                    "{package} has several programs (its `bin`) and yobuild cannot tell which to start: {second}"
                )
            })
        }
        other => other,
    }
}

/// What its code reads that it would not get — set here, and not in the
/// kept environment, or a credential yoagent reads (`withheld`) — and
/// what the scan missed, if anything.
fn reads(code: &Path, withheld: &[String]) -> Reads {
    let scan = crate::environment::names_read(code);
    let missed = match (scan.files, scan.missed()) {
        (0, None) => Some("no JavaScript or TypeScript code was found in it to scan".into()),
        (_, missed) => missed,
    };
    Reads {
        names: scan
            .names
            .into_iter()
            .filter(|name| std::env::var_os(name).is_some_and(|v| !v.is_empty()))
            .filter(|name| withheld.contains(name) || !crate::environment::kept(name.as_ref()))
            .collect(),
        missed,
    }
}

/// [`reads`]: the names, and what the scan missed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Reads {
    names: Vec<String>,
    missed: Option<String>,
}

/// What step 4 found: the program, the packages with install scripts (not
/// run), what its code reads.
struct Installed {
    bin: PathBuf,
    scripts: Vec<String>,
    reads: Reads,
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
    reads: &Reads,
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
    if !reads.names.is_empty() {
        lines.push(format!(
            "Its code reads {}, set here, which it will not get (it starts with the kept environment)",
            reads.names.join(", ")
        ));
    }
    if let Some(missed) = &reads.missed {
        lines.push(format!(
            "Its code was scanned in part for what it reads ({missed}): there may be more"
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
        "Propose an MCP server find_plugin returned, for this session (at the version it offered). \
         The owner is asked before it starts, and again with its tools before you get them; its \
         tools are yours from your next step, each call asking the owner. A refusal says why: do \
         not propose a package the owner blocked again."
    }
    fn parameters_schema(&self) -> Json {
        json!({
            "type": "object",
            "properties": {
                "package": { "type": "string", "description": "The npm package, as find_plugin gave it." },
                "version": { "type": "string", "description": "The version find_plugin said could be loaded; leave it out to take that one." },
                "name": { "type": "string", "description": "What to call it (its tools are <name>__<tool>); by default from the package's name." },
                "args": { "type": "array", "items": { "type": "string" }, "description": "The server's own arguments, as its documentation gives them." }
            },
            "required": ["package"]
        })
    }

    /// Every refusal, a malformed call's too, is the result and a notice.
    async fn execute(&self, params: Json, ctx: ToolContext) -> Result<ToolResult, ToolError> {
        match self.load(&params, &ctx).await {
            Ok(text) => Ok(ToolResult {
                content: vec![Content::Text { text }],
                details: json!({}),
            }),
            Err(Refused { message, level }) => {
                let package = params["package"].as_str().unwrap_or("?");
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
