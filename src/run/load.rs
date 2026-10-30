//! `load_plugin` (`[extend]`, design §14): the agent proposes one MCP stdio
//! server, pi package or DSH package that `find_plugin` returned this
//! session; yobuild installs it without running anything, asks the owner,
//! starts it and lists its tools, asks again with the tools in front of
//! them, and only then offers them — through [`Loaded`], a tool source
//! yoagent asks again at the next turn (yoagent 0.25.14's `generation`); a
//! server's leave that list once yoagent's exit watch (0.25.15) says it has
//! gone. A pi or DSH package runs in a Node runtime of its own, marked
//! restricted (yoagent-rutis 0.9: tools only; `package.rs`). Every loaded
//! tool asks, call by call (the gate's
//! [`Gate::with_asked`](crate::builtins::Gate::with_asked)). For the session
//! only: nothing is written to `agent.toml` or its lock.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value as Json};
use yoagent::mcp::{ExitWatch, McpClient, McpToolAdapter, McpToolInfo, McpTransport};
use yoagent::tool_source::ToolSource;
use yoagent::{AgentTool, Content, ToolContext, ToolError, ToolResult};

use crate::builtins::{find_plugin, Candidate, FindPlugin};
use crate::frontend::host::PluginHost;
use crate::frontend::session::UI_TIMEOUT;
use crate::frontend::{NoticeLevel, Session, UiRequest};
use crate::manifest::{Extend, ExtendKind, Manifest};
use crate::run::host::clip;

/// Starting the server, and listing its tools: each.
const START_TIME: Duration = Duration::from_secs(30);
/// Closing a server: a remote's DELETE to a host gone quiet would
/// otherwise hold the end of a run, or of the session, for minutes.
const CLOSE_TIME: Duration = Duration::from_secs(5);

/// Closes `client` within [`CLOSE_TIME`]; what fails is logged, not said
/// (the server is left either way).
async fn close_quietly(client: &McpClient, name: &str) {
    match tokio::time::timeout(CLOSE_TIME, client.close()).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!("stopping the loaded MCP server {name}: {e}"),
        Err(_) => tracing::warn!(
            "stopping the loaded MCP server {name}: no answer within {} s; left",
            CLOSE_TIME.as_secs()
        ),
    }
}
/// Each npm call of the install.
const NPM_TIME: Duration = Duration::from_secs(300);
const DESCRIPTION_CHARS: usize = 300;
const SCHEMA_CHARS: usize = 400;
const DAY: i64 = 24 * 60 * 60;

mod package;
mod remote;
pub(crate) use package::Plugins;

/// A server or package the owner kept this session, as the end of the
/// session says it. Made only by `load` once both questions were allowed:
/// `name` passed [`name_problem`] and `tools` [`tools_problem`] (each once,
/// each `<name>__<tool>` a tool name), which [`Loaded::asks`] relies on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Kept {
    pub kind: ExtendKind,
    pub name: String,
    pub package: String,
    pub version: String,
    /// A server's arguments (a package takes none).
    pub args: Vec<String>,
    /// As the server or plugin names them (the agent sees
    /// `<name>__<tool>`).
    pub tools: Vec<String>,
    /// Why it stopped, once it was seen to.
    pub stopped: Option<String>,
    /// A remote server's address and headers: `Some` exactly when `kind`
    /// is remote (whose `package` is its registry name, `version` empty).
    pub remote: Option<KeptRemote>,
}

/// A remote server as the block keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeptRemote {
    pub url: String,
    pub host: String,
    /// Each header sent, its value as written (`Bearer ${CTX_KEY}`): the
    /// variable the owner named, never its value.
    pub headers: Vec<(String, String)>,
}

impl Kept {
    /// The `[[mcp]]` (or `[[plugin]]`) block that would keep it.
    pub fn block(&self) -> String {
        let quoted = |items: &[String]| {
            items
                .iter()
                .map(|t| Json::String(t.clone()).to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let source = match (self.kind, &self.remote) {
            (ExtendKind::Remote, Some(remote)) => {
                let mut lines = format!("url  = {}\n", Json::String(remote.url.clone()));
                if !remote.headers.is_empty() {
                    let headers: Vec<String> = remote
                        .headers
                        .iter()
                        .map(|(name, value)| format!("{name} = {}", Json::String(value.clone())))
                        .collect();
                    lines.push_str(&format!("headers = {{ {} }}\n", headers.join(", ")));
                }
                ("mcp", lines)
            }
            (kind, _) => {
                let (table, key) = match kind {
                    ExtendKind::Pi => ("plugin", "pi   "),
                    ExtendKind::Dsh => ("plugin", "dsh  "),
                    _ => ("mcp", "npm  "),
                };
                let spec = Json::String(format!("{}@{}", self.package, self.version));
                (table, format!("{key}= {spec}\n"))
            }
        };
        let mut block = format!(
            "[[{}]]\nname = {}\n{}",
            source.0,
            Json::String(self.name.clone()),
            source.1
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
/// (`agent.toml`, `agent.lock`; `handoff.toml`, `handoff.lock`), and what a
/// pi or DSH package put in it would share with the manifest's plugins of
/// its kind ([`shared`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Files {
    pub manifest: String,
    pub lock: String,
    pub shared: Vec<(ExtendKind, String)>,
}

/// The grants a pi or DSH package added to `manifest` would have to share
/// with its plugins of that kind (design §12, "One rule for both
/// ecosystems"), said when they have any: pi's `model` and `env`, the DSH
/// plugins' grants.
pub(crate) fn shared(manifest: &Manifest) -> Vec<(ExtendKind, String)> {
    use crate::manifest::PluginKind;
    let mut out = Vec::new();
    let pi = manifest
        .plugins
        .iter()
        .find(|p| matches!(p.kind, PluginKind::Pi(_)));
    if let Some(p) = pi {
        let mut grants = Vec::new();
        if p.grants.model.is_some() {
            grants.push("`model`".to_owned());
        }
        if !p.grants.env.is_empty() {
            grants.push(format!("`env` ({})", p.grants.env.join(", ")));
        }
        if !grants.is_empty() {
            out.push((
                ExtendKind::Pi,
                format!("the pi plugins' {}", grants.join(" and ")),
            ));
        }
    }
    let dsh = manifest
        .plugins
        .iter()
        .find(|p| matches!(p.kind, PluginKind::Dsh(_)));
    if let Some(p) = dsh {
        let said = p.grants.said();
        if !said.is_empty() {
            out.push((
                ExtendKind::Dsh,
                format!("the DSH plugins' grants ({})", said.join(", ")),
            ));
        }
    }
    out
}

/// What the session loaded, said when it ends (on stderr) and as each is
/// kept (an Info notice): the `[[mcp]]` and `[[plugin]]` blocks that would
/// keep them, one that stopped marked so.
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
                let what = match &k.remote {
                    Some(remote) => format!("{} at {}", k.package, remote.host),
                    None => format!("{}@{}", k.package, k.version),
                };
                format!(
                    "{what} as {} (tools: {}{stopped})",
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
    for kind in [ExtendKind::Pi, ExtendKind::Dsh] {
        if !kept.iter().any(|k| k.kind == kind) {
            continue;
        }
        let (plugins, set) = match kind {
            ExtendKind::Pi => ("pi plugins", "pi"),
            ExtendKind::Dsh => ("DSH plugins", "dsh"),
            ExtendKind::Mcp | ExtendKind::Remote => continue,
        };
        text.push_str(&format!(
            "\nIn {} a {} runs with the other {plugins}, in the `{set}` runtime, its hooks too, its tools under their own names",
            files.manifest,
            kind.label()
        ));
        match files.shared.iter().find(|(k, _)| *k == kind) {
            Some((_, grants)) => {
                text.push_str(&format!(": give it {grants}, as they share them.\n"))
            }
            None => text.push_str(".\n"),
        }
    }
    if kept.iter().any(|k| k.remote.is_some()) {
        text.push_str(&format!(
            "\nA remote server's headers name the variables you gave: {} reads them from the environment when it connects, as any `[[mcp]]` header's.\n",
            files.manifest
        ));
    }
    let mut sets: Vec<&str> = Vec::new();
    for k in kept {
        let set = match k.kind {
            ExtendKind::Mcp => "mcp",
            // Nothing installed: in no set.
            ExtendKind::Remote => continue,
            ExtendKind::Pi => "pi",
            ExtendKind::Dsh => "dsh",
        };
        if !sets.contains(&set) {
            sets.push(set);
        }
    }
    let sets = match sets.as_slice() {
        [one] => format!("the lock's `{one}` set"),
        [rest @ .., last] => format!(
            "the lock's {} and `{last}` sets",
            rest.iter()
                .map(|s| format!("`{s}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        [] => return text,
    };
    text.push_str(&format!(
        "\nThe next run resolves {its} dependencies again ({sets}, without the age limit) and writes {}, which --locked needs.",
        files.lock
    ));
    text
}

/// How long a server is seen gone before it is said stopped: yoagent's
/// exit text settles within 0.5 s (the last of its stderr read).
const SETTLE: Duration = Duration::from_millis(600);

/// A server the owner kept, running.
struct Server {
    name: String,
    tools: Vec<Arc<dyn AgentTool>>,
    client: Arc<tokio::sync::Mutex<McpClient>>,
    /// yoagent's watch over its process, taken from the transport before
    /// the client was built: read without the `tokio::Mutex` yobuild keeps
    /// the client in (a call in flight holds it). `None` for a remote
    /// server, which has no process here.
    exit: Option<ExitWatch>,
    /// Why a remote server is gone, once a call found it had ended its
    /// session (`remote::Ending` sets it).
    ended: Arc<Mutex<Option<String>>>,
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
            exit: Some(exit),
            ended: Arc::default(),
            gone: Mutex::default(),
        }
    }

    /// A remote server: seen gone only once a call finds its session ended.
    fn remote(
        name: String,
        tools: Vec<Arc<dyn AgentTool>>,
        client: Arc<tokio::sync::Mutex<McpClient>>,
        ended: Arc<Mutex<Option<String>>>,
    ) -> Self {
        Server {
            name,
            tools,
            client,
            exit: None,
            ended,
            gone: Mutex::default(),
        }
    }

    /// Reads the watch: whether the server was newly seen gone. A reason
    /// read is kept when longer than the one before.
    fn look(&self) -> bool {
        let exited = self
            .exit
            .as_ref()
            .and_then(|e| e.exited())
            .map(|why| said(&why));
        let Some(why) = exited.or_else(|| locked(&self.ended).clone()) else {
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

/// A kept package found stopped ([`Loaded::packages_stopped`]).
pub(crate) struct Stopped {
    pub name: String,
    pub kind: ExtendKind,
    /// Its runtime, to remove.
    pub runtime: String,
    pub why: String,
}

/// A pi or DSH package the owner kept, running in its runtime.
struct Running {
    name: String,
    /// Its added runtime (`loaded-<n>`).
    runtime: String,
    /// Its adapter's handler: the one whose tools it offers.
    handler: String,
    /// Its tools as the plugin names them, as the owner saw them.
    tools: Vec<String>,
}

#[derive(Default)]
struct Lists {
    /// The servers running (or found gone, until [`Loaded::stopped`]).
    servers: Vec<Server>,
    /// The packages running (until found stopped).
    packages: Vec<Running>,
    /// Packages being loaded: their adapter's handler, and the name their
    /// tools take (read for the second question).
    loading: Vec<(String, String)>,
    /// Every server and package kept this session, stopped ones too: never
    /// shrinks, and holds each before its tools are offered (one lock).
    kept: Vec<Kept>,
}

#[derive(Default)]
struct Shared {
    lists: Mutex<Lists>,
    /// Moves on each load, each server or package found gone and the
    /// close; only forward.
    generation: AtomicU64,
    /// The bridge's source of restricted handlers registered mid-run
    /// (yoagent-rutis 0.9's `late_tools`): a package loaded during a run.
    late: OnceLock<yoagent_rutis::LateTools>,
}

/// What the agent loaded this session, as a tool source: the servers'
/// tools and, through the bridge's late source, the tools of packages kept
/// during the run; a generation bumped on each load and each found gone, so
/// yoagent offers the change from the next turn. A package's tools in later
/// runs come from the bridge's extension, named by
/// [`restricted_name`](Loaded::restricted_name). Clones share it (the
/// naming closure the bridge keeps holds it weakly: [`naming`](Loaded::naming)).
/// Dropped, the servers are stopped (each `kill_on_drop`); `run` closes it
/// at the session's end in any case.
#[derive(Clone, Default)]
pub(crate) struct Loaded(Arc<Shared>);

/// Takes a load's handler out of [`Lists::loading`] however the load ends.
struct Loading {
    loaded: Loaded,
    handler: String,
}

impl Drop for Loading {
    fn drop(&mut self) {
        locked(&self.loaded.0.lists)
            .loading
            .retain(|(h, _)| *h != self.handler);
    }
}

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

    /// Why `tool` is refused without asking, if it is: a tool of a package
    /// kept and stopped since. A run that started before it stopped still
    /// offers it (its tools came with the run's start); a server's leave
    /// at the next turn, as `Loaded` offers them.
    pub fn refused(&self, tool: &str) -> Option<String> {
        locked(&self.0.lists).kept.iter().find_map(|k| {
            let own = tool
                .strip_prefix(&k.name)
                .and_then(|t| t.strip_prefix("__"))
                .is_some_and(|t| k.tools.iter().any(|own| own == t));
            let why = k.stopped.as_ref().filter(|_| own)?;
            match k.kind {
                // Its tools left at the next turn (`Loaded` offers them).
                ExtendKind::Mcp | ExtendKind::Remote => None,
                ExtendKind::Pi | ExtendKind::Dsh => Some(format!(
                    "`{tool}` is a tool of the {} `{}`, which stopped this session ({why}): it is not run",
                    k.kind.label(),
                    k.name
                )),
            }
        })
    }

    /// [`restricted_name`](Self::restricted_name) for the bridge's extension
    /// (`with_restricted_tools`), holding `Loaded` weakly: the late source
    /// `Loaded` holds keeps the closure, which would otherwise keep `Loaded`.
    pub fn naming(&self) -> impl Fn(&str, &str) -> Option<String> + Send + Sync + 'static {
        let weak = Arc::downgrade(&self.0);
        move |handler, tool| Loaded(weak.upgrade()?).restricted_name(handler, tool)
    }

    /// Every server and package kept this session, stopped ones too.
    pub fn kept(&self) -> Vec<Kept> {
        locked(&self.0.lists).kept.clone()
    }

    /// The bridge's late source, once: set where the agent is built.
    pub fn set_late(&self, late: yoagent_rutis::LateTools) {
        if self.0.late.set(late).is_err() {
            tracing::warn!("the late tools were set twice; the first kept");
        }
    }

    /// The name a restricted handler's tool is offered under (the bridge
    /// extension's `with_restricted_tools`): a package being loaded,
    /// `<name>__<tool>` for each, so its tools can be read for the second
    /// question; a package kept and running, only the tools the owner saw;
    /// anything else — another restricted handler, a package blocked or
    /// stopped — `None`, left out.
    pub fn restricted_name(&self, handler: &str, tool: &str) -> Option<String> {
        let lists = locked(&self.0.lists);
        let loading = lists
            .loading
            .iter()
            .find(|(h, _)| h == handler)
            .map(|(_, name)| name);
        let running = lists
            .packages
            .iter()
            .find(|p| p.handler == handler && p.tools.iter().any(|t| t == tool))
            .map(|p| &p.name);
        loading.or(running).map(|name| format!("{name}__{tool}"))
    }

    /// A load of the package `name`, whose adapter registers `handler`, in
    /// progress until the guard drops.
    fn loading(&self, handler: &str, name: &str) -> Loading {
        locked(&self.0.lists)
            .loading
            .push((handler.to_owned(), name.to_owned()));
        Loading {
            loaded: self.clone(),
            handler: handler.to_owned(),
        }
    }

    /// The tools of the package being loaded as `name`, as the late source
    /// offers them this run (it asks the handler's `tools` once a run): for
    /// the second question. None read is said as such: the source logs a
    /// `tools` that failed, and tells nothing else.
    async fn read_tools(&self, name: &str) -> Result<Vec<Arc<dyn AgentTool>>, String> {
        let Some(late) = self.0.late.get() else {
            return Err("its tools cannot be read: the bridge's late tools are not set".into());
        };
        let prefix = format!("{name}__");
        let tools: Vec<_> = late
            .tools()
            .await
            .into_iter()
            .filter(|t| t.name().starts_with(&prefix))
            .collect();
        match tools.is_empty() {
            true => Err("it offered no tools as a loaded package gets them (in a runtime of its own, no grants, no config), or its adapter's `tools` failed (the log says)".into()),
            false => Ok(tools),
        }
    }

    fn add_package(&self, kept: Kept, running: Running) {
        let mut lists = locked(&self.0.lists);
        lists.kept.push(kept);
        lists.packages.push(running);
        self.0.generation.fetch_add(1, Ordering::SeqCst);
    }

    /// The packages whose runtime or a row has stopped: taken out and
    /// marked stopped — the run that loaded one drops its tools at the next
    /// turn, later runs leave them out, and a run started before it stopped
    /// has them [`refused`](Self::refused) without asking. `run`'s tick
    /// asks, with the host, and removes each one's runtime.
    pub fn packages_stopped(&self, host: &PluginHost) -> Vec<Stopped> {
        let mut lists = locked(&self.0.lists);
        let mut out = Vec::new();
        lists.packages.retain(|p| {
            let why = host.added_stopped(&p.runtime);
            if why.is_empty() {
                return true;
            }
            out.push((p.name.clone(), p.runtime.clone(), why.join("; ")));
            false
        });
        let mut said = Vec::new();
        for (name, runtime, why) in out {
            let k = lists.kept.iter_mut().find(|k| k.name == name);
            let k = k.expect("a running package is kept");
            k.stopped = Some(why.clone());
            said.push(Stopped {
                name,
                kind: k.kind,
                runtime,
                why,
            });
        }
        if !said.is_empty() {
            self.0.generation.fetch_add(1, Ordering::SeqCst);
        }
        said
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
        self.take_stopped(|at| at.elapsed() >= SETTLE)
    }

    /// At the session's end: every server seen gone, once the reasons of
    /// those seen within [`SETTLE`] have settled (at most that long). Each
    /// runs in a process group of its own (yoagent 0.25.16), so the owner's
    /// Ctrl+C never reaches one: one gone is its own doing.
    pub async fn stopped_at_end(&self) -> Vec<(String, String)> {
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
        self.take_stopped(|_| true)
    }

    /// The servers seen gone whose first sighting `say` accepts, taken out
    /// and marked stopped.
    fn take_stopped(&self, say: impl Fn(Instant) -> bool) -> Vec<(String, String)> {
        let mut lists = locked(&self.0.lists);
        self.look(&lists);
        let mut out = Vec::new();
        lists.servers.retain(|s| match s.gone() {
            Some((at, why)) if say(at) => {
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

    /// Stops every server (yoagent's stdio `close` kills it; a remote's
    /// sends DELETE), each within [`CLOSE_TIME`].
    pub async fn close(&self) {
        let servers = std::mem::take(&mut locked(&self.0.lists).servers);
        self.0.generation.fetch_add(1, Ordering::SeqCst);
        for server in servers {
            close_quietly(&*server.client.lock().await, &server.name).await;
        }
    }
}

#[async_trait::async_trait]
impl ToolSource for Loaded {
    /// The running servers' tools: one seen gone is left out at once. A
    /// server's `list_changed` is not heeded: its tools are those the
    /// owner saw at the second question.
    ///
    /// Then the packages' tools the late source offers this run: only a
    /// kept, running package's (the source still holds one blocked at the
    /// second question, or stopped since). Asked without the lists' lock:
    /// the source names tools through [`restricted_name`](Self::restricted_name).
    async fn tools(&self) -> Vec<Arc<dyn AgentTool>> {
        let mut tools: Vec<Arc<dyn AgentTool>> = {
            let lists = locked(&self.0.lists);
            self.look(&lists);
            lists
                .servers
                .iter()
                .filter(|s| !s.is_gone())
                .flat_map(|s| s.tools.iter().cloned())
                .collect()
        };
        if let Some(late) = self.0.late.get() {
            let late = late.tools().await;
            let lists = locked(&self.0.lists);
            let offered = |name: &str| {
                lists.packages.iter().any(|p| {
                    name.strip_prefix(&p.name)
                        .and_then(|t| t.strip_prefix("__"))
                        .is_some_and(|t| p.tools.iter().any(|own| own == t))
                })
            };
            tools.extend(late.into_iter().filter(|t| offered(t.name())));
        }
        tools
    }

    /// Looks first, so a server gone since moves it before yoagent reads it;
    /// the late source's moves too (both only forward).
    fn generation(&self) -> u64 {
        self.look(&locked(&self.0.lists));
        let late = self.0.late.get().map_or(0, |l| l.generation());
        self.0.generation.load(Ordering::SeqCst).wrapping_add(late)
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
    /// Runtimes added for packages: each load's is `loaded-<n>`, a name
    /// never used again (the loader keeps a name's resolution).
    runtimes: u32,
    /// Names a package load started a runtime under: not given again this
    /// session, kept or not — the bridge's late source keeps a run's tools
    /// by name, and would offer the earlier load's under it.
    tried: BTreeSet<String>,
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
    /// The credentials yoagent reads (the helper scripts' environment).
    keys: Arc<Vec<String>>,
    /// How a remote's credential is read from the environment.
    lookup: remote::Lookup,
    /// What a pi or DSH package runs on; `None` in `check`, which loads
    /// nothing.
    plugins: Option<Plugins>,
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
        plugins: Option<Plugins>,
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
            keys: Arc::new(manifest.key_variables()),
            lookup: remote::environment(),
            plugins,
            state: Arc::default(),
        }
    }

    /// The environment a remote's credential is read from: a test's.
    #[cfg(test)]
    pub(crate) fn with_lookup(mut self, lookup: remote::Lookup) -> Self {
        self.lookup = lookup;
        self
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
        // also is, else as the first kind it was found as.
        let candidates = self.find.candidates();
        let named: Vec<Candidate> = candidates
            .into_iter()
            .filter(|c| c.package == package)
            .collect();
        let Some(first) = named.first().cloned() else {
            return Err(refused(format!(
                "{package} is not among this session's candidates: only a package find_plugin returned can be loaded"
            )));
        };
        let candidate = named
            .into_iter()
            .find(|c| c.kind == ExtendKind::Mcp)
            .unwrap_or(first);
        let version = match (candidate.remote(), &candidate.version, version) {
            // A remote has no version to take.
            (Some(_), _, Some(_)) => {
                return Err(refused(format!(
                    "{package}: {} (give no `version`)",
                    find_plugin::NO_VERSION
                )))
            }
            (Some(_), _, None) => String::new(),
            (None, version_, version) => match (version_, version) {
            (Err(why), _) => {
                return Err(refused(format!("{package} is not loadable: {why}")))
            }
            (Ok(v), Some(version)) if v != version => {
                return Err(refused(format!(
                    "{package}@{version} is not the version find_plugin offered: {package}@{v} (or give no `version`)"
                )))
            }
            (Ok(v), _) => v.clone(),
            },
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
                "this session has loaded {kept} plugins, as many as it may ([extend] max_loads {})",
                self.extend.max_loads
            )));
        }
        Ok((candidate, version, Busy::claim(&self.state, &mut state)))
    }

    /// Step 2, the name: given, or the package's.
    fn name(&self, candidate: &Candidate, given: Option<&str>) -> Result<String, Refused> {
        let name = match (given, candidate.remote()) {
            (Some(name), _) => name.to_owned(),
            (None, Some(_)) => remote::default_name(&candidate.package),
            (None, None) => default_name(&candidate.package),
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
        if locked(&self.state).tried.contains(&name) {
            return Err(pass_another(format!(
                "the name {name:?} was used by an earlier load this session"
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
        let name = self.name(&candidate, given.as_deref())?;
        if let Some(remote) = candidate.remote() {
            if !args.is_empty() {
                return Err(refused(
                    "a remote MCP server takes no `args` (they are a stdio server's)",
                ));
            }
            return self.load_remote(&candidate, remote, &name, ctx).await;
        }
        if candidate.kind != ExtendKind::Mcp {
            if !args.is_empty() {
                return Err(refused(format!(
                    "a {} takes no `args` (they are an MCP server's)",
                    candidate.kind.label()
                )));
            }
            return self
                .load_package(&candidate, &package, &version, &name, ctx)
                .await;
        }
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
        let withheld = self.withheld.clone();
        let package_ = package.clone();
        let Installed {
            bin,
            scripts,
            reads,
        } = self
            .install(
                crate::install::Set::Mcp,
                &package,
                &version,
                ctx,
                move |dir, scripts| {
                    let code = dir.join("node_modules").join(&package_);
                    Ok(Installed {
                        bin: bin(&code, &package_)?,
                        scripts,
                        reads: reads(&code, &withheld),
                    })
                },
            )
            .await?;
        // 5. Ask: start it?
        let details = first_details(
            &candidate,
            &version,
            &name,
            &args,
            &self.cwd,
            &scripts,
            &reads,
            &[],
        );
        let title =
            format!("Start the MCP server {package}@{version} as {name}? (asked by the agent)");
        self.ask(title, details, &package, "start it", ctx).await?;
        // 6. Started, its tools listed and judged.
        let (client, exit, listed) = self.start(&package, &bin, &args, ctx).await?;
        let names: Vec<String> = listed.iter().map(|t| t.name.clone()).collect();
        let own = tools_problem(&name, &names).map_err(|e| failed(format!("{package}: {e}")))?;
        // 7. Ask: keep it?
        let title = format!(
            "Keep the MCP server {name}? It offers: {}",
            clip(&own.join(", "), 200)
        );
        if let Err(no) = self
            .ask(
                title,
                second_details(
                    "the server",
                    listed.iter().map(|t| {
                        (
                            format!("{name}__{}", t.name),
                            t.description.clone().unwrap_or_default(),
                            t.input_schema.clone(),
                        )
                    }),
                ),
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
            kind: ExtendKind::Mcp,
            name: name.clone(),
            package: package.clone(),
            version: version.clone(),
            args,
            tools: own,
            stopped: None,
            remote: None,
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

    /// Step 4: a one-entry set of `set`, every package in its tree
    /// published `min_age_days` ago, installed without its scripts; then
    /// `then` with its folder and the install scripts not run — what it
    /// finds by reading files — all off the async threads. Stopped (npm
    /// killed, the partial folder removed) when the run is: the blocking
    /// thread may outlive this call by the moment npm takes to see `stop`,
    /// and a later load meanwhile makes its own `.partial-` folder
    /// (`install_set`'s rename decides).
    async fn install<T: Send + 'static>(
        &self,
        set: crate::install::Set,
        package: &str,
        version: &str,
        ctx: &ToolContext,
        then: impl FnOnce(PathBuf, Vec<String>) -> Result<T, String> + Send + 'static,
    ) -> Result<T, Refused> {
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
        let adapters = self
            .plugins
            .as_ref()
            .map_or_else(PathBuf::new, |p| p.adapters.clone());
        let installing = tokio::task::spawn_blocking(move || {
            let how = crate::install::NpmRun {
                limit: NPM_TIME,
                stop: Some(&stop),
            };
            let (dir, scripts) = crate::install::install_one(
                home.as_deref(),
                set,
                &adapters,
                &package_,
                &version_,
                before.as_deref(),
                &how,
            )
            .map_err(|e| format!("{package_}@{version_} could not be installed: {e}"))?;
            then(dir, scripts)
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
        // `stdio_options`: a process group of its own.
        let inner = yoagent::mcp::StdioTransport::with_options(
            &program,
            &argv,
            Some(env),
            super::stdio_options(),
        )
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

/// The tools as the server or plugin names them, once each can be offered
/// as `<name>__<tool>`; else why not (none listed, a name a model cannot
/// take, one listed twice).
fn tools_problem(name: &str, listed: &[String]) -> Result<Vec<String>, String> {
    if listed.is_empty() {
        return Err("it lists no tools".into());
    }
    let mut own: Vec<String> = Vec::new();
    for tool in listed {
        let full = format!("{name}__{tool}");
        if !super::tool_name(&full) {
            return Err(match super::tool_name(tool) {
                true => format!(
                    "with the prefix {name}__ its tool {tool:?} is {} characters, more than 64: load it under a shorter `name`",
                    full.len()
                ),
                false => format!(
                    "its tool {:?} cannot be offered to a model (names take up to 64 letters, digits, _ and -)",
                    clip(tool, 80)
                ),
            });
        }
        if own.contains(tool) {
            return Err(format!("it lists the tool {tool:?} twice"));
        }
        own.push(tool.clone());
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
/// it, `extra` (a package's), and the two warnings.
#[allow(clippy::too_many_arguments)]
fn first_details(
    candidate: &Candidate,
    version: &str,
    name: &str,
    args: &[String],
    cwd: &Path,
    scripts: &[String],
    reads: &Reads,
    extra: &[String],
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
    lines.extend(extra.iter().cloned());
    lines.push(
        "Its tools are not known until it starts: you will be asked again once they are.".into(),
    );
    lines.push("It runs with your rights: this is not a sandbox.".into());
    lines.join("\n")
}

/// The second question's details: each tool — as the agent gets it, its
/// description (`who`'s own claim) and parameters.
fn second_details(who: &str, tools: impl IntoIterator<Item = (String, String, Json)>) -> String {
    let mut lines = vec![format!(
        "Its tools, as {who} describes them (its own claims); each call will ask you:"
    )];
    for (name, description, schema) in tools {
        let description = match description.trim() {
            "" => "(no description)",
            text => text,
        };
        lines.push(format!("{name}: {}", clip(description, DESCRIPTION_CHARS)));
        lines.push(format!(
            "  parameters: {}",
            clip(&schema.to_string(), SCHEMA_CHARS)
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
        "Propose a plugin find_plugin returned, for this session (at the version it offered). \
         The owner is asked before it starts, and again with its tools before you get them; its \
         tools are yours from your next step, each call asking the owner. A refusal says why: do \
         not propose a package the owner blocked again."
    }
    fn parameters_schema(&self) -> Json {
        json!({
            "type": "object",
            "properties": {
                "package": { "type": "string", "description": "The npm package, or a remote server's MCP registry name, as find_plugin gave it." },
                "version": { "type": "string", "description": "The version find_plugin said could be loaded; leave it out to take that one." },
                "name": { "type": "string", "description": "What to call it (its tools are <name>__<tool>); by default from the package's name." },
                "args": { "type": "array", "items": { "type": "string" }, "description": "An MCP server's own arguments, as its documentation gives them (a pi or DSH package takes none)." }
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
