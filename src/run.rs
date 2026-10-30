//! `yobuild run`: an agent from its manifest — the model, its limits, the
//! built-ins, the plugins (by ecosystem), MCP servers, and a frontend.
//!
//! In three steps: every check first, before anything starts (a missing key
//! or package, a prompt that cannot be filled, a plugin yobuild cannot load
//! whole); then the runtimes, servers and tools (refusing two tools of one
//! name); then the frontend.
//!
//! Pinned plugins and servers are installed before anything starts
//! (`crate::install`: locked beside the manifest, into the cache); a rutis
//! plugin's runtime must be installed in its own package.
//!
//! The agent works in the process's working folder: yoagent's file tools
//! read relative paths from there, so the gate, facts and bash do too.

pub(crate) mod allowed;
pub mod check;
pub(crate) mod expect;
pub(crate) mod host;
pub(crate) mod load;
pub(crate) mod measure;
pub mod plugin_log;
pub(crate) mod plugins;
pub(crate) mod script;

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rutis::Ctx;
use yoagent::context::ExecutionLimits;
use yoagent::extension::Budget;
use yoagent::mcp::{HttpTransport, McpClient, McpToolAdapter};
use yoagent::provider::{prices, MockProvider, ModelConfig};
use yoagent::tools::{
    BashTool, EditFileTool, ListFilesTool, ReadFileTool, SearchTool, WriteFileTool,
};
use yoagent::{Agent, AgentEvent, AgentTool, Content};
use yoagent_rutis::RutisBridge;

use crate::builtins::{AskOwner, Facts, FindPlugin, Gate, OfferedCheck, Scratch, Sources};
use crate::frontend::host::{DshHome, PluginHost, PluginHostBuilder, Row};
use crate::frontend::session::UI_TIMEOUT;
use crate::frontend::{
    services, web, ClientMessage, NoticeLevel, RunOutcome, ServerMessage, Session,
};
use crate::install;
use crate::manifest::{
    ExtendKind, Gate as GateMode, Manifest, Mcp, PluginKind, Source, Tools as ToolSet, Transport,
    EXTEND_TOOLS,
};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Where yobuild's own runtime files are: `frontend` holds `plugins/` (the
/// terminal UI and yobuild's own plugins) beside `web/lib.js`; `adapters`
/// holds yoagent-rutis's `pi/` and `dsh/`.
#[derive(Debug, Clone)]
pub struct Layout {
    pub frontend: PathBuf,
    pub adapters: PathBuf,
}

impl Layout {
    /// The checkout this crate was built from (`frontend/`, and `adapters/`
    /// set up by `scripts/try-coding-agent.sh --setup-only`): for the
    /// tests and `examples/coding_agent.rs`.
    pub fn checkout() -> Self {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        Self {
            frontend: repo.join("frontend"),
            adapters: repo.join("adapters"),
        }
    }

    /// The runtime sets `manifest` needs, installed in the cache (`home`,
    /// else `YOBUILD_HOME` or `~/.yobuild`; design §13): `adapters` for a pi
    /// or DSH plugin, `ui` for the terminal UI (`terminal`) or one of
    /// yobuild's own plugins. What it does not need is named by a path
    /// that says so, never installed. `say` hears an install begin.
    pub fn installed(
        manifest: &Manifest,
        home: Option<&Path>,
        terminal: bool,
        say: &dyn Fn(&str),
    ) -> Result<Self, BoxError> {
        use crate::runtime::{installed, RuntimeSet};
        // A pi or DSH plugin, or one the agent may load (`[extend]`).
        let adapters = manifest
            .plugins
            .iter()
            .any(|p| matches!(p.kind, PluginKind::Pi(_) | PluginKind::Dsh(_)))
            || manifest.extend.as_ref().is_some_and(|e| {
                e.kinds
                    .iter()
                    .any(|k| matches!(k, ExtendKind::Pi | ExtendKind::Dsh))
            });
        let ui = terminal || manifest.plugins.iter().any(|p| p.builtin.is_some());
        if !adapters && !ui {
            return Ok(Self::unneeded());
        }
        let home = match home {
            Some(home) => home.to_path_buf(),
            None => crate::install::home()?,
        };
        let unneeded = Self::unneeded();
        Ok(Self {
            frontend: match ui {
                true => installed(&home, RuntimeSet::Ui, say)?,
                false => unneeded.frontend,
            },
            adapters: match adapters {
                true => installed(&home, RuntimeSet::Adapters, say)?,
                false => unneeded.adapters,
            },
        })
    }

    /// Paths no file is at, for a manifest that needs neither set.
    fn unneeded() -> Self {
        Self {
            frontend: PathBuf::from("(yobuild's ui runtime: not needed by this manifest)"),
            adapters: PathBuf::from("(yobuild's adapters runtime: not needed by this manifest)"),
        }
    }
}

impl Default for Layout {
    /// The checkout ([`Layout::checkout`]): the library's tests run from it;
    /// the `yobuild` binary uses [`Layout::installed`].
    fn default() -> Self {
        Self::checkout()
    }
}

/// Whether running `manifest` needs Node and npm: a plugin (each runs in a
/// Node runtime, or has its package installed), a pinned MCP server (npm
/// installs it), `[extend]` finding what npm lists (`find_plugin` searches
/// with npm; remote servers are the MCP registry's only), or the
/// terminal UI (`terminal`).
pub(crate) fn needs_node(manifest: &Manifest, terminal: bool) -> bool {
    terminal
        || manifest.extend.as_ref().is_some_and(|e| e.asks_npm())
        || !manifest.plugins.is_empty()
        || manifest.mcp.iter().any(|s| {
            matches!(
                &s.transport,
                Transport::Stdio {
                    source: Source::Package(_),
                    ..
                }
            )
        })
}

/// How to run an agent.
#[derive(Debug)]
#[non_exhaustive]
pub struct Options {
    pub manifest: Manifest,
    /// The model's replies from this file instead of the provider: a JSON
    /// list of `{ "text": … }` or `{ "tools": [{ "name", "arguments" }] }`.
    pub scripted: Option<PathBuf>,
    /// A first prompt.
    pub task: Option<String>,
    /// Run `task` (required) alone, with no frontend, and return when it
    /// ends; each tool call and notice is written to stderr meanwhile.
    pub once: bool,
    /// Say each run's plugin time and the runtimes' memory to every
    /// frontend (a notice); with `once` too. Else they go to the log (info).
    pub measure: bool,
    /// Where the web frontend listens, when the frontend is the web page
    /// (default `127.0.0.1:8787`; with `phone`, `0.0.0.0:8787`).
    pub addr: Option<SocketAddr>,
    pub layout: Layout,
    /// Refuse a lock (beside the manifest, `<stem>.lock`) with a set that
    /// is missing, stale, disagrees with itself or is no longer needed,
    /// instead of resolving it again (CI; an agent someone checked).
    pub locked: bool,
    /// yobuild's home, where pinned packages are installed (under
    /// `plugins/`; `None`: `YOBUILD_HOME`, else `~/.yobuild`).
    pub home: Option<PathBuf>,
}

impl Options {
    pub fn new(manifest: Manifest) -> Self {
        Self {
            manifest,
            scripted: None,
            task: None,
            once: false,
            measure: false,
            addr: None,
            layout: Layout::default(),
            locked: false,
            home: None,
        }
    }
}

/// The session budget's name: a run it stops says "session budget of …".
pub(crate) const SESSION_BUDGET: &str = "session budget";

/// What to do about a run that ended `outcome` with `error` (design §11):
/// a stop's fix (yoagent's limits, budgets and loop check; else what a
/// plugin's stop calls for); a failed or rejected run's when yobuild knows
/// one (`hints`). None for a completed or aborted run.
pub fn what_to_do(outcome: RunOutcome, error: Option<&str>) -> Option<&'static str> {
    let error = error.unwrap_or_default();
    match outcome {
        RunOutcome::Stopped => crate::hints::what_to_do(true, error),
        RunOutcome::Error | RunOutcome::Rejected => crate::hints::what_to_do(false, error),
        _ => None,
    }
}

/// What to do about a refusal `run` gives at start (a plugin, a server, the
/// terminal UI), when yobuild knows (`hints`).
pub fn what_to_do_at_start(error: &str) -> Option<&'static str> {
    crate::hints::hint(crate::hints::Scope::Start, error).map(|h| h.fix)
}

/// How the session ended: its last run's outcome (none when no run was
/// made), the notices it gave, and the agent (its messages) unless the
/// session was stopped with Ctrl+C.
#[non_exhaustive]
pub struct Ended {
    /// `Aborted` when Ctrl+C stopped a run in progress.
    pub outcome: Option<RunOutcome>,
    pub error: Option<String>,
    pub notices: Vec<(NoticeLevel, String)>,
    pub agent: Option<Agent>,
    /// The servers the agent loaded this session and the `[[mcp]]` blocks
    /// that would keep them (`[extend]`), to write once the session ends.
    pub loaded: Option<String>,
}

/// Runs the agent: until the frontend ends the session (terminal; an error
/// when the terminal UI stops on its own), Ctrl+C (web), or the task's run
/// ends (`once`).
pub async fn run(root: &Ctx, options: Options) -> Result<Ended, BoxError> {
    let started = Instant::now();
    // Ctrl+C, SIGHUP, SIGTERM from here on: each step that waits while the
    // session starts is raced against them (`Stops::until`, dropping what
    // it started), and the session's loop ends on one. Servers run in
    // process groups of their own, so a terminal's signal reaches only
    // yobuild, which must stop them.
    let mut stops =
        crate::stops::Stops::listen().map_err(|e| format!("cannot listen for Ctrl+C: {e}"))?;
    let cwd = std::env::current_dir()?;
    // Node and npm, before anything uses them (design §13).
    let terminal = options.manifest.frontend.terminal && !options.once;
    if needs_node(&options.manifest, terminal) {
        crate::runtime::node_and_npm()?;
    }
    // yobuild's own plugins (`yobuild:<name>`), given their files first.
    let options = Options {
        manifest: options.manifest.with_builtins(&options.layout.frontend)?,
        ..options
    };
    let checked = check(&options, &cwd)?;
    let Options {
        manifest,
        task,
        once,
        measure: options_measure,
        addr,
        layout,
        scripted,
        home,
        ..
    } = options;
    let options_scripted = scripted.is_some();
    let name = &manifest.agent.name;
    let terminal = manifest.frontend.terminal && !once;

    // A terminal UI ends the session when it quits, as does a `once` run.
    let (session, driver) = Session::new(terminal || once);
    // The runs plugins' wakes may start (design §12): none under `--once`.
    session.allow_wakes(if once { 0 } else { host::IDLE_WAKES });
    // What loads only in part: a warning every frontend gets (kept in the
    // session's history for those that come later; `--once` prints it).
    for note in &checked.notes {
        tracing::info!("{note}");
        session.notice(NoticeLevel::Warning, note.clone());
    }
    // An `env` grant that hands nothing over: a plugin that needs the key
    // fails when it uses it, in its own words; this says why before then
    // (a Warning). One that hands over some of its names only: an Info
    // notice (the others may be fallbacks).
    for unset in env_unset(&manifest, key_is_set) {
        let note = format!("plugin {}: {}", unset.plugin, unset_grant_note(&unset));
        tracing::info!("{note}");
        let level = match unset.all {
            true => NoticeLevel::Warning,
            false => NoticeLevel::Info,
        };
        session.notice(level, note);
    }
    let dyld = dyld_note(
        &manifest,
        std::env::vars_os().map(|(k, _)| k),
        cfg!(target_os = "macos"),
    );
    let notes = environment_note(&manifest, std::env::vars_os().map(|(k, _)| k), cfg!(unix))
        .into_iter()
        .chain(dyld)
        .chain(dsh_home_note(
            &checked.plan,
            std::env::var_os("YOAGENT_DSH_HOME"),
        ));
    for note in notes {
        tracing::info!("{note}");
        session.notice(NoticeLevel::Info, note);
    }
    // What changed in `agent.lock`.
    for said in checked.installed.said() {
        tracing::info!("{said}");
        session.notice(NoticeLevel::Info, said.clone());
    }
    // What the pi adapter only logs and a grant would change: said, once
    // (seen through `plugin_log::layer`, which the binary installs).
    let said = plugin_log::Said::new(&checked.plan.pi_names(), manifest.pi_model());
    let _watching = plugin_log::watch({
        let session = session.clone();
        move |line: &plugin_log::Line| {
            if let Some((_, note)) = said.note(line) {
                session.notice(NoticeLevel::Warning, note);
            }
        }
    });
    let scratch = match manifest.builtins.scratch {
        true => Some(Scratch::create(name)?),
        false => None,
    };
    // `_dsh_home`: kept until the session ends.
    let Started {
        bridge,
        host,
        ask_owner,
        runtimes,
        gate: judge,
        dsh_home: _dsh_home,
    } = stops
        .until(start(
            root,
            &manifest,
            &checked.plan,
            &layout,
            &session,
            terminal,
            &cwd,
            scratch.as_ref(),
            once,
        ))
        .await?;
    let plan = &checked.plan;
    // Shared with `load_plugin`, which adds a runtime for each package it
    // loads; the tick looks only when no load holds it.
    let host = Arc::new(tokio::sync::Mutex::new(host));

    // The tools: yoagent's (in `cwd`), `ask_owner`, each MCP server's; no
    // two of one name. (A plugin's tools come with each run; yoagent keeps
    // one of these over a plugin's of the same name, and says so.)
    let mut tools = Tools::default();
    tools.add("yoagent", tools_for(manifest.builtins.tools, &cwd));
    if let Some(ask) = &ask_owner {
        tools.add(AskOwner::TOOL, vec![Box::new(ask.clone())]);
    }
    // What the agent loads this session (design §14): offered from the next
    // turn, each tool asking.
    let loaded = load::Loaded::default();
    let files = load::Files {
        manifest: crate::install::file_name(&manifest.path),
        lock: crate::install::file_name(&crate::install::lock_path(&manifest)),
        shared: load::shared(&manifest),
    };
    if let (Some(extend), Some(sources)) = (&manifest.extend, &checked.sources) {
        // Loaded tools ask only through the gate: the manifest refuses
        // `[extend]` with `gate = "off"`, and this keeps that true here.
        if judge.is_none() {
            return Err(
                "[extend] needs the gate (`gate = \"off\"` would let loaded tools run unasked)"
                    .into(),
            );
        }
        let find = FindPlugin::new(session.clone(), extend.clone(), sources.clone());
        let load = load::LoadPlugin::new(
            session.clone(),
            files.clone(),
            &manifest,
            extend.clone(),
            find.clone(),
            loaded.clone(),
            once,
            home.clone(),
            &cwd,
            Some(load::Plugins {
                host: host.clone(),
                bridge: bridge.clone(),
                adapters: layout.adapters.clone(),
            }),
        );
        tools.add(FindPlugin::TOOL, vec![Box::new(find)]);
        tools.add(load::LoadPlugin::TOOL, vec![Box::new(load)]);
    }
    for server in &manifest.mcp {
        let found = stops
            .until(mcp(server, &checked.servers[&server.name]))
            .await?;
        tools.add(&format!("mcp {}", server.name), found);
    }
    tools.check()?;
    // The agent's own tools: never freed through a plugin's offer.
    let own: HashSet<String> = tools.from.iter().map(|(name, _)| name.clone()).collect();

    let prompt = checked.prompt.fill(&cwd, scratch.as_ref());
    let mut agent = match checked.provider {
        Some(provider) => Agent::from_provider(provider, checked.config.clone()),
        None => {
            Agent::from_config(checked.config.clone()).with_api_key(checked.key.unwrap_or_default())
        }
    }
    .with_system_prompt(prompt)
    // The session's id, for logs (a label is informational in yoagent).
    .with_run_label(session.id())
    .with_tools(tools.list)
    .with_tool_source(loaded.clone())
    .with_execution_limits(limits(&manifest))
    .with_extension({
        // A loaded package's runtime is restricted: its tools are named, and
        // offered, as `Loaded` says (only a kept package's, frozen at the
        // second question); one loaded mid-run comes through the late source,
        // which `Loaded` offers after the servers' tools.
        let extension = bridge.extension().with_restricted_tools(loaded.naming());
        loaded.set_late(extension.late_tools());
        // A plugin that is the approval policy: no run goes on unguarded —
        // without a policy plugin up, every call is denied.
        match manifest.plugins.iter().any(|p| p.policy) {
            true => extension
                .require_policy()
                .with_policy_timeout(Some(UI_TIMEOUT + Duration::from_secs(30))),
            false => extension,
        }
    });
    let adapters = Adapters {
        pi: plan.pi_handler.clone(),
        dsh: plan.dsh_handler.clone(),
    };
    let other_owner = other_owner(&manifest, plan, bridge.registry().clone(), adapters.clone());
    let offered = offered_check(
        bridge.registry().clone(),
        offerers(&manifest, plan),
        adapters,
        own,
    );
    let asked = {
        let loaded = loaded.clone();
        Arc::new(move |tool: &str| loaded.asks(tool)) as crate::builtins::AskedCheck
    };
    // A stopped package's tools, still offered by a run that started
    // before it stopped: refused without asking.
    let refused = {
        let loaded = loaded.clone();
        Arc::new(move |tool: &str| loaded.refused(tool)) as crate::builtins::RefusedCheck
    };
    if let Some(gate) = judge.map(|g| {
        agent_gate(g, &manifest, offered)
            .with_asked(asked)
            .with_refused(refused)
    }) {
        agent = agent.with_extension(gate);
        // A plugin's allowed tools, checked once its tools are offered.
        let names: Vec<(String, String)> = manifest
            .plugins
            .iter()
            .flat_map(|p| p.allow.iter().map(|t| (p.name.clone(), t.clone())))
            .collect();
        if !names.is_empty() {
            agent = agent.with_extension(
                allowed::AllowedCheck::new(session.clone(), names).with_other_owner(other_owner),
            );
        }
    }
    if let Some(report) = &manifest.builtins.report {
        agent = agent.with_extension(report.clone());
    }
    if manifest.builtins.facts {
        let mut facts = Facts::new(session.clone(), &cwd);
        if let Some(ask) = &ask_owner {
            facts = facts.with_open_questions(ask.open_questions());
        }
        agent = agent.with_extension(facts);
    }
    if let Some(budget) = checked.session_budget {
        agent = agent.with_extension(budget);
    }
    if let Some(budget) = checked.budget {
        agent = agent.with_extension(budget);
    }
    // Last: each run's plugin time and the runtimes' memory (design §9).
    if !runtimes.is_empty() {
        let registry = bridge.registry().clone();
        let say = (options_measure || once).then(|| session.clone());
        let names = vec![
            (plan.pi_handler.clone(), "pi-extensions".to_owned()),
            (plan.dsh_handler.clone(), "dsh-tools".to_owned()),
        ];
        agent = agent.with_extension(measure::Measure::new(say, registry, runtimes, names));
    }
    // Last of all, in a scripted run: each call's `expect`, checked against
    // the result the model sees.
    if options_scripted {
        agent = agent.with_extension(expect::Expect::new(session.clone()));
    }
    eprintln!("{name}: ready in {:.1}s", started.elapsed().as_secs_f64());

    let ui = "terminal-ui";
    let mut served = None;
    if terminal {
        let row =
            Row::new(ui, layout.frontend.join("plugins/terminal-ui.ts").display()).runtime("ui");
        stops
            .until(async { host.lock().await.load([row]).await })
            .await?;
    } else if !once {
        let phone = manifest.frontend.phone;
        let addr = addr.unwrap_or_else(|| match phone {
            true => "0.0.0.0:8787".parse().expect("an address"),
            false => "127.0.0.1:8787".parse().expect("an address"),
        });
        let web = stops.until(web::serve(session.clone(), addr)).await?;
        // Every interface: a link the phone can open.
        let url = match (web.addr().ip().is_unspecified(), lan_ip()) {
            (true, Some(ip)) => format!("http://{ip}:{}/?t={}", web.addr().port(), web.token()),
            (true, None) => {
                eprintln!("{name}: no address on the local network found; use this machine's");
                web.url()
            }
            _ => web.url(),
        };
        // The URL on stdout (what a launcher reads); a reader gone does not
        // end the agent (`println!` would panic on a closed pipe).
        let _ = writeln!(std::io::stdout(), "{name}: {url}  (Ctrl+C to stop)");
        if task.is_some() {
            eprintln!("{name}: the task starts when the page is open");
        }
        served = Some(web);
    }

    // What the frontends see; in `once`, written out as it comes.
    let (seen, mut ended_runs, watching) = watch(&session, once);
    // With a frontend, the task waits for it: a question its run asks must
    // reach the owner (with nobody there it would get its default — a
    // refusal — unseen). `--once` has no owner to wait for.
    let mut task = task;
    if once {
        if let Some(task) = task.take() {
            session.send(ClientMessage::Prompt { text: task });
        }
    }
    let mut driving = tokio::spawn(driver.run(agent));
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let mut reported = std::collections::HashSet::new();
    let mut quitting = false;
    // Until the session ends: the frontend quits (terminal), its run ends
    // (`once`), or Ctrl+C (or SIGHUP, SIGTERM). Meanwhile a plugin that
    // stops is said.
    let end = loop {
        tokio::select! {
            joined = &mut driving => break End::Driven(Box::new(joined)),
            by = stops.next() => {
                // As the driver sees it: the observer's copy may lag.
                break End::Interrupted {
                    running: session.is_running(),
                    by,
                };
            }
            changed = ended_runs.changed(), if once && !quitting => {
                if changed.is_err() || *ended_runs.borrow_and_update() > 0 {
                    session.send(ClientMessage::Quit);
                    quitting = true;
                }
            }
            _ = tick.tick() => {
                if task.is_some() && session.frontends() > 0 {
                    let text = task.take().expect("a task");
                    session.send(ClientMessage::Prompt { text });
                }
                // A loaded server gone, its reason settled: said, and marked
                // stopped (its tools left when it was first seen gone).
                for (server, why) in loaded.stopped() {
                    tracing::error!("the loaded MCP server {server} stopped: {why}");
                    session.notice(
                        NoticeLevel::Error,
                        format!("The loaded MCP server `{server}` stopped: {why}"),
                    );
                }
                // The host is `load_plugin`'s while it starts a package (up
                // to a minute or so): what needs it is looked at a later
                // tick — the terminal UI's end among it.
                let Ok(host_now) = host.try_lock() else { continue };
                // A row that fails, stops, or loses its runtime: said once.
                // (The terminal UI's own row is watched below; a loaded
                // package's are `Loaded`'s.)
                for stopped in host_now.stopped(&[ui]) {
                    if reported.insert(stopped.clone()) {
                        tracing::error!("{stopped}");
                        session.notice(NoticeLevel::Error, format!("A plugin stopped: {stopped}"));
                        if let Some(fix) = what_to_do_at_start(&format!("a plugin stopped: {stopped}")) {
                            session.notice(NoticeLevel::Info, format!("What to do: {fix}."));
                        }
                    }
                }
                // A loaded package's runtime or row stopped: said, its tools
                // gone (`Loaded::packages_stopped`), and its runtime removed
                // — a process may be left of it — in a task of its own.
                for stopped in loaded.packages_stopped(&host_now) {
                    let label = stopped.kind.label();
                    tracing::error!("the loaded {label} {} stopped: {}", stopped.name, stopped.why);
                    session.notice(
                        NoticeLevel::Error,
                        format!("The loaded {label} `{}` stopped: {}", stopped.name, stopped.why),
                    );
                    let (host, session) = (host.clone(), session.clone());
                    tokio::spawn(async move {
                        if let Err(e) = host.lock().await.remove_runtime(&stopped.runtime).await {
                            tracing::warn!("removing a stopped package's runtime: {e}");
                            session.notice(
                                NoticeLevel::Warning,
                                format!("The loaded {label} `{}`'s runtime could not be stopped whole: {e}", stopped.name),
                            );
                        }
                    });
                }
                if terminal && !host_now.is_running(ui) {
                    break End::UiGone;
                }
            }
        }
    };
    let (agent, interrupted, failure) = match end {
        End::Driven(joined) => match *joined {
            Ok(agent) => (Some(agent), None, None),
            Err(e) => (None, None, Some(format!("the agent's task failed: {e}"))),
        },
        End::Interrupted { running, by } => {
            driving.abort();
            drop(driving.await);
            (None, running.then_some(by), None)
        }
        End::UiGone => {
            // Nobody is there to quit: the session would wait for ever.
            session.send(ClientMessage::Quit);
            let agent = driving.await.ok();
            let why = host.lock().await.stopped(&[]).join("; ");
            (agent, None, Some(format!("the terminal UI stopped: {why}")))
        }
    };
    drop(served);
    let mut ended = finish(seen, watching, agent, once, interrupted).await;
    // However it ended: what was loaded, and how to keep it (nothing is
    // written). Each was said as a notice when it was kept; the binary
    // writes this once the runtimes have stopped (the terminal UI may hold
    // the terminal until then), and on an error it is written here. A
    // server gone since the last tick is marked stopped in it (a Ctrl+C
    // never reaches one: each runs in a process group of its own).
    for (server, why) in loaded.stopped_at_end().await {
        tracing::error!("the loaded MCP server {server} stopped: {why}");
    }
    for stopped in loaded.packages_stopped(&*host.lock().await) {
        let label = stopped.kind.label();
        tracing::error!(
            "the loaded {label} {} stopped: {}",
            stopped.name,
            stopped.why
        );
    }
    let kept = loaded.kept();
    loaded.close().await;
    ended.loaded = (!kept.is_empty()).then(|| load::kept_text(&kept, &files));
    if let (Some(_), Some(text)) = (&failure, &ended.loaded) {
        eprintln!("{text}");
    }
    match failure {
        Some(failure) => Err(match ended.outcome {
            Some(outcome) => format!(
                "{failure} (the last run ended {outcome:?}{})",
                ended.error.map(|e| format!(": {e}")).unwrap_or_default()
            ),
            None => failure,
        }
        .into()),
        None => Ok(ended),
    }
}

/// Why the session ended.
enum End {
    /// The driver returned (or its task failed).
    Driven(Box<Result<Agent, tokio::task::JoinError>>),
    /// Ctrl+C (or SIGHUP, SIGTERM); whether a run was in progress.
    Interrupted {
        running: bool,
        by: crate::stops::Stop,
    },
    /// The terminal UI's runtime stopped.
    UiGone,
}

/// The gate the manifest asks for, as the host services judge with it: free
/// in the scratch folder; for `ask_owner` only when the built-in adds
/// yobuild's; for an MCP server's allowed tools (`<server>__<tool>`). The
/// agent's is [`agent_gate`] of it.
fn gate(
    manifest: &Manifest,
    session: &Session,
    cwd: &Path,
    scratch: Option<&Scratch>,
) -> Option<Gate> {
    if manifest.builtins.gate != GateMode::AllowList {
        return None;
    }
    let (servers, _) = allowed(manifest);
    let mut gate = Gate::new(session.clone(), cwd).with_allowed_tools(servers);
    if let Some(scratch) = scratch {
        gate = gate.with_scratch(scratch);
    }
    if manifest.builtins.ask_owner {
        gate = gate.with_allowed_tools([AskOwner::TOOL]);
    }
    if manifest.extend.is_some() {
        gate = gate.with_allowed_tools([FindPlugin::TOOL, load::LoadPlugin::TOOL]);
    }
    Some(gate)
}

/// The agent's gate: the host's (a clone, sharing the files the agent made,
/// so `host_approval.judge` knows them) plus a plugin's allowed tools, only
/// when `offered` says that plugin offered the tool that would run.
fn agent_gate(judge: Gate, manifest: &Manifest, offered: OfferedCheck) -> Gate {
    judge.with_tools_allowed_if(allowed(manifest).1, offered)
}

/// The tools the entries allow, as the model sees them: the MCP servers'
/// (`<server>__<tool>`; a server's tools are the agent's own, so the name
/// is enough), and the plugins' (as they are; a plugin's must also come from
/// that plugin).
fn allowed(manifest: &Manifest) -> (Vec<String>, Vec<String>) {
    let servers = manifest
        .mcp
        .iter()
        .flat_map(|m| m.allow.iter().map(move |t| format!("{}__{t}", m.name)))
        .collect();
    let plugins = manifest
        .plugins
        .iter()
        .flat_map(|p| p.allow.iter().cloned())
        .collect();
    (servers, plugins)
}

/// Who may offer a plugin's allowed tool, by the row that registered its
/// handler (the owner the host sees, rutis 0.12's `caller_row`; design §4
/// "Who a plugin is"): a pi plugin, the `pi` row's adapter handler and one
/// of the plugin's own extension files (`source`, said by the adapter); a
/// DSH plugin, the `dsh` row's adapter handler (all DSH plugins share it)
/// or one of the plugin's own rows; a rutis plugin, its row
/// `plugin:<name>`. Owners are compared whole: a handler registered from
/// no row ("language plugin (session …)") is nobody's.
#[derive(Debug, Clone, PartialEq)]
enum Offerer {
    Pi(Vec<PathBuf>),
    Dsh(Vec<String>),
    Rutis(String),
}

/// The adapters' rows (`run/plugins.rs`).
const PI_ROW: &str = "pi";
const DSH_ROW: &str = "dsh";

/// The adapters' handler names this process gave them (`Plan`).
#[derive(Clone)]
struct Adapters {
    pi: String,
    dsh: String,
}

impl Offerer {
    fn offers(&self, owner: &yoagent_rutis::ToolOwner, adapters: &Adapters) -> bool {
        match self {
            Offerer::Pi(files) => {
                owner.owner() == PI_ROW
                    && owner.handler() == adapters.pi
                    && owner.source().is_some_and(|source| {
                        let source =
                            std::fs::canonicalize(source).unwrap_or_else(|_| PathBuf::from(source));
                        files.contains(&source)
                    })
            }
            Offerer::Dsh(rows) => {
                (owner.owner() == DSH_ROW && owner.handler() == adapters.dsh)
                    || rows.iter().any(|row| row == owner.owner())
            }
            Offerer::Rutis(row) => owner.owner() == row,
        }
    }
}

/// Who may offer `plugin`'s allowed tools.
fn offerer(plugin: &crate::manifest::Plugin, plan: &plugins::Plan) -> Offerer {
    match &plugin.kind {
        PluginKind::Pi(_) => Offerer::Pi(
            plan.pi_files
                .iter()
                .find(|(name, _)| *name == plugin.name)
                .map(|(_, files)| files.clone())
                .unwrap_or_default(),
        ),
        PluginKind::Dsh(_) => Offerer::Dsh(plugins::dsh_plugin_rows(plan, &plugin.name)),
        PluginKind::Rutis { .. } => Offerer::Rutis(plugins::row_id(&plugin.name)),
    }
}

/// For each tool a plugin allows, who may offer it.
fn offerers(manifest: &Manifest, plan: &plugins::Plan) -> HashMap<String, Vec<Offerer>> {
    let mut map: HashMap<String, Vec<Offerer>> = HashMap::new();
    for plugin in &manifest.plugins {
        let offerer = offerer(plugin, plan);
        for tool in &plugin.allow {
            map.entry(tool.clone()).or_default().push(offerer.clone());
        }
    }
    map
}

/// The gate's check for plugins' allowed tools: the plugin that offered
/// the tool in the run (`offered_by`) must be one that allows it — and the
/// name must not be one of the agent's own tools (yoagent runs its own on a
/// clash while `offered_by` still names the plugin).
fn offered_check(
    registry: Arc<yoagent_rutis::registry::Registry>,
    offerers: HashMap<String, Vec<Offerer>>,
    adapters: Adapters,
    own: HashSet<String>,
) -> OfferedCheck {
    Arc::new(move |run: &str, tool: &str| {
        if own.contains(tool) {
            return false;
        }
        let (Some(owner), Some(allowed)) = (registry.offered_by(run, tool), offerers.get(tool))
        else {
            return false;
        };
        allowed.iter().any(|o| o.offers(&owner, &adapters))
    })
}

/// For `AllowedCheck`: the owner that offered an allowed tool in a run,
/// when no entry that allows the name accepts it (the gate's rule: then it
/// asks) — with its `source` for pi, whose extensions share one row.
fn other_owner(
    manifest: &Manifest,
    plan: &plugins::Plan,
    registry: Arc<yoagent_rutis::registry::Registry>,
    adapters: Adapters,
) -> allowed::OtherOwner {
    let offerers = offerers(manifest, plan);
    Arc::new(move |run: &str, _plugin: &str, tool: &str| {
        let owner = registry.offered_by(run, tool)?;
        let accepted = offerers
            .get(tool)
            .is_some_and(|all| all.iter().any(|o| o.offers(&owner, &adapters)));
        (!accepted).then(|| match owner.source() {
            Some(source) if owner.owner() == PI_ROW => format!("{} ({source})", owner.owner()),
            _ => owner.owner().to_owned(),
        })
    })
}

/// What `run` needs that can be checked before anything starts.
struct Checked {
    key: Option<String>,
    config: ModelConfig,
    /// `--scripted`'s replies.
    provider: Option<MockProvider>,
    budget: Option<Budget>,
    /// The same across the session's runs.
    session_budget: Option<Budget>,
    prompt: Prompt,
    plan: plugins::Plan,
    servers: HashMap<String, Launch>,
    sources: Option<Sources>,
    notes: Vec<String>,
    installed: install::Installed,
}

fn check(options: &Options, cwd: &Path) -> Result<Checked, BoxError> {
    let manifest = &options.manifest;
    if options.once && options.task.as_deref().is_none_or(|t| t.trim().is_empty()) {
        return Err("a once run needs a task".into());
    }
    let mut notes = Vec::new();

    // The model: a missing key fails here.
    let model = &manifest.agent.model;
    let (key, provider) = match &options.scripted {
        Some(path) => (None, Some(script::provider(path, cwd)?)),
        None => match model.api_key() {
            Some(key) => (Some(key), None),
            // A local server: an empty key, so yoagent's fallback key is not sent.
            None if !model.needs_key() => (Some(String::new()), None),
            None => {
                return Err(format!(
                    "{}/{} needs its key in {}",
                    model.provider(),
                    model.id(),
                    model.key_env().join(" or ")
                )
                .into())
            }
        },
    };
    prices::enable_bundled();
    let config = match provider {
        Some(_) => ModelConfig::mock(),
        None => model.config(),
    };
    // The manifest refuses a budget for a model with no price.
    let budget = manifest
        .limits
        .budget_usd
        .and_then(|usd| Budget::for_model(usd, &config));
    // Named apart: a run stopped by one says which ("session budget of $2.00
    // spent").
    let session_budget = manifest
        .limits
        .session_budget_usd
        .and_then(|usd| Budget::for_model(usd, &config))
        .map(|b| b.across_runs().with_name(SESSION_BUDGET));
    let unpriced: Vec<&str> = [
        (manifest.limits.budget_usd.is_some() && budget.is_none()).then_some("budget_usd"),
        (manifest.limits.session_budget_usd.is_some() && session_budget.is_none())
            .then_some("session_budget_usd"),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !unpriced.is_empty() {
        let which = unpriced.join(" and ");
        match provider {
            Some(_) => notes.push(format!("{which} not kept: a scripted model has no price")),
            // The manifest found a price; the run must too, or the limit is
            // not kept unsaid.
            None => {
                let model = format!("{}/{}", model.provider(), model.id());
                return Err(format!("{which}: {model} has no price here").into());
            }
        }
    }
    let prompt = Prompt::read(manifest)?;
    let Prepared {
        installed,
        plan,
        servers,
        sources,
        notes: more,
    } = prepare(
        manifest,
        &options.layout,
        options.home.as_deref(),
        options.locked,
        cwd,
    )?;
    notes.extend(more);
    Ok(Checked {
        key,
        config,
        provider,
        budget,
        session_budget,
        prompt,
        plan,
        servers,
        sources,
        notes,
        installed,
    })
}

/// The services every plugin runtime is given (the loader's catalog).
const SHARED: [&str; 7] = [
    "yoagent",
    services::FRONTEND,
    services::UI,
    services::ASK_OWNER,
    host::WORKSPACE,
    host::SESSION,
    host::APPROVAL,
];

/// The services the adapters' own rows provide for their runtime only:
/// the DSH host (`yoagent-dsh-host`), and the pi adapter's host services
/// (`pi-host-session`, `pi-host-approval`: a call through them names its
/// pi extension, so a plugin of another runtime could otherwise call as
/// one) and its `ui` (`pi-host-ui`).
const LOCAL: [&str; 4] = [
    DSH_HOST_SERVICE,
    plugins::PI_HOST_SESSION,
    plugins::PI_HOST_APPROVAL,
    "pi-host-ui",
];

/// How every stdio MCP server starts, a manifest's and a loaded one: in a
/// process group of its own (yoagent 0.25.16) — a terminal's Ctrl+C or
/// hangup reaches only yobuild, which stops it (`crate::stops`), and its
/// close stops what it started too (an `npx` wrapper's `node`), unless that
/// wrapper exited first and was collected (yoagent then leaves its group
/// alone).
pub(crate) fn stdio_options() -> yoagent::mcp::StdioOptions {
    yoagent::mcp::StdioOptions::new().own_process_group(true)
}

/// The plugins as [`start`] started them.
struct Started {
    bridge: RutisBridge,
    host: PluginHost,
    ask_owner: Option<AskOwner>,
    /// Each runtime and its folder (`--measure`).
    runtimes: Vec<(String, PathBuf)>,
    /// The gate `host_approval` judges with (`None` with `gate = "off"`): the
    /// agent's is a clone, sharing the files the agent made.
    gate: Option<Gate>,
    /// DSH's home with its host services, while the `dsh` runtime uses it
    /// (a private one is removed when dropped).
    dsh_home: Option<DshHome>,
}

/// The plugins started as `run` starts them: the bridge, the frontend's
/// services and `ask_owner`'s on `root`, then the runtimes and rows of
/// `plan`, waited for (the adapters' handlers, each policy plugin's judge,
/// the services DSH plugins inject). `terminal`: the terminal UI's runtime
/// too (its row is loaded later).
#[allow(clippy::too_many_arguments)]
async fn start(
    root: &Ctx,
    manifest: &Manifest,
    plan: &plugins::Plan,
    layout: &Layout,
    session: &Session,
    terminal: bool,
    cwd: &Path,
    scratch: Option<&Scratch>,
    once: bool,
) -> Result<Started, BoxError> {
    let bridge = RutisBridge::install(root)?;
    services::provide(root, session)?;
    // The host services (design §12): granted per plugin, refusing the rest.
    let judge = gate(manifest, session, cwd, scratch);
    host::provide(
        root,
        host::Host::new(
            session.clone(),
            manifest,
            cwd,
            judge.clone(),
            once,
            &plan.pi_names(),
        ),
    )?;
    // `ask_owner`, the tool and (for plugins) the host service: one
    // numbering. Before the plugins start: one may use it as it loads. The
    // service is there either way (rutis has no optional inject: a plugin
    // injecting a missing service would never start), refusing without the
    // built-in.
    let ask_owner = manifest
        .builtins
        .ask_owner
        .then(|| AskOwner::new(session.clone()));
    services::provide_ask_owner(root, ask_owner.clone())?;
    // The runtimes and their plugins.
    let mut builder = PluginHost::builder();
    // Each runtime and its folder: whose memory is said after each run.
    let mut runtimes: Vec<(String, PathBuf)> = Vec::new();
    if terminal {
        // The terminal UI reads the keyboard: its runtime keeps the terminal.
        // Its folder spelled as a rutis package's is (canonical), so one
        // started from the same folder is seen as that (`--measure`).
        let dir = layout.frontend.join("plugins");
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        builder = builder.node("ui", &dir).terminal("ui");
        runtimes.push(("ui".into(), dir));
    }
    if plan.pi {
        let dir = layout.adapters.join("pi");
        builder = builder.node("pi", &dir);
        runtimes.push(("pi".into(), dir));
    }
    // DSH's own host services, on request, and the DSH host's session log:
    // in a home of their own.
    let dsh_home = match plan.dsh && (plan.dsh_services || plan.dsh_host) {
        true => Some(DshHome::new()?),
        false => None,
    };
    if plan.dsh {
        let dir = plan.dsh_dir.clone();
        builder = builder.node("dsh", &dir);
        runtimes.push(("dsh".into(), dir));
        for (name, value) in dsh_home.iter().flat_map(DshHome::env) {
            builder = builder.env("dsh", name, value);
        }
    }
    // Each rutis plugin's package, a runtime of its own.
    for (at, dir) in plan.rutis.iter().enumerate() {
        let name = plugins::rutis_runtime_name(at);
        builder = builder.node(&name, dir);
        runtimes.push((name, dir.clone()));
    }
    for service in SHARED {
        builder = builder.share(service);
    }
    // What the adapters' rows provide for their own runtime: the DSH host
    // and the pi adapter's host services (a plugin of another runtime could
    // otherwise get the stand-in, or call as a pi extension; design §12).
    for service in LOCAL {
        builder = builder.local(service);
    }
    builder = kept_environment(builder, manifest, plan, &runtimes);
    let mut host = builder.start(root).await?;
    if !plan.rows.is_empty() {
        let rows = with_session_log(plan, dsh_home.as_ref())?;
        host.load(rows)
            .await
            .map_err(|e| dsh_waiting(e, plan, manifest.dsh.host_services))?;
        // The adapters' handlers, each from its own row; and a rutis plugin
        // marked `policy` registers its judge in its own time (a row may be
        // Active before its `apply` ends): a run started before would find
        // no policy, or only another plugin's — so wait for one from its
        // own row (a pi policy is the pi row's handler).
        let rows = |handler: &String| match *handler == plan.pi_handler {
            true => PI_ROW,
            false => DSH_ROW,
        };
        let handlers: Vec<(&str, &str)> = plan
            .handlers
            .iter()
            .map(|h| (h.as_str(), rows(h)))
            .collect();
        let judges: Vec<String> = manifest
            .plugins
            .iter()
            .filter(|p| p.policy && matches!(p.kind, PluginKind::Rutis { .. }))
            .map(|p| plugins::row_id(&p.name))
            .collect();
        wait_for(&bridge, &handlers, &judges, &host).await?;
    }
    if !plan.dsh_modules.is_empty() {
        dsh_services(&mut host, plan, manifest.dsh.host_services).await?;
    }
    if plan.dsh_host {
        dsh_host_started(&mut host, plan).await?;
    }
    Ok(Started {
        bridge,
        host,
        ask_owner,
        runtimes,
        gate: judge,
        dsh_home,
    })
}

/// What `run` and `check` both do before anything starts: the pinned
/// packages installed, the plugins planned, each MCP server's launch.
struct Prepared {
    installed: install::Installed,
    plan: plugins::Plan,
    servers: HashMap<String, Launch>,
    /// `find_plugin`'s sources, with `[extend]` (a bad
    /// `YOBUILD_MCP_REGISTRY` refused here, before anything starts).
    sources: Option<Sources>,
    /// Said before the agent starts (a Warning notice each).
    notes: Vec<String>,
}

fn prepare(
    manifest: &Manifest,
    layout: &Layout,
    home: Option<&Path>,
    locked: bool,
    cwd: &Path,
) -> Result<Prepared, BoxError> {
    let lookup = |name: &str| std::env::var(name).ok();
    // The MCP registry only for an agent that finds MCP servers, stdio or
    // remote: a bad `YOBUILD_MCP_REGISTRY` refuses nothing it would not
    // use.
    let sources = match &manifest.extend {
        Some(extend) if extend.asks_registry() => Some(Sources::from_env()?),
        Some(_) => Some(Sources::npm_only()),
        None => None,
    };
    let mut notes = Vec::new();
    // Nothing judging calls is allowed (an agent in a sandbox), but said.
    if manifest.builtins.gate == GateMode::Off && !manifest.plugins.iter().any(|p| p.policy) {
        notes.push(
            "nothing judges tool calls: `gate = \"off\"` and no plugin is `policy = true` \
             (every call runs as the agent asks; fine in a sandbox)"
                .into(),
        );
    }
    // A model yoagent has no preset for: its limits are guesses (a window
    // too small cuts the conversation early; too large, a request fails).
    let model = &manifest.agent.model;
    if !model.limits_known() {
        let config = model.config();
        notes.push(format!(
            "model {}/{}: yoagent has no preset for it, so its context window ({} tokens) and output limit ({}) are the `{}` provider's defaults, not necessarily the model's",
            model.provider(),
            model.id(),
            config.context_window,
            config.max_tokens,
            model.provider()
        ));
    }
    let installed = install::install(manifest, &layout.adapters, home, locked)?;
    let plan = plugins::plan(manifest, layout, &installed, cwd, &lookup)?;
    let withheld = withheld(manifest);
    notes.extend(plan.notes.iter().cloned());
    let servers = manifest
        .mcp
        .iter()
        .map(|server| {
            let launch = launch(server, &installed, &lookup, &withheld)?;
            Ok((server.name.clone(), launch))
        })
        .collect::<Result<_, BoxError>>()?;
    Ok(Prepared {
        installed,
        plan,
        servers,
        sources,
        notes,
    })
}

/// The system prompt: the manifest's file (`{cwd}`, `{scratch}` filled in
/// at the start), or a plain one; the report's instruction after it.
struct Prompt {
    text: String,
    /// The agent's own name, for the plain prompt.
    plain: Option<String>,
}

impl Prompt {
    fn read(manifest: &Manifest) -> Result<Prompt, BoxError> {
        let mut prompt = match &manifest.agent.prompt {
            Some(path) => {
                let text = std::fs::read_to_string(path)
                    .map_err(|e| format!("prompt {}: {e}", path.display()))?;
                if text.contains("{scratch}") && !manifest.builtins.scratch {
                    return Err(format!(
                        "prompt {}: {{scratch}}, but [builtins] scratch is off",
                        path.display()
                    )
                    .into());
                }
                Prompt { text, plain: None }
            }
            None => Prompt {
                text: String::new(),
                plain: Some(manifest.agent.name.clone()),
            },
        };
        if let Some(report) = &manifest.builtins.report {
            prompt.text.push_str("\n\n");
            prompt.text.push_str(&report.instruction());
        }
        Ok(prompt)
    }

    fn fill(&self, cwd: &Path, scratch: Option<&Scratch>) -> String {
        let cwd = cwd.display().to_string();
        match &self.plain {
            Some(name) => {
                let scratch = scratch.map_or_else(String::new, |s| {
                    format!(
                        " Your scratch folder, for what is not a deliverable: {}.",
                        s.path().display()
                    )
                });
                format!(
                    "You are {name}, an agent working in {cwd}.{scratch}{}",
                    self.text
                )
            }
            None => {
                let text = self.text.replace("{cwd}", &cwd);
                match scratch {
                    Some(s) => text.replace("{scratch}", &s.path().display().to_string()),
                    None => text,
                }
            }
        }
    }
}

/// The agent's tools as they are gathered, each with where it came from.
#[derive(Default)]
struct Tools {
    list: Vec<Box<dyn AgentTool>>,
    from: Vec<(String, String)>,
}

impl Tools {
    fn add(&mut self, source: &str, tools: Vec<Box<dyn AgentTool>>) {
        for tool in tools {
            self.from.push((tool.name().to_owned(), source.to_owned()));
            self.list.push(tool);
        }
    }

    /// No two tools of one name, and none named `ask_owner`, `find_plugin`
    /// or `load_plugin` but yobuild's (a server's would be allowed by the
    /// gate when the built-in is on).
    fn check(&self) -> Result<(), BoxError> {
        let mut seen: HashMap<&str, &str> = HashMap::new();
        for (name, source) in &self.from {
            if name == AskOwner::TOOL && source != AskOwner::TOOL {
                return Err(format!(
                    "tool ask_owner from {source}: the name is yobuild's ([builtins] ask_owner)"
                )
                .into());
            }
            if EXTEND_TOOLS.contains(&name.as_str()) && source != name {
                return Err(
                    format!("tool {name} from {source}: the name is yobuild's ([extend])").into(),
                );
            }
            if let Some(first) = seen.insert(name, source) {
                return Err(format!("two tools named {name}: from {first} and {source}").into());
            }
        }
        Ok(())
    }
}

/// `builder` with the kept environment (design §12 "Step 5"): every
/// runtime inherits the kept set (`environment::KEPT`, `KEPT_PREFIXES`)
/// and what [`kept_for`] says for it; the credentials yoagent reads set
/// here are withheld besides (off unix, where nothing else is taken out,
/// they are set empty). `runtimes` are the runtimes `builder` starts.
pub(crate) fn kept_environment(
    mut builder: PluginHostBuilder,
    manifest: &Manifest,
    plan: &plugins::Plan,
    runtimes: &[(String, PathBuf)],
) -> PluginHostBuilder {
    builder = builder.inherit(crate::environment::inherit());
    let withheld = withheld(manifest);
    tracing::debug!(
        "withheld from plugin runtimes: {withheld:?}; granted: {:?}",
        plan.env
    );
    for name in withheld {
        builder = builder.withhold(name);
    }
    for (runtime, _) in runtimes {
        for entry in kept_for(runtime, plan) {
            builder = builder.keep(runtime, entry);
        }
    }
    builder
}

/// What the runtime `runtime` keeps beyond the kept set: its plugins'
/// `env` grant (`Plan::kept`), and for the terminal UI's runtime (`ui`,
/// yobuild's own code) pi-tui's settings (`environment::UI_KEPT`).
pub(crate) fn kept_for(runtime: &str, plan: &plugins::Plan) -> Vec<String> {
    let ui = match runtime {
        "ui" => crate::environment::UI_KEPT,
        _ => &[],
    };
    plan.kept(runtime)
        .iter()
        .cloned()
        .chain(ui.iter().map(|e| (*e).to_owned()))
        .collect()
}

/// yoagent's tools the manifest asks for (`[builtins] tools`): all six —
/// bash and search start in `cwd` (the file tools read relative paths from
/// the process's working folder, which is `cwd`) — or reading only, its
/// paths confined to `cwd`.
fn tools_for(set: ToolSet, cwd: &Path) -> Vec<Box<dyn AgentTool>> {
    let here = cwd.display().to_string();
    match set {
        ToolSet::ReadOnly => vec![
            Box::new(ReadFileTool::default().with_allowed_paths(vec![here.clone()])),
            Box::new(ListFilesTool::default().with_allowed_paths(vec![here.clone()])),
            Box::new(
                SearchTool::default()
                    .with_root(here.clone())
                    .with_allowed_paths(vec![here]),
            ),
        ],
        ToolSet::Coding => vec![
            Box::new(BashTool::default().with_cwd(here.clone())),
            Box::new(ReadFileTool::default()),
            Box::new(WriteFileTool::new()),
            Box::new(EditFileTool::new()),
            Box::new(ListFilesTool::default()),
            Box::new(SearchTool::default().with_root(here)),
        ],
    }
}

/// A run's cost, as `--once` says it when the run ends: `Cost: $0.0021
/// (12345 tokens in, 678 out; 3 turns)`. The dollars are yoagent's
/// `total_cost_usd` (the run, its sub-agents, compaction and decision
/// models); the tokens its `total_usage` (all but decision models'), tokens
/// in counted as facts counts them (input + cache reads + cache writes);
/// turns the agent's own. A run with no turn: `unknown` when it was aborted
/// or failed (its figures are lost with it — after Ctrl+C, say — not zero),
/// else `nothing spent`. Otherwise no dollars is `not priced` (part of it
/// has no price: a scripted model has none), and less than a hundredth of a
/// cent `<$0.0001`.
fn cost(outcome: RunOutcome, stats: &yoagent::SessionStats, total_usd: Option<f64>) -> String {
    if stats.turns == 0 {
        return match outcome {
            RunOutcome::Aborted | RunOutcome::Error => {
                "Cost: unknown (the run's figures were lost with it)".to_owned()
            }
            _ => "Cost: nothing spent".to_owned(),
        };
    }
    let usage = stats.total_usage();
    let tokens_in = usage.input + usage.cache_read + usage.cache_write;
    let price = match total_usd {
        Some(usd) if usd > 0.0 && usd < 0.00005 => "<$0.0001".to_owned(),
        Some(usd) => format!("${usd:.4}"),
        None => "not priced".to_owned(),
    };
    format!(
        "Cost: {price} ({tokens_in} tokens in, {} out; {} turn{})",
        usage.output,
        stats.turns,
        if stats.turns == 1 { "" } else { "s" }
    )
}

/// What the frontends saw, as an observer sees it (a question asked with
/// only observers there gets its default answer at once).
#[derive(Default)]
struct Seen {
    /// The first run's end (`once`: the task's) and the last's.
    first: Option<(RunOutcome, Option<String>)>,
    last: Option<(RunOutcome, Option<String>)>,
    notices: Vec<(NoticeLevel, String)>,
}

type Shared = Arc<std::sync::Mutex<Seen>>;

/// Watches the session: how many runs ended (`ended`; it closes with the
/// session), and what was seen; with `print`, each tool call and notice on
/// stderr. Ends with the session.
fn watch(
    session: &Session,
    print: bool,
) -> (
    Shared,
    tokio::sync::watch::Receiver<u64>,
    tokio::task::JoinHandle<()>,
) {
    let seen: Shared = Arc::default();
    let (ended, ended_rx) = tokio::sync::watch::channel(0u64);
    let mut watching = session.observe();
    let record = seen.clone();
    // A handle to say what to do; the task ends on `Closed`, which needs the
    // session's end, so it never keeps a session alive on its own (`run`
    // holds one until the end anyway).
    let notify = session.clone();
    let task = tokio::spawn(async move {
        // The hints for refused tool calls said this run (each once).
        let mut said: HashSet<&'static str> = HashSet::new();
        loop {
            match watching.messages.recv().await {
                Some(ServerMessage::RunEnd {
                    outcome,
                    error,
                    stats,
                    total_cost_usd,
                    ..
                }) => {
                    said.clear();
                    if print {
                        eprintln!("  {}", cost(outcome, &stats, total_cost_usd));
                    }
                    // A run that did not complete: what to do, for every
                    // frontend (design §11).
                    if let Some(fix) = what_to_do(outcome, error.as_deref()) {
                        notify.notice(NoticeLevel::Info, format!("What to do: {fix}."));
                    }
                    {
                        let mut seen = record.lock().unwrap();
                        seen.first.get_or_insert_with(|| (outcome, error.clone()));
                        seen.last = Some((outcome, error));
                    }
                    ended.send_modify(|n| *n += 1);
                }
                Some(ServerMessage::Event { event, .. }) => {
                    if let AgentEvent::ToolExecutionEnd {
                        tool_name,
                        result,
                        is_error,
                        ..
                    } = *event
                    {
                        let text: Vec<&str> = result
                            .content
                            .iter()
                            .filter_map(|c| match c {
                                Content::Text { text } => Some(text.as_str()),
                                _ => None,
                            })
                            .collect();
                        let text = text.join(" ");
                        // A call refused for a reason yobuild knows (the
                        // policy plugin gone, a handler failed): what to do,
                        // once a run — the run itself may still complete.
                        if is_error {
                            if let Some(h) = crate::hints::hint(crate::hints::Scope::Call, &text) {
                                if said.insert(h.id) {
                                    notify.notice(
                                        NoticeLevel::Info,
                                        format!("What to do: {}.", h.fix),
                                    );
                                }
                            }
                        }
                        if print {
                            let first = text.lines().next().unwrap_or_default();
                            match is_error {
                                true => eprintln!("  {tool_name} ✗ {first}"),
                                false => eprintln!("  {tool_name} ✓"),
                            }
                        }
                    }
                }
                Some(ServerMessage::Notice { level, message }) => {
                    if print {
                        eprintln!("  {level:?}: {message}");
                    }
                    record.lock().unwrap().notices.push((level, message));
                }
                // The sender goes with the task: a waiting `once` wakes.
                Some(ServerMessage::Closed) | None => return,
                Some(_) => {}
            }
        }
    });
    (seen, ended_rx, task)
}

/// How the session ended, once the watch saw it close: its first run's
/// outcome with `once`, else its last's; `Aborted` when Ctrl+C came
/// during a run (the run itself then ends as an error: the driver is gone).
async fn finish(
    seen: Shared,
    watching: tokio::task::JoinHandle<()>,
    agent: Option<Agent>,
    once: bool,
    interrupted: Option<crate::stops::Stop>,
) -> Ended {
    // The session closes once its driver is gone; the watch then ends.
    let unsure = match tokio::time::timeout(Duration::from_secs(5), watching).await {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(format!("the session's watch failed: {e}")),
        Err(_) => Some("the session did not close within 5 s".to_owned()),
    };
    let seen = std::mem::take(&mut *seen.lock().unwrap());
    let ran = match once {
        true => seen.first,
        false => seen.last,
    };
    let (mut outcome, mut error) = match ran {
        Some((outcome, error)) => (Some(outcome), error),
        None => (None, None),
    };
    if let Some(by) = interrupted {
        (outcome, error) = (
            Some(RunOutcome::Aborted),
            Some(format!("stopped {}", by.said())),
        );
    }
    if outcome.is_none() {
        // A `once` run that never ended; or a session whose end was not seen.
        let why = match (once, unsure) {
            (_, Some(why)) => Some(format!("could not tell how the session ended: {why}")),
            (true, None) => Some("the session ended before the run did".to_owned()),
            (false, None) => None,
        };
        if let Some(why) = why {
            tracing::warn!("{why}");
            (outcome, error) = (Some(RunOutcome::Error), Some(why));
        }
    }
    Ended {
        outcome,
        error,
        notices: seen.notices,
        agent,
        loaded: None,
    }
}

/// Whether the services the DSH plugins' rows inject (`plan.dsh_modules`)
/// exist in the DSH runtime, and their modules import there: a plugin
/// missing a service never starts, while its row reads as started (the
/// waiting is inside DSH's own loader) — so ask from inside, with the
/// adapters' service probe. A service still missing after a short look is
/// looked for again, longer, before the run is refused (a provider may
/// start slowly).
async fn dsh_services(
    host: &mut PluginHost,
    plan: &plugins::Plan,
    host_services: bool,
) -> Result<(), BoxError> {
    let mut missing = Vec::new();
    for wait in [1000, 3000] {
        missing = probe_services(host, "dsh", &plan.dsh_dir, &plan.dsh_modules, &[], wait).await?;
        if missing.is_empty() {
            return Ok(());
        }
    }
    Err(dsh_missing(&missing, plan, host_services))
}

/// A plugin or stdio MCP server: what starts a process of its own (a
/// plugin's runtime, a server), and so gets the kept environment and what
/// its entry passes (design §12 "Step 5"). Made by [`starts`]: a `Server`
/// is a stdio one (an HTTP server starts nothing, and gets nothing). A
/// plugin carries what its runtime's launch sets besides (the DSH runtime's
/// home, [`dsh_launch`]), which it gets whatever its grant says.
#[derive(Clone, Copy)]
pub(crate) enum Starts<'a> {
    Plugin(&'a crate::manifest::Plugin, &'static [&'static str]),
    Server(&'a Mcp),
}

impl Starts<'_> {
    pub fn name(&self) -> &str {
        match self {
            Starts::Plugin(p, _) => &p.name,
            Starts::Server(s) => &s.name,
        }
    }

    /// It gets `name` beyond the kept set: its `env` grant passes it, its
    /// runtime's launch sets it, or its server's `env` sets it.
    pub fn gets(&self, name: &str) -> bool {
        match self {
            Starts::Plugin(p, launch) => p.grants.passes(name) || launch.contains(&name),
            Starts::Server(s) => match &s.transport {
                Transport::Stdio { env, .. } => env.contains_key(name),
                Transport::Http { .. } => false,
            },
        }
    }

    /// What it gets beyond the kept set, as its entry names it: its `env`
    /// grant's names and prefixes, or its server's `env` names (`check
    /// --json`'s `environment`).
    pub fn environment(&self) -> Vec<String> {
        match self {
            Starts::Plugin(p, _) => p.grants.env.clone(),
            Starts::Server(s) => match &s.transport {
                Transport::Stdio { env, .. } => env.keys().cloned().collect(),
                Transport::Http { .. } => Vec::new(),
            },
        }
    }
}

/// The manifest's plugins, then its stdio servers (an HTTP server starts
/// nothing).
pub(crate) fn starts(manifest: &Manifest) -> Vec<Starts<'_>> {
    let servers = manifest
        .mcp
        .iter()
        .filter(|s| matches!(s.transport, Transport::Stdio { .. }));
    let dsh = dsh_launch(manifest, DshHome::chosen().is_some());
    manifest
        .plugins
        .iter()
        .map(|p| match p.kind {
            PluginKind::Dsh(_) => Starts::Plugin(p, dsh),
            _ => Starts::Plugin(p, &[]),
        })
        .chain(servers.map(Starts::Server))
        .collect()
}

/// What the DSH runtime's launch sets (`DshHome::env`): a home, given when
/// it has host services or the DSH host (as `start` decides, from the
/// plan's `dsh_services` and `dsh_host`) — `chosen` when the user's own
/// (`YOAGENT_DSH_HOME`).
pub(crate) fn dsh_launch(manifest: &Manifest, chosen: bool) -> &'static [&'static str] {
    let dsh = manifest
        .plugins
        .iter()
        .any(|p| matches!(p.kind, PluginKind::Dsh(_)));
    match dsh && (manifest.dsh.host_services || manifest.dsh_grants().is_some()) {
        true => DshHome::names(chosen),
        false => &[],
    }
}

/// The names of `vars` (this process's environment) that one or more of
/// the plugins and stdio servers do not get (design §12 "Step 5"; `check
/// --json`'s agent `leftOut`), sorted: on unix every name the kept set
/// leaves out and its entry does not get (a name `env -u` cannot take —
/// empty, holding `=`, not UTF-8 — aside: it passes, or refuses a server);
/// off unix (`unix` false) only the credentials yoagent reads, set empty.
/// A variable set empty counts: it is left out all the same (the notes on
/// one entry, by `key_is_set`, say only what holds a value).
pub(crate) fn left_out(
    manifest: &Manifest,
    vars: impl IntoIterator<Item = std::ffi::OsString>,
    unix: bool,
) -> Vec<String> {
    let starting = starts(manifest);
    let credentials = manifest.key_variables();
    let mut out: Vec<String> = vars
        .into_iter()
        .filter_map(|name| name.into_string().ok())
        .filter(|name| !name.is_empty() && !name.contains('='))
        .filter(|name| match unix {
            true => !crate::environment::kept(name.as_ref()),
            false => credentials.contains(name),
        })
        .filter(|name| starting.iter().any(|entry| !entry.gets(name)))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The one note on what plugins and stdio servers do not get (design §12
/// "Step 5", "Said"), when something starts and something is left out:
/// the rule and a count ([`left_out`]; their names are `check --json`'s
/// `leftOut`), the credentials yoagent reads among them named — since one
/// that needs a key fails at its first call, in its own words. `run` says
/// it as an Info notice as the session starts, `check` as an agent note.
pub(crate) fn environment_note(
    manifest: &Manifest,
    vars: impl IntoIterator<Item = std::ffi::OsString>,
    unix: bool,
) -> Option<String> {
    let left = left_out(manifest, vars, unix);
    if left.is_empty() {
        return None;
    }
    let credentials: Vec<String> = manifest
        .key_variables()
        .into_iter()
        .filter(|k| left.contains(k))
        .map(|k| format!("`{k}`"))
        .collect();
    const HANDED: &str = "an entry that needs one gets it only from its entry (an `env` grant, or a `\"${NAME}\"` reference in its `config` or in a server's `env`, arguments, URL or headers), and what it spends with it is not metered";
    if !unix {
        return Some(format!(
            "plugins and stdio MCP servers inherit the whole environment here but the credentials yoagent reads, set empty in their environment (there is no /usr/bin/env here to start them through): {}; {HANDED}",
            credentials.join(", ")
        ));
    }
    let n = left.len();
    let head = format!(
        "plugins and stdio MCP servers start with the kept environment (the path, home, locale, terminal, session, proxies, standard folders, toolchains) and what their entries grant: {n} variable(s) set here are left out of one or more of them (`check --json` lists them: the agent's `leftOut`)"
    );
    Some(match credentials.is_empty() {
        true => format!(
            "{head}; an `env` grant on a plugin, or a stdio server's `env` table, passes one it needs"
        ),
        false => format!(
            "{head}, among them the credentials yoagent reads ({}); {HANDED}",
            credentials.join(", ")
        ),
    })
}

/// `check`'s notes on what `entry`'s own code reads (`read`: the scan,
/// `environment::names_read` — a rutis plugin's own file, not what it
/// imports) but does not get, of the names `set` here (design §12 "Step
/// 5", "Said"): a credential yoagent reads, never with a suggestion to
/// grant it (taken out on unix, set empty elsewhere); on unix (`unix`) a
/// name that loads code, left out on purpose, without one either; any other
/// name the kept set leaves out, with how to hand it over — an `env` grant
/// for a plugin, its `env` table for a server. A hint, not a proof (the
/// scan's limits).
pub(crate) fn reads_notes(
    entry: Starts<'_>,
    read: &std::collections::BTreeSet<String>,
    set: impl Fn(&str) -> bool,
    credentials: &[String],
    unix: bool,
) -> Vec<String> {
    read.iter()
        .filter(|name| set(name) && !entry.gets(name))
        .filter_map(|name| {
            if credentials.contains(name) {
                let lost = match unix {
                    true => "taken out of its environment",
                    false => "set empty in its environment",
                };
                return Some(format!(
                    "its code reads `{name}`, a credential yoagent reads, {lost}: one handed over is spent unmetered"
                ));
            }
            if !unix || crate::environment::kept(name.as_ref()) {
                return None;
            }
            if crate::environment::loads_code(name) {
                return Some(format!(
                    "its code reads `{name}`, which is set here but left out of its environment on purpose: it can load code into its runtime"
                ));
            }
            Some(match entry {
                Starts::Plugin(..) => format!(
                    "its code reads `{name}`, which is set here but left out of its environment: grant it (`grants = {{ env = [\"{name}\"] }}`) if it should have it"
                ),
                Starts::Server(_) => format!(
                    "its code reads `{name}`, which is set here but left out of its environment: hand it over in its `env` table (`{name} = \"${{{name}}}\"`) if it should have it"
                ),
            })
        })
        .collect()
}

/// `check`'s notes on what `plugin`'s `env` grant hands over of the names
/// `set` here (`vars`), beside the credentials yoagent reads (`keys_given`
/// says those): a name that loads code into its runtime, and one that looks
/// like a secret (`manifest::secret`) — the owner's account, used as the
/// plugin sees fit.
pub(crate) fn given_notes(
    plugin: &crate::manifest::Plugin,
    vars: impl IntoIterator<Item = std::ffi::OsString>,
    set: impl Fn(&str) -> bool,
    credentials: &[String],
) -> Vec<String> {
    // A grant names letters, digits and `_`: a name not UTF-8 is none of it.
    let mut names: Vec<String> = vars
        .into_iter()
        .filter_map(|name| name.into_string().ok())
        .filter(|name| plugin.grants.passes(name) && set(name) && !credentials.contains(name))
        .collect();
    names.sort();
    names.dedup();
    names
        .into_iter()
        .filter_map(|name| {
            match (
                crate::environment::loads_code(&name),
                crate::manifest::secret(&name),
            ) {
                (true, _) => Some(format!(
                    "it is given `{name}`, which can load code into its runtime"
                )),
                (false, true) => Some(format!(
                    "it is given `{name}`, set here: the owner's own, used as the plugin sees fit"
                )),
                (false, false) => None,
            }
        })
        .collect()
}

/// `check`'s agent note when a kept proxy variable set here (`vars`, with
/// values) holds a password (`environment::proxy_password`) and something
/// starts: every plugin and stdio server gets it.
pub(crate) fn proxy_note(
    manifest: &Manifest,
    vars: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Option<String> {
    if starts(manifest).is_empty() {
        return None;
    }
    let mut names: Vec<String> = vars
        .into_iter()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value)))
        .filter(|(name, value)| {
            crate::environment::PROXIES.contains(&name.as_str())
                && crate::environment::proxy_password(&value.to_string_lossy())
        })
        .map(|(name, _)| format!("`{name}`"))
        .collect();
    names.sort();
    (!names.is_empty()).then(|| format!(
        "{} hold(s) a password, and every plugin and stdio MCP server gets it as the owner's network setting",
        names.join(", ")
    ))
}

/// What each plugin and stdio MCP server loses of `withheld`, by its name,
/// when they do not all lose the same (an `env` grant or a server's `env`
/// keeps some for one): `check` notes each entry's then, since the one
/// combined note ([`environment_note`]) cannot say who lost what. Alike (or
/// none started): nothing.
pub(crate) fn withheld_from(
    manifest: &Manifest,
    withheld: &[String],
) -> Vec<(String, Vec<String>)> {
    let lost = |kept: &dyn Fn(&String) -> bool| -> Vec<String> {
        withheld
            .iter()
            .filter(|name| !kept(name))
            .cloned()
            .collect()
    };
    let plugins = manifest
        .plugins
        .iter()
        .map(|p| (p.name.clone(), lost(&|name| p.grants.passes(name))));
    let servers = manifest.mcp.iter().filter_map(|s| match &s.transport {
        Transport::Stdio { env, .. } => {
            Some((s.name.clone(), lost(&|name| env.contains_key(name))))
        }
        _ => None,
    });
    let entries: Vec<(String, Vec<String>)> = plugins.chain(servers).collect();
    let alike = entries.windows(2).all(|pair| pair[0].1 == pair[1].1);
    match alike {
        true => Vec::new(),
        false => entries
            .into_iter()
            .filter(|(_, lost)| !lost.is_empty())
            .collect(),
    }
}

/// What happens to a withheld credential: on unix it is taken out
/// (`/usr/bin/env -u`); elsewhere, with no `env` to exec through, it is set
/// empty — which a plugin may read differently from unset (an SDK sends the
/// empty key), so `check`'s note on one entry says which.
#[cfg(unix)]
pub(crate) const TAKEN_ONE: &str = "taken out of its environment";
#[cfg(not(unix))]
pub(crate) const TAKEN_ONE: &str =
    "set empty in its environment (not taken out: there is no `env` here to start it through)";

/// A credential yoagent reads is set here, and not empty: one exported
/// empty (`export OPENAI_API_KEY=`, to turn it off) hands a plugin nothing
/// usable, so the notes do not call it given.
pub(crate) fn key_is_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

/// `check`'s note on a plugin whose `env` grant names some unset or empty
/// here ([`env_unset`]): "it gets nothing from it" when none of its names
/// is set, else only those names said. `run` says it prefixed with the
/// plugin, a Warning when none is set, else Info.
pub(crate) fn unset_grant_note(unset: &UnsetGrant) -> String {
    let keys: Vec<String> = unset.names.iter().map(|k| format!("`{k}`")).collect();
    match unset.all {
        true => format!(
            "its `env` grant names {}, unset or empty here: it gets nothing from it",
            keys.join(", ")
        ),
        false => format!(
            "its `env` grant names {}, unset or empty here: not handed over (the rest of the grant is)",
            keys.join(", ")
        ),
    }
}

/// A plugin's `env` grant naming some variables not set here
/// ([`env_unset`]).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct UnsetGrant {
    pub plugin: String,
    /// The grant's names unset or empty here, in its order.
    pub names: Vec<String>,
    /// None of the grant's names is set, and it holds no prefix: it hands
    /// nothing over.
    pub all: bool,
}

/// The `DYLD_*` variables handed to an entry on purpose — an `env`
/// grant's name set here (`vars`: this process's names), a stdio server's
/// `env` name — said when there are any and `macos`: every plugin runtime
/// and stdio server starts through `/usr/bin/env` (design §12 "Step 5"),
/// which is protected while System Integrity Protection is on (the
/// default; not looked up: the note says so), and the loader drops them as
/// it starts it. Every other `DYLD_*` the kept environment leaves out
/// anyway. A native module that needs one works only with SIP off; this
/// says why.
pub(crate) fn dyld_note(
    manifest: &Manifest,
    vars: impl IntoIterator<Item = std::ffi::OsString>,
    macos: bool,
) -> Option<String> {
    if !macos {
        return None;
    }
    let set: Vec<String> = vars
        .into_iter()
        .map(|name| name.to_string_lossy().into_owned())
        .collect();
    let granted = manifest
        .plugins
        .iter()
        .flat_map(|p| p.grants.env.iter().filter(|e| set.contains(e)).cloned());
    let servers = manifest.mcp.iter().flat_map(|s| match &s.transport {
        Transport::Stdio { env, .. } => env.keys().cloned().collect(),
        _ => Vec::new(),
    });
    let mut dropped: Vec<String> = granted
        .chain(servers)
        .filter(|name| name.starts_with(crate::environment::CODE_LOADING_PREFIX))
        .map(|name| format!("`{name}`"))
        .collect();
    dropped.sort();
    dropped.dedup();
    (!dropped.is_empty()).then(|| format!(
        "{}, handed to a plugin or stdio MCP server, does not reach it while System Integrity Protection is on (macOS's default): every plugin runtime and stdio server starts through the protected /usr/bin/env, which drops `DYLD_*` variables",
        dropped.join(", ")
    ))
}

/// Where the DSH host writes its session log, when that is the user's own
/// DSH home (`YOAGENT_DSH_HOME`, `home`): said by `check` and by `run`.
pub(crate) fn dsh_home_note(
    plan: &plugins::Plan,
    home: Option<std::ffi::OsString>,
) -> Option<String> {
    let home = home.filter(|v| !v.to_string_lossy().trim().is_empty())?;
    plan.dsh_host.then(|| {
        format!(
            "the DSH host writes its session log in YOAGENT_DSH_HOME ({}), a DSH home of your own",
            Path::new(&home).join("sessions").display()
        )
    })
}

/// `plan`'s rows, the DSH host's session log given its folder in the
/// run's DSH home (`<home>/sessions`): refused when that row is not there
/// once, so the log is never written where its empty folder would point.
fn with_session_log(plan: &plugins::Plan, home: Option<&DshHome>) -> Result<Vec<Row>, BoxError> {
    let Some(id) = plan.dsh_session_log.as_deref() else {
        return Ok(plan.rows.clone());
    };
    let Some(home) = home else {
        return Err("the DSH host's session log has no DSH home".into());
    };
    let sessions = home.path().join("sessions");
    let mut given = 0;
    let rows = plan
        .rows
        .iter()
        .map(|row| match row.id() == id && row.runtime_is("dsh") {
            true => {
                given += 1;
                row.clone().config(serde_json::json!({ "root": sessions }))
            }
            false => row.clone(),
        })
        .collect();
    match given {
        1 => Ok(rows),
        n => Err(
            format!("the DSH host's session log row `{id}` is planned {n} times, not once").into(),
        ),
    }
}

/// Whether the DSH host (`dsh-host.ts`) started: it provides
/// `yoagent-dsh-host` once every service it injects exists. Without it the
/// DSH adapter finds no host and runs every call agentless — the plugins'
/// grants not honoured, unsaid — so a host that does not start refuses the
/// run, naming what it waits for. Looked at as `dsh_services` does.
async fn dsh_host_started(host: &mut PluginHost, plan: &plugins::Plan) -> Result<(), BoxError> {
    let module = plan.dsh_dir.join("dsh-host.ts").display().to_string();
    let mut missing = Vec::new();
    for wait in [1000, 3000] {
        missing = probe_services(
            host,
            "dsh",
            &plan.dsh_dir,
            std::slice::from_ref(&module),
            &[DSH_HOST_SERVICE],
            wait,
        )
        .await?;
        if dsh_host_refusal(&missing, &plan.dsh_dir).is_none() {
            return Ok(());
        }
    }
    Err(dsh_host_refusal(&missing, &plan.dsh_dir)
        .unwrap_or_default()
        .into())
}

/// The DSH host's provider, `dsh-host.ts`'s `yoagent-dsh-host`.
const DSH_HOST_SERVICE: &str = "yoagent-dsh-host";

/// Why the DSH host did not start, from the services the probe found
/// `missing` (with `dsh-host.ts`'s own and its provider): `None` when its
/// provider is there.
pub(crate) fn dsh_host_refusal(missing: &[String], dir: &Path) -> Option<String> {
    if !missing.iter().any(|m| m == DSH_HOST_SERVICE) {
        return None;
    }
    let waits: Vec<&str> = missing
        .iter()
        .map(String::as_str)
        .filter(|m| *m != DSH_HOST_SERVICE)
        .collect();
    let why = match waits.is_empty() {
        true => "it provides no host, though every service it injects exists (its log says why)"
            .to_owned(),
        false => format!("it waits for {}", waits.join(", ")),
    };
    Some(format!(
        "the DSH host did not start: {why} (the DSH runtime's folder: {})",
        dir.display()
    ))
}

/// A `load` that failed because DSH plugins wait for services nobody
/// provides (rutis says which, from what each row injects): said as the
/// DSH check says it, with its hints. Any other failure as it is.
fn dsh_waiting(error: BoxError, plan: &plugins::Plan, host_services: bool) -> BoxError {
    use yoagent_rutis::rows::{RowState, RowsNotRunning};
    let Some(not_running) = error.downcast_ref::<RowsNotRunning>() else {
        return error;
    };
    let dsh_rows: Vec<&str> = plan
        .rows
        .iter()
        .filter(|row| row.runtime_is("dsh") && row.id().starts_with("plugin:"))
        .map(|row| row.id())
        .collect();
    let mut missing: Vec<String> = Vec::new();
    for row in &not_running.rows {
        // Not with a note: the list may be incomplete, which the error as
        // it is says.
        match (&row.state, &row.missing) {
            (RowState::Waiting, Some(names))
                if !names.is_empty()
                    && row.note.is_none()
                    && dsh_rows.contains(&row.id.as_str()) =>
            {
                missing.extend(names.iter().cloned());
            }
            _ => return error,
        }
    }
    missing.sort();
    missing.dedup();
    dsh_missing(&missing, plan, host_services)
}

/// "DSH plugins wait for services nobody provides: …", with what may help.
fn dsh_missing(missing: &[String], plan: &plugins::Plan, host_services: bool) -> BoxError {
    let mut hints = Vec::new();
    if !host_services {
        hints.push("DSH's own host services come with [dsh] host_services = true".to_owned());
    }
    if !plan.dsh_peers.is_empty() {
        hints.push(format!(
            "peers the plugins list, not loaded as plugins: {}",
            plan.dsh_peers.join(", ")
        ));
    }
    let hints = match hints.is_empty() {
        true => String::new(),
        false => format!(" ({})", hints.join("; ")),
    };
    format!(
        "DSH plugins wait for services nobody provides: {}{hints}",
        missing.join(", ")
    )
    .into()
}

/// One look by `adapters/dsh/probe-services.ts`, in the DSH runtime
/// `runtime` (`dsh`, or one `load_plugin` added), after `wait_ms`: the
/// services missing; an error for modules that cannot be imported, an
/// answer not in its shape, or a probe that fails or does not answer. The
/// probe is unloaded however it went.
pub(crate) async fn probe_services(
    host: &mut PluginHost,
    runtime: &str,
    base: &Path,
    modules: &[String],
    names: &[&str],
    wait_ms: u64,
) -> Result<Vec<String>, BoxError> {
    let out = tempfile::NamedTempFile::new()?.into_temp_path();
    std::fs::remove_file(&out)?;
    let id = match runtime {
        "dsh" => "yobuild:dsh-services".to_owned(),
        added => format!("yobuild:dsh-services:{added}"),
    };
    let id = id.as_str();
    // What had stopped already is another's: only what stops meanwhile
    // (the probe, or the runtime under it) fails it. An added runtime's
    // rows are said apart.
    let stopped = |host: &PluginHost| {
        let mut all = host.stopped(&[]);
        all.extend(host.added_stopped(runtime));
        all
    };
    let before = stopped(host);
    host.load([Row::new(id, base.join("probe-services.ts").display())
        .runtime(runtime)
        .config(serde_json::json!({
            "modules": modules,
            "names": names,
            "base": base,
            "out": &*out,
            "waitMs": wait_ms,
        }))])
    .await?;
    let deadline = Instant::now() + Duration::from_millis(wait_ms) + Duration::from_secs(15);
    let answer = loop {
        // `writeFileSync` creates the file before it writes: read again
        // until it holds whole JSON (the deadline says a broken one).
        if let Ok(text) = std::fs::read_to_string(&out) {
            if serde_json::from_str::<serde_json::Value>(&text).is_ok() {
                break Ok(text);
            }
        }
        let failures: Vec<String> = stopped(host)
            .into_iter()
            .filter(|f| !before.contains(f))
            .collect();
        if !failures.is_empty() {
            break Err(format!(
                "the DSH service check failed: {}",
                failures.join("; ")
            ));
        }
        if Instant::now() >= deadline {
            break Err(format!(
                "the DSH service check gave no answer: {:?}",
                host.status()
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    // A probe that cannot be unloaded is said, not taken for its answer.
    if let Err(e) = host.unload(id).await {
        tracing::warn!("unloading the DSH service check: {e}");
    }
    let text = answer?;
    let broken = || format!("the DSH service check answered {text:?}");
    let found: serde_json::Value = serde_json::from_str(&text).map_err(|_| broken())?;
    let errors = found["importErrors"].as_object().ok_or_else(broken)?;
    if !errors.is_empty() {
        return Err(format!(
            "DSH plugins that cannot be imported: {}",
            serde_json::Value::Object(errors.clone())
        )
        .into());
    }
    let missing = found["missing"].as_array().ok_or_else(broken)?;
    Ok(missing
        .iter()
        .map(|m| m.as_str().map(str::to_owned).ok_or_else(broken))
        .collect::<Result<_, _>>()?)
}

/// Waits until each of `handlers` — (name, row) — is registered by that
/// row, and each row of `judges` has registered a handler judging calls
/// (`before_tool`; any one of its handlers will do); a plugin that failed
/// meanwhile fails it at once.
async fn wait_for(
    bridge: &RutisBridge,
    handlers: &[(&str, &str)],
    judges: &[String],
    host: &PluginHost,
) -> Result<(), BoxError> {
    let registry = bridge.registry().clone();
    let missing = || -> Vec<String> {
        let have = registry.handlers();
        let mut missing: Vec<String> = handlers
            .iter()
            .filter(|(name, row)| !have.iter().any(|h| h.name() == *name && h.owner() == *row))
            .map(|(name, row)| format!("{name} (from row {row})"))
            .collect();
        for row in judges {
            let judging = have
                .iter()
                .any(|h| h.owner() == row && h.hooks().contains(&"before_tool"));
            if !judging {
                missing.push(format!(
                    "a call judge (before_tool) from the policy plugin's row {row} (does it inject `yoagent`?)"
                ));
            }
        }
        missing
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let failures = host.stopped(&[]);
        if !failures.is_empty() {
            return Err(failures.join("; ").into());
        }
        let missing = missing();
        if missing.is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "plugins did not register {missing:?} in 60 s: {:?}",
                host.status()
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn limits(manifest: &Manifest) -> ExecutionLimits {
    let mut limits = ExecutionLimits::default();
    let l = &manifest.limits;
    if let Some(turns) = l.max_turns {
        limits = limits.with_max_turns(turns);
    }
    if let Some(tokens) = l.max_tokens {
        limits = limits.with_max_total_tokens(usize::try_from(tokens).unwrap_or(usize::MAX));
    }
    if let Some(minutes) = l.max_minutes {
        limits = limits.with_max_duration(Duration::from_secs(minutes.saturating_mul(60)));
    }
    limits
}

/// How to reach an MCP server, its `${NAME}`s filled in.
enum Launch {
    Stdio {
        command: PathBuf,
        args: Vec<String>,
        env: HashMap<String, String>,
        /// The credentials set here ([`withheld`]); [`without`] leaves
        /// out those its `env` sets.
        withheld: Vec<String>,
    },
    Http {
        url: String,
        headers: Vec<(String, String)>,
    },
}

/// How to start `server`; a stdio server has each of `withheld` (the
/// credentials set here) taken out of its environment, unless its own `env`
/// sets it — and on unix every name not in the kept set ([`without`]).
fn launch(
    server: &Mcp,
    installed: &install::Installed,
    lookup: &dyn Fn(&str) -> Option<String>,
    withheld: &[String],
) -> Result<Launch, BoxError> {
    let failed =
        |e: &dyn std::fmt::Display| -> BoxError { format!("mcp {}: {e}", server.name).into() };
    // Its name prefixes its tools' (`<name>__<tool>`), which models take
    // only of letters, digits, `_` and `-`.
    if !tool_name(&format!("{}__x", server.name)) {
        return Err(failed(&format!(
            "its name prefixes its tools ({}__<tool>), and tool names take only letters, digits, _ and -: give the server a `name`",
            server.name
        )));
    }
    Ok(match &server.transport {
        Transport::Stdio {
            source,
            bin,
            args,
            env,
        } => {
            let command = match source {
                Source::Path(path) => path.clone(),
                // Refused when read: a server is a package or a path.
                Source::Builtin(name) => {
                    return Err(failed(&format!("`yobuild:{name}` is not an MCP server")))
                }
                Source::Package(package) => {
                    let dir = plugins::in_set(installed, install::Set::Mcp, package)
                        .map_err(|e| failed(&e))?;
                    bin_of(&dir, package.name(), bin.as_deref()).map_err(|e| failed(&e))?
                }
            };
            Launch::Stdio {
                command,
                args: args
                    .iter()
                    .map(|a| a.resolve_with(lookup))
                    .collect::<Result<_, _>>()
                    .map_err(|e| failed(&e))?,
                env: env
                    .iter()
                    .map(|(k, v)| Ok((k.clone(), v.resolve_with(lookup)?)))
                    .collect::<Result<_, crate::manifest::Unset>>()
                    .map_err(|e| failed(&e))?,
                withheld: withheld.to_vec(),
            }
        }
        Transport::Http { url, headers } => Launch::Http {
            url: url.resolve_with(lookup).map_err(|e| failed(&e))?,
            headers: headers
                .iter()
                .map(|(name, value)| Ok((name.clone(), value.resolve_with(lookup)?)))
                .collect::<Result<_, crate::manifest::Unset>>()
                .map_err(|e| failed(&e))?,
        },
    })
}

/// The credentials yoagent reads that are set in this process (design §12,
/// "Step 4"), which plugin runtimes and stdio MCP servers have taken out of
/// their environment but where their entry hands one over: a plugin could
/// otherwise call a model past the budget, unmetered. (The adapters' helper
/// scripts have every one removed: `plugins::node_json`.)
pub(crate) fn withheld(manifest: &Manifest) -> Vec<String> {
    manifest
        .key_variables()
        .into_iter()
        .filter(|name| std::env::var_os(name).is_some())
        .collect()
}

/// The credentials the manifest hands an entry on purpose, by its name: a
/// plugin's `env` grant or one referenced in its `config`, or in an MCP
/// server's `env` (or an `env` name whose value is a reference:
/// `OPENAI_API_KEY = "${MINE}"` hands a key over under that name), `args`,
/// URL or headers. `check` notes each (what it spends with it is not
/// metered). A name counts only when `set` here ([`key_is_set`]: set and
/// not empty) — an `env` grant's, a reference (one to an unset name is
/// refused anyway; one to an empty name is filled empty, which hands
/// nothing over), an `env` name only when a reference filling it is set:
/// [`env_unset`] says a grant's others.
pub(crate) fn keys_given(
    manifest: &Manifest,
    set: impl Fn(&str) -> bool,
) -> Vec<(String, Vec<String>)> {
    let keys = manifest.key_variables();
    let given = |names: Vec<&str>| -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for name in names {
            if keys.iter().any(|k| k == name) && !out.iter().any(|o| o == name) {
                out.push(name.to_owned());
            }
        }
        out
    };
    let plugins = manifest.plugins.iter().map(|plugin| {
        let names = plugin.config_references();
        let granted = plugin.grants.env.iter().filter(|k| set(k));
        let names = granted.chain(names.iter().filter(|k| set(k)));
        (
            plugin.name.clone(),
            given(names.map(String::as_str).collect()),
        )
    });
    let servers = manifest.mcp.iter().map(|server| {
        let names: Vec<&str> = match &server.transport {
            // A key's name filled from somewhere (a secret's name holds
            // only references: `guard`).
            // (filled from a set reference), and the references set here.
            Transport::Stdio { args, env, .. } => env
                .iter()
                .filter(|(_, value)| value.references().into_iter().any(&set))
                .map(|(name, _)| name.as_str())
                .chain(
                    args.iter()
                        .chain(env.values())
                        .flat_map(|v| v.references())
                        .filter(|r| set(r)),
                )
                .collect(),
            Transport::Http { url, headers } => std::iter::once(url)
                .chain(headers.values())
                .flat_map(|v| v.references())
                .filter(|r| set(r))
                .collect(),
        };
        (server.name.clone(), given(names))
    });
    plugins
        .chain(servers)
        .filter(|(_, keys)| !keys.is_empty())
        .collect()
}

/// The names of each plugin's `env` grant that are not `set` here: it
/// gets nothing under them, which `check` and `run` say (else "it is
/// given" would read as if it got them). A prefix is not said (what it
/// passes is whatever is set).
pub(crate) fn env_unset(manifest: &Manifest, set: impl Fn(&str) -> bool) -> Vec<UnsetGrant> {
    manifest
        .plugins
        .iter()
        .filter_map(|plugin| {
            let names: Vec<String> = plugin
                .grants
                .env
                .iter()
                .filter(|k| !k.ends_with('*') && !set(k))
                .cloned()
                .collect();
            (!names.is_empty()).then(|| UnsetGrant {
                plugin: plugin.name.clone(),
                all: names.len() == plugin.grants.env.len(),
                names,
            })
        })
        .collect()
}

/// A stdio server's program, arguments and added variables (those its
/// `env` sets; off unix, the withheld ones set empty too).
type StdioCommand = (String, Vec<String>, HashMap<String, String>);

/// A stdio server's command with the kept environment (design §12 "Step
/// 5"): `withheld` and every name of this process's environment not in
/// `environment::KEPT` taken out (`frontend::host::env_without`:
/// `/usr/bin/env -u NAME … <command> <args>`; yoagent's transport can only
/// add variables), but the names its own `env` sets: `env -u` removes a
/// name whoever set it. Its program is looked for first
/// (`frontend::host::startable`).
#[cfg(unix)]
fn without(
    command: &Path,
    args: &[String],
    env: &HashMap<String, String>,
    withheld: &[String],
) -> Result<StdioCommand, BoxError> {
    let vars = std::env::vars_os().map(|(k, _)| k);
    without_from(command, args, env, withheld, vars)
}

/// [`without`] with `vars` as this process's environment's names. yoagent's
/// transport takes the command as text, so a name to take out that is not
/// UTF-8 (which a lossy `-u` would miss, letting it through) refuses the
/// start, naming it.
#[cfg(unix)]
fn without_from(
    command: &Path,
    args: &[String],
    env: &HashMap<String, String>,
    withheld: &[String],
    vars: impl IntoIterator<Item = std::ffi::OsString>,
) -> Result<StdioCommand, BoxError> {
    let set_here = |name: &std::ffi::OsStr| name.to_str().is_some_and(|n| env.contains_key(n));
    let names: Vec<std::ffi::OsString> = withheld
        .iter()
        .filter(|name| !env.contains_key(*name))
        .map(Into::into)
        .chain(crate::frontend::host::not_inherited(
            vars,
            |name| crate::environment::kept(name) || set_here(name),
            // yoagent's transport starts it, not rutis: no fresh channel
            // token is set on it, so one here is this process's own.
            false,
        ))
        .collect();
    if let Some(name) = names.iter().find(|n| n.to_str().is_none()) {
        return Err(format!(
            "the environment variable {name:?} has a name that is not UTF-8: yobuild cannot take it out of a stdio MCP server's environment (the server's command is text): unset it"
        )
        .into());
    }
    // yoagent's transport starts it in this process's folder, with the
    // `PATH` its `env` sets, if any.
    let path = env.get("PATH").map(std::ffi::OsStr::new);
    crate::frontend::host::startable(command.as_os_str(), None, path)?;
    let (program, argv) = crate::frontend::host::env_without(
        &names,
        command.as_os_str().to_owned(),
        args.iter().map(Into::into).collect(),
        None,
        path,
    )?;
    // Every word is UTF-8 (the command a path the manifest or the
    // installed package names, the names checked above), so none is lost
    // here.
    let text = |word: std::ffi::OsString| word.to_string_lossy().into_owned();
    Ok((
        text(program),
        argv.into_iter().map(text).collect(),
        env.clone(),
    ))
}

/// No `env` to exec through: the variables are set empty instead (but the
/// names its own `env` sets).
#[cfg(not(unix))]
fn without(
    command: &Path,
    args: &[String],
    env: &HashMap<String, String>,
    withheld: &[String],
) -> Result<StdioCommand, BoxError> {
    let mut env = env.clone();
    for name in withheld {
        env.entry(name.clone()).or_default();
    }
    Ok((command.display().to_string(), args.to_vec(), env))
}

/// An MCP server's tools, each `<server>__<tool>` (the server is called with
/// its own names); those its `allow` names must be among them.
async fn mcp(server: &Mcp, launch: &Launch) -> Result<Vec<Box<dyn AgentTool>>, BoxError> {
    let failed = |e: &dyn std::fmt::Display| format!("mcp {}: {e}", server.name);
    let client = match launch {
        Launch::Stdio {
            command,
            args,
            env,
            withheld,
        } => {
            let (command, args, env) =
                without(command, args, env, withheld).map_err(|e| failed(&e))?;
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            McpClient::connect_stdio_with(&command, &args, Some(env), stdio_options())
                .await
                .map_err(|e| failed(&e))?
        }
        Launch::Http { url, headers } => {
            let mut transport = HttpTransport::new(url).map_err(|e| failed(&e))?;
            for (name, value) in headers {
                transport = transport
                    .with_header(name, value.clone())
                    .map_err(|e| failed(&e))?;
            }
            McpClient::connect_http_with(transport)
                .await
                .map_err(|e| failed(&e))?
        }
    };
    let client = Arc::new(tokio::sync::Mutex::new(client));
    // `<server>__<tool>`: two servers' tools of one name, or a server's
    // named like a built-in, do not meet.
    let tools = McpToolAdapter::from_client_with_prefix(client, server.name.clone())
        .await
        .map_err(|e| failed(&e))?;
    if tools.is_empty() {
        return Err(failed(&"the server lists no tools").into());
    }
    if let Some(bad) = tools.iter().find(|t| !tool_name(t.name())) {
        let own = &bad.name()[server.name.len() + 2..];
        let why = match tool_name(own) {
            // Too long only with the prefix.
            true => format!(
                "with the prefix {}__ its tool {own:?} is {} characters, more than 64: give the server a shorter `name`",
                server.name,
                bad.name().len()
            ),
            false => format!(
                "its tool {own:?} cannot be offered to a model (names take up to 64 letters, digits, _ and -)"
            ),
        };
        return Err(failed(&why).into());
    }
    // What it allows must be among what it offers (a typo, or a server
    // whose tools changed, is refused here, not left asking).
    let offered: Vec<&str> = tools
        .iter()
        .map(|t| &t.name()[server.name.len() + 2..])
        .collect();
    if let Some(gone) = server.allow.iter().find(|a| !offered.contains(&a.as_str())) {
        return Err(failed(&format!(
            "its `allow` names {gone:?}, which it does not offer (it offers: {})",
            offered.join(", ")
        ))
        .into());
    }
    Ok(tools
        .into_iter()
        .map(|t| Box::new(t) as Box<dyn AgentTool>)
        .collect())
}

/// A tool name providers take: 1 to 64 letters, digits, `_` and `-`.
fn tool_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The program an installed package runs: its `bin` — the one named `bin`
/// when it has several (a single `bin` is named after the package).
fn bin_of(dir: &Path, package: &str, name: Option<&str>) -> Result<PathBuf, String> {
    let path = dir.join("package.json");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let manifest: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let unscoped = package.rsplit('/').next().unwrap_or(package);
    let file = match (&manifest["bin"], name) {
        (serde_json::Value::String(_), Some(name)) if name != unscoped => {
            return Err(format!("{package} has one bin, {unscoped}, not {name}"))
        }
        (serde_json::Value::String(file), _) => file.as_str(),
        (serde_json::Value::Object(bins), Some(name)) => bins
            .get(name)
            .ok_or_else(|| format!("{package} has no bin {name:?}"))?
            .as_str()
            .ok_or_else(|| format!("{package}: bin {name:?} is not a path"))?,
        (serde_json::Value::Object(bins), None) if bins.len() == 1 => bins
            .values()
            .next()
            .and_then(|f| f.as_str())
            .ok_or_else(|| format!("{package}: its bin is not a path"))?,
        (serde_json::Value::Object(bins), None) if bins.len() > 1 => {
            let names: Vec<&String> = bins.keys().collect();
            return Err(format!(
                "{package} has several bins ({names:?}): name one with bin"
            ));
        }
        _ => return Err(format!("{package} has no bin")),
    };
    Ok(dir.join(file))
}

/// This machine's address on the local network (no packet is sent).
fn lan_ip() -> Option<std::net::IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    socket.local_addr().ok().map(|a| a.ip())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_runs_cost_counts_cache_as_tokens_in_and_tells_lost_from_unpriced() {
        use super::{cost, RunOutcome};
        let mut stats = yoagent::SessionStats::default();
        // No turn: nothing spent, unless the run's figures went with it.
        assert_eq!(
            cost(RunOutcome::Completed, &stats, None),
            "Cost: nothing spent"
        );
        assert_eq!(
            cost(RunOutcome::Rejected, &stats, None),
            "Cost: nothing spent"
        );
        for lost in [RunOutcome::Aborted, RunOutcome::Error] {
            assert_eq!(
                cost(lost, &stats, None),
                "Cost: unknown (the run's figures were lost with it)"
            );
        }
        stats.usage.input = 1000;
        stats.usage.cache_read = 200;
        stats.usage.cache_write = 34;
        stats.usage.output = 56;
        stats.turns = 3;
        let done = RunOutcome::Completed;
        assert_eq!(
            cost(done, &stats, Some(0.00213)),
            "Cost: $0.0021 (1234 tokens in, 56 out; 3 turns)"
        );
        assert_eq!(
            cost(done, &stats, Some(0.00215)),
            "Cost: $0.0022 (1234 tokens in, 56 out; 3 turns)"
        );
        assert_eq!(
            cost(done, &stats, Some(0.00001)),
            "Cost: <$0.0001 (1234 tokens in, 56 out; 3 turns)"
        );
        // Sub-agents' and compaction's tokens count, as their dollars do.
        stats.sub_agents.usage.input = 100;
        stats.compaction.usage.output = 4;
        stats.turns = 1;
        assert_eq!(
            cost(RunOutcome::Stopped, &stats, None),
            "Cost: not priced (1334 tokens in, 60 out; 1 turn)"
        );
    }

    #[test]
    fn what_to_do_is_said_for_stopped_failed_and_rejected_runs_only() {
        let turns = "[Agent stopped: Max turns reached (50/50)]";
        assert!(what_to_do(RunOutcome::Stopped, Some(turns))
            .unwrap()
            .contains("max_turns"));
        assert!(what_to_do(RunOutcome::Stopped, None)
            .unwrap()
            .starts_with("a plugin stopped the run"));
        assert!(what_to_do(
            RunOutcome::Rejected,
            Some("input rejected: the plugin host is not running (shut down)")
        )
        .is_some());
        assert_eq!(what_to_do(RunOutcome::Error, None), None);
        assert_eq!(what_to_do(RunOutcome::Completed, Some(turns)), None);
        assert_eq!(what_to_do(RunOutcome::Aborted, Some(turns)), None);
        assert!(what_to_do_at_start("the terminal UI stopped: …").is_some());
        assert_eq!(what_to_do_at_start("Auth error: x"), None);
    }
    use super::*;

    #[test]
    fn the_dsh_host_is_refused_only_without_its_provider_saying_what_it_waits_for() {
        let dir = Path::new("/h/dsh");
        let missing =
            |names: &[&str]| -> Vec<String> { names.iter().map(|n| n.to_string()).collect() };
        assert_eq!(dsh_host_refusal(&missing(&["other"]), dir), None);
        assert_eq!(
            dsh_host_refusal(&missing(&["yoagent-dsh-host", "sessionPersistence"]), dir).unwrap(),
            "the DSH host did not start: it waits for sessionPersistence (the DSH runtime's folder: /h/dsh)"
        );
        assert!(dsh_host_refusal(&missing(&["yoagent-dsh-host"]), dir)
            .unwrap()
            .contains("it provides no host, though every service it injects exists"));
    }

    #[test]
    fn the_session_log_gets_its_folder_in_the_dsh_home_or_the_run_is_refused() {
        let plan = |rows: Vec<Row>, log: Option<&str>| plugins::Plan {
            rows,
            dsh_host: log.is_some(),
            dsh_session_log: log.map(str::to_owned),
            ..plugins::Plan::default()
        };
        let row = |id: &str| Row::new(id, "x").runtime("dsh");
        let home = DshHome::new().unwrap();
        let rows =
            with_session_log(&plan(vec![row("a"), row("log")], Some("log")), Some(&home)).unwrap();
        assert_eq!(rows[0].config_json(), &serde_json::json!({}));
        assert_eq!(
            rows[1].config_json(),
            &serde_json::json!({ "root": home.path().join("sessions") })
        );
        // Not there, there twice, no home: refused, never an empty folder.
        let e = with_session_log(&plan(vec![row("a")], Some("log")), Some(&home)).unwrap_err();
        assert!(e.to_string().contains("planned 0 times"), "{e}");
        let e = with_session_log(
            &plan(vec![row("log"), row("log")], Some("log")),
            Some(&home),
        )
        .unwrap_err();
        assert!(e.to_string().contains("planned 2 times"), "{e}");
        assert!(with_session_log(&plan(vec![row("log")], Some("log")), None).is_err());
        // No host: the rows as planned.
        let rows = with_session_log(&plan(vec![row("log")], None), None).unwrap();
        assert_eq!(rows[0].config_json(), &serde_json::json!({}));
    }

    #[test]
    fn the_dsh_home_is_said_only_with_the_dsh_host_and_a_home_of_ones_own() {
        let host = plugins::Plan {
            dsh_host: true,
            ..plugins::Plan::default()
        };
        let note = dsh_home_note(&host, Some("/me/.dsh".into())).unwrap();
        assert!(note.contains("/me/.dsh/sessions"), "{note}");
        assert_eq!(dsh_home_note(&host, None), None);
        assert_eq!(dsh_home_note(&host, Some(" ".into())), None);
        assert_eq!(
            dsh_home_note(&plugins::Plan::default(), Some("/me/.dsh".into())),
            None
        );
    }

    #[test]
    fn read_only_is_reading_listing_and_searching_only() {
        let names = |set| -> Vec<String> {
            tools_for(set, Path::new("/p"))
                .iter()
                .map(|t| t.name().to_owned())
                .collect()
        };
        assert_eq!(
            names(ToolSet::ReadOnly),
            ["read_file", "list_files", "search"]
        );
        assert!(names(ToolSet::Coding).contains(&"bash".to_owned()));
    }

    fn manifest(dir: &Path, text: &str) -> Manifest {
        Manifest::parse(
            &format!("[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n{text}"),
            &dir.join("a.toml"),
            dir,
        )
        .unwrap()
    }

    #[test]
    fn the_prompt_is_filled_and_scratch_must_be_on_to_be_named() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("p.md"), "in {cwd}, scratch {scratch}").unwrap();
        let with = manifest(
            dir.path(),
            "prompt = \"./p.md\"\n[builtins]\nscratch = true\nreport = [\"Done\"]\n",
        );
        let scratch = Scratch::at(dir.path().join("s")).unwrap();
        let text = Prompt::read(&with)
            .unwrap()
            .fill(Path::new("/w"), Some(&scratch));
        assert!(
            text.starts_with(&format!("in /w, scratch {}", scratch.path().display())),
            "{text}"
        );
        assert!(text.contains("## Done"), "the report's instruction: {text}");
        let without = manifest(dir.path(), "prompt = \"./p.md\"\n");
        let Err(e) = Prompt::read(&without) else {
            panic!("{{scratch}} without a scratch")
        };
        assert!(e
            .to_string()
            .ends_with("{scratch}, but [builtins] scratch is off"));
        let plain = Prompt::read(&manifest(dir.path(), ""))
            .unwrap()
            .fill(Path::new("/w"), None);
        assert_eq!(plain, "You are a, an agent working in /w.");
        let named = Prompt::read(&manifest(dir.path(), ""))
            .unwrap()
            .fill(Path::new("/w"), Some(&scratch));
        assert_eq!(
            named,
            format!(
                "You are a, an agent working in /w. Your scratch folder, for what is not a deliverable: {}.",
                scratch.path().display()
            )
        );
        let gone = manifest(dir.path(), "prompt = \"./gone.md\"\n");
        assert!(Prompt::read(&gone)
            .err()
            .unwrap()
            .to_string()
            .starts_with("prompt "));
    }

    #[test]
    fn limits_come_from_the_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let limits = limits(&manifest(
            dir.path(),
            "[limits]\nmax_turns = 3\nmax_tokens = 1000\nmax_minutes = 2\n",
        ));
        assert_eq!(limits.max_turns, 3);
        assert_eq!(limits.max_total_tokens, 1000);
        assert_eq!(limits.max_duration, Duration::from_secs(120));
    }

    #[test]
    fn two_tools_of_one_name_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut tools = Tools::default();
        tools.add("yoagent", tools_for(ToolSet::Coding, dir.path()));
        assert!(tools.check().is_ok());
        let (session, _driver) = Session::new(false);
        let mut ours = Tools::default();
        ours.add(
            AskOwner::TOOL,
            vec![Box::new(AskOwner::new(session.clone()))],
        );
        assert!(ours.check().is_ok());
        let mut theirs = Tools::default();
        theirs.add("mcp x", vec![Box::new(AskOwner::new(session))]);
        let e = theirs.check().unwrap_err().to_string();
        assert!(e.starts_with("tool ask_owner from mcp x"), "{e}");
        let extend = crate::manifest::Extend::default();
        let sources = Sources::at("http://127.0.0.1:9").unwrap();
        let find = || {
            Box::new(FindPlugin::new(
                Session::new(false).0,
                extend.clone(),
                sources.clone(),
            ))
        };
        let mut ours = Tools::default();
        ours.add(FindPlugin::TOOL, vec![find()]);
        assert!(ours.check().is_ok());
        let mut theirs = Tools::default();
        theirs.add("mcp x", vec![find()]);
        let e = theirs.check().unwrap_err().to_string();
        assert_eq!(
            e,
            "tool find_plugin from mcp x: the name is yobuild's ([extend])"
        );
        tools.add("mcp files", tools_for(ToolSet::Coding, dir.path()));
        let e = tools.check().unwrap_err().to_string();
        assert_eq!(e, "two tools named bash: from yoagent and mcp files");
    }

    #[test]
    fn the_gate_allows_the_scratch_and_ask_owner_only_with_the_built_in() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("p");
        std::fs::create_dir(&project).unwrap();
        let scratch = Scratch::at(dir.path().join("s")).unwrap();
        let (session, _driver) = Session::new(false);
        let write = |gate: &Gate, path: &Path| {
            gate.check(
                "write_file",
                &serde_json::json!({ "path": path, "content": "x" }),
            )
            .question()
            .is_none()
        };
        let ask = |gate: &Gate| {
            gate.check(AskOwner::TOOL, &serde_json::json!({ "question": "q?" }))
                .question()
                .is_none()
        };
        let both = manifest(dir.path(), "[builtins]\nscratch = true\nask_owner = true\n");
        let judge = gate(&both, &session, &project, Some(&scratch)).unwrap();
        let elsewhere = dir.path().join("elsewhere.md");
        assert!(
            write(&judge, &scratch.path().join("notes.md")),
            "free in the scratch"
        );
        assert!(!write(&judge, &elsewhere), "outside both asks");
        assert!(ask(&judge), "ask_owner is allowed with the built-in");
        let neither = manifest(dir.path(), "");
        let judge = gate(&neither, &session, &project, None).unwrap();
        assert!(
            !write(&judge, &scratch.path().join("notes.md")),
            "no scratch: it asks"
        );
        assert!(
            !ask(&judge),
            "a tool named ask_owner asks without the built-in"
        );
        let find = |gate: &Gate| {
            gate.check(FindPlugin::TOOL, &serde_json::json!({ "need": "lint" }))
                .question()
                .is_none()
        };
        assert!(
            !find(&judge),
            "a tool named find_plugin asks without [extend]"
        );
        let extend = manifest(dir.path(), "[extend]\n");
        let judge = gate(&extend, &session, &project, None).unwrap();
        assert!(find(&judge), "find_plugin is allowed with [extend]");
        let load = |gate: &Gate| {
            gate.check(load::LoadPlugin::TOOL, &serde_json::json!({}))
                .question()
                .is_none()
        };
        assert!(load(&judge), "load_plugin is allowed with [extend]");
        let neither = gate(&neither, &session, &project, None).unwrap();
        assert!(!load(&neither), "and asks without it");
        let off = manifest(dir.path(), "[builtins]\ngate = \"off\"\n");
        assert!(gate(&off, &session, &project, None).is_none());
    }

    #[test]
    fn tool_names_models_take() {
        assert!(tool_name("server__echo") && tool_name("a-b_c9"));
        assert!(!tool_name("tools.example.com__x") && !tool_name("a b") && !tool_name(""));
        assert!(tool_name(&"x".repeat(64)) && !tool_name(&"x".repeat(65)));
    }

    /// The agent's gate is the host's plus the plugins' allowed tools,
    /// sharing what the agent made: `host_approval.judge` frees removing a
    /// file the agent wrote, and still asks for one it did not.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_hosts_gate_knows_the_files_the_agent_made() {
        use yoagent::provider::mock::{MockResponse, MockToolCall};
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().canonicalize().unwrap();
        std::fs::write(project.join("theirs.txt"), "").unwrap();
        let m = manifest(&project, "");
        let (session, _driver) = Session::new(false);
        let judge = gate(&m, &session, &project, None).unwrap();
        let check: OfferedCheck = Arc::new(|_run: &str, _tool: &str| false);
        let made = project.join("made.txt");
        let mut agent = Agent::from_provider(
            MockProvider::new(vec![
                MockResponse::ToolCalls(vec![MockToolCall {
                    name: "write_file".into(),
                    arguments: serde_json::json!({ "path": made, "content": "x" }),
                    provider_metadata: None,
                }]),
                MockResponse::Text("done".into()),
            ]),
            ModelConfig::mock(),
        )
        .with_tools(vec![Box::new(WriteFileTool::new())])
        .with_extension(agent_gate(judge.clone(), &m, check));
        let mut events = agent.prompt("go").await;
        while events.recv().await.is_some() {}
        agent.finish().await;
        assert!(made.exists());
        let free = |file: &str| {
            matches!(
                judge.check_argv(&["rm", file], &project),
                crate::builtins::Check::Free { .. }
            )
        };
        assert!(free("made.txt"));
        assert!(!free("theirs.txt"));
    }

    #[test]
    fn the_gate_allows_what_the_entries_allow_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let m = manifest(
            dir.path(),
            "[[plugin]]\npi = \"./t.ts\"\nallow = [\"todo\"]\n\
             [[mcp]]\nname = \"files\"\nnpm = \"./fs.sh\"\nallow = [\"read_text_file\"]\n",
        );
        let (servers, plugins) = allowed(&m);
        assert_eq!(
            (servers.as_slice(), plugins.as_slice()),
            (
                &["files__read_text_file".to_owned()][..],
                &["todo".to_owned()][..]
            )
        );
        let (session, _driver) = Session::new(false);
        let check: OfferedCheck = Arc::new(|_run: &str, tool: &str| tool == "todo");
        let judge = agent_gate(gate(&m, &session, dir.path(), None).unwrap(), &m, check);
        let runs = |tool: &str| {
            judge
                .check(tool, &serde_json::json!({}))
                .question()
                .is_none()
        };
        // An MCP server's allowed tool runs by name; a plugin's only in a run
        // where its plugin offered it (outside a run, it asks).
        assert!(runs("files__read_text_file") && !runs("todo"));
        assert!(!runs("files__write_file") && !runs("read_text_file") && !runs("todos"));
    }

    #[test]
    fn a_plugin_s_allowed_tool_comes_only_from_that_plugin() {
        use yoagent_rutis::ToolOwner;
        let dir = tempfile::tempdir().unwrap();
        let ext = dir.path().join("a.ts");
        std::fs::write(&ext, "").unwrap();
        let ext = std::fs::canonicalize(&ext).unwrap();
        let adapters = Adapters {
            pi: "pi-extensions-x1".into(),
            dsh: "dsh-tools-x1".into(),
        };
        let pi = Offerer::Pi(vec![ext.clone()]);
        let dsh_a = Offerer::Dsh(vec!["plugin:a".into(), "plugin:a:x".into()]);
        let words = Offerer::Rutis("plugin:words".into());
        let from_a =
            ToolOwner::new("pi-extensions-x1", "pi").with_source(ext.display().to_string());
        let from_b = ToolOwner::new("pi-extensions-x1", "pi").with_source("/elsewhere/b.ts");
        let no_source = ToolOwner::new("pi-extensions-x1", "pi");
        let dsh = ToolOwner::new("dsh-tools-x1", "dsh");
        let rutis = ToolOwner::new("words", "plugin:words");
        let no_row = ToolOwner::new("words", "language plugin (session 1)");
        let offers = |o: &Offerer, owner: &ToolOwner| o.offers(owner, &adapters);
        // The same file by another path (a symlink, `..`): still that file.
        #[cfg(unix)]
        {
            let link = dir.path().join("link");
            std::os::unix::fs::symlink(dir.path(), &link).unwrap();
            let via_link = ToolOwner::new("pi-extensions-x1", "pi")
                .with_source(link.join("sub/../a.ts").display().to_string());
            std::fs::create_dir(dir.path().join("sub")).unwrap();
            assert!(offers(&pi, &via_link));
        }
        // pi: the pi row's adapter handler and one of the plugin's files.
        assert!(offers(&pi, &from_a));
        assert!(!offers(&pi, &from_b) && !offers(&pi, &no_source) && !offers(&pi, &dsh));
        // The adapter's handler name from another row, or the pi row's
        // file claimed by another handler: not the pi plugin.
        let name_elsewhere =
            ToolOwner::new("pi-extensions-x1", "plugin:x").with_source(ext.display().to_string());
        let other_handler = ToolOwner::new("words", "pi").with_source(ext.display().to_string());
        assert!(!offers(&pi, &name_elsewhere) && !offers(&pi, &other_handler));
        // DSH: the dsh row's adapter handler, or the plugin's own rows.
        assert!(offers(&dsh_a, &dsh));
        assert!(offers(&dsh_a, &ToolOwner::new("own", "plugin:a:x")));
        assert!(!offers(&dsh_a, &ToolOwner::new("own", "plugin:b")));
        assert!(!offers(&dsh_a, &ToolOwner::new("dsh-tools-x1", "plugin:x")));
        assert!(!offers(&dsh_a, &from_a) && !offers(&dsh_a, &rutis));
        // rutis: its own row, exactly.
        assert!(offers(&words, &rutis));
        assert!(!offers(&words, &ToolOwner::new("words", "plugin:words2")));
        assert!(!offers(&words, &ToolOwner::new("words", "plugin:other")));
        assert!(!offers(&words, &dsh) && !offers(&words, &from_a));
        // A handler from no row is nobody's.
        for o in [&pi, &dsh_a, &words] {
            assert!(!offers(o, &no_row));
        }
    }

    #[test]
    fn the_agents_own_tool_is_never_freed_by_a_plugins_offer() {
        let (session, _driver) = Session::new(false);
        drop(session);
        let registry = Arc::new(yoagent_rutis::registry::Registry::default());
        let offerers: HashMap<String, Vec<Offerer>> = [(
            "web_fetch".to_owned(),
            vec![Offerer::Rutis("plugin:w".into())],
        )]
        .into();
        let adapters = Adapters {
            pi: "pi-extensions-x1".into(),
            dsh: "dsh-tools-x1".into(),
        };
        let check = offered_check(
            registry,
            offerers,
            adapters,
            ["web_fetch".to_owned()].into(),
        );
        assert!(!check("run-1", "web_fetch"), "the agent's own name");
        assert!(!check("run-1", "other"), "offered by nobody");
    }

    #[test]
    fn a_packages_bin() {
        let dir = tempfile::tempdir().unwrap();
        let write = |bin: &str| {
            std::fs::write(
                dir.path().join("package.json"),
                format!(r#"{{"version":"1.0.0","bin":{bin}}}"#),
            )
            .unwrap()
        };
        let bin = |name: Option<&str>| bin_of(dir.path(), "@s/srv", name);
        write(r#""./s.js""#);
        assert_eq!(bin(None).unwrap(), dir.path().join("./s.js"));
        assert_eq!(bin(Some("srv")).unwrap(), dir.path().join("./s.js"));
        assert_eq!(
            bin(Some("x")).unwrap_err(),
            "@s/srv has one bin, srv, not x"
        );
        write(r#"{"a":"./a.js","b":"./b.js"}"#);
        assert_eq!(bin(Some("b")).unwrap(), dir.path().join("./b.js"));
        assert!(bin(None).unwrap_err().contains("several bins"));
        assert_eq!(bin(Some("c")).unwrap_err(), "@s/srv has no bin \"c\"");
        write(r#"{"a":1}"#);
        assert_eq!(bin(None).unwrap_err(), "@s/srv: its bin is not a path");
        write("null");
        assert_eq!(bin(None).unwrap_err(), "@s/srv has no bin");
    }

    /// A stdio server has the credentials set here taken out but those its
    /// `env` sets (`env -u` would remove those too), and on unix every name
    /// not kept: through `env`, even with nothing withheld.
    #[test]
    fn a_server_has_the_withheld_credentials_taken_out_but_what_its_env_sets() {
        let dir = tempfile::tempdir().unwrap();
        let m = Manifest::parse(
            "[agent]\nname = \"a\"\nmodel = \"deepseek/deepseek-chat\"\n\
             [[mcp]]\nname = \"s\"\nnpm = \"./s.sh\"\nargs = [\"--x\"]\n\
             env = { OPENAI_API_KEY = \"${MINE}\" }\n",
            &dir.path().join("a.toml"),
            dir.path(),
        )
        .unwrap();
        std::fs::write(dir.path().join("s.sh"), "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::Permissions::from_mode(0o755);
            std::fs::set_permissions(dir.path().join("s.sh"), mode).unwrap();
        }
        let withheld = ["OPENAI_API_KEY".to_owned(), "GROQ_API_KEY".to_owned()];
        let launch = super::launch(
            &m.mcp[0],
            &install::Installed::default(),
            &|_: &str| Some("v".to_owned()),
            &withheld,
        )
        .unwrap();
        let Launch::Stdio {
            command,
            args,
            env,
            withheld,
        } = launch
        else {
            panic!("stdio")
        };
        assert_eq!(withheld, ["OPENAI_API_KEY", "GROQ_API_KEY"]);
        let (program, argv, env) = super::without(&command, &args, &env, &withheld).unwrap();
        assert_eq!(env.get("OPENAI_API_KEY").map(String::as_str), Some("v"));
        let command = command.display().to_string();
        if cfg!(unix) {
            assert_eq!(program, "/usr/bin/env");
            assert_eq!(argv[..2], ["-u", "GROQ_API_KEY"]);
            assert_eq!(argv[argv.len() - 2..], [command.as_str(), "--x"]);
            let taken: Vec<&String> = argv
                .chunks(2)
                .filter(|w| w[0] == "-u")
                .map(|w| &w[1])
                .collect();
            // Its own `env`'s name and the kept set stay.
            for name in ["OPENAI_API_KEY", "PATH", "HOME"] {
                assert!(!taken.iter().any(|t| *t == name), "{name} taken out");
            }
            // Nothing withheld: through `env` all the same.
            let script = dir.path().join("s.sh");
            let (program, _, _) = super::without(&script, &[], &env, &[]).unwrap();
            assert_eq!(program, "/usr/bin/env");
        } else {
            assert_eq!((program, argv), (command, vec!["--x".to_owned()]));
            assert_eq!(env.get("GROQ_API_KEY").map(String::as_str), Some(""));
        }
    }

    /// Each runtime keeps its own plugins' grant; only the terminal UI's
    /// runtime keeps pi-tui's settings.
    #[test]
    fn each_runtime_keeps_its_own_grant_and_only_ui_the_terminal_settings() {
        let plan = plugins::Plan {
            env: vec![
                ("pi".to_owned(), vec!["TAVILY_API_KEY".to_owned()]),
                ("rutis-0".to_owned(), vec!["PI_LENS_*".to_owned()]),
            ],
            ..plugins::Plan::default()
        };
        let ui = super::kept_for("ui", &plan);
        assert_eq!(ui, crate::environment::UI_KEPT);
        assert!(ui.iter().any(|e| e == "PI_HYPERLINKS"));
        assert_eq!(super::kept_for("pi", &plan), ["TAVILY_API_KEY"]);
        assert_eq!(super::kept_for("rutis-0", &plan), ["PI_LENS_*"]);
        assert!(super::kept_for("dsh", &plan).is_empty());
    }

    /// A stdio server's `-u` list, exactly: the withheld credentials once,
    /// every name not kept, but what its own `env` sets and the kept set.
    #[cfg(unix)]
    #[test]
    fn a_stdio_servers_list_is_every_name_not_kept_but_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("s.sh");
        std::fs::write(&script, "#!/bin/sh\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let env: HashMap<String, String> = [("SERVER_TOKEN".to_owned(), "t".to_owned())].into();
        let vars = [
            "PATH", "LANG", "OPENAI_API_KEY", "UNRELATED", "SERVER_TOKEN", "RUTIS_CHANNEL_TOKEN",
        ]
        .map(std::ffi::OsString::from);
        let withheld = ["OPENAI_API_KEY".to_owned()];
        let (program, argv, _) = super::without_from(&script, &[], &env, &withheld, vars).unwrap();
        assert_eq!(program, "/usr/bin/env");
        let script = script.display().to_string();
        assert_eq!(
            argv,
            // This process's own channel token too: rutis starts no server.
            [
                "-u",
                "OPENAI_API_KEY",
                "-u",
                "UNRELATED",
                "-u",
                "RUTIS_CHANNEL_TOKEN",
                script.as_str()
            ]
        );
        // A name to take out that is not UTF-8 refuses the start.
        use std::os::unix::ffi::OsStringExt;
        let odd = std::ffi::OsString::from_vec(b"BAD\xffNAME".to_vec());
        let error = super::without_from(Path::new(&script), &[], &env, &[], [odd])
            .unwrap_err()
            .to_string();
        assert!(error.contains("has a name that is not UTF-8"), "{error}");
    }

    /// Which of yobuild's runtime sets a manifest needs (design §13): none
    /// for no plugin (and no home looked up), `adapters` for pi or DSH (or
    /// `[extend]` finding them), `ui`
    /// for a built-in or the terminal UI, neither for a rutis plugin of
    /// one's own; and when Node and npm are needed.
    #[test]
    fn a_manifest_gets_the_runtime_sets_it_needs() {
        use crate::runtime::{seed, RuntimeSet};
        let dir = tempfile::tempdir().unwrap();
        let m = |entries: &str| {
            Manifest::parse(
                &format!("[agent]\nname = \"a\"\nmodel = \"deepseek/deepseek-chat\"\n{entries}"),
                &dir.path().join("a.toml"),
                dir.path(),
            )
            .unwrap()
        };
        let home = tempfile::tempdir().unwrap();
        let adapters = seed(home.path(), RuntimeSet::Adapters);
        let ui = seed(home.path(), RuntimeSet::Ui);
        let never = |text: &str| panic!("nothing is installed here: {text}");
        let layout = |manifest: &Manifest, terminal: bool, home: Option<&Path>| {
            Layout::installed(manifest, home, terminal, &never).unwrap()
        };
        let installed = |l: &Layout| (l.adapters == adapters, l.frontend == ui);
        // No plugin: nothing, not even a home.
        let none = layout(&m(""), false, Some(Path::new("/no/home/at/all")));
        assert_eq!(installed(&none), (false, false));
        assert!(!none.adapters.exists() && !none.frontend.exists());
        let at = Some(home.path());
        for (entries, terminal, want) in [
            ("[[plugin]]\npi = \"./p.ts\"\n", false, (true, false)),
            ("[[plugin]]\ndsh = \"./d.ts\"\n", false, (true, false)),
            // A package the agent may load runs on the adapters too.
            ("[extend]\nkinds = [\"pi\"]\n", false, (true, false)),
            (
                "[extend]\nkinds = [\"mcp\", \"dsh\"]\n",
                false,
                (true, false),
            ),
            ("[extend]\n", false, (false, false)),
            (
                "[[plugin]]\nrutis = \"yobuild:todo-plan\"\n",
                false,
                (false, true),
            ),
            (
                "[[plugin]]\npi = \"yobuild:confirm-dangerous\"\n",
                false,
                (true, true),
            ),
            (
                "[[plugin]]\nrutis = \"./r.ts\"\nruntime = \"node\"\n",
                false,
                (false, false),
            ),
            ("", true, (false, true)),
        ] {
            assert_eq!(
                installed(&layout(&m(entries), terminal, at)),
                want,
                "{entries} {terminal}"
            );
        }
        // Node: a plugin, a pinned server, the terminal UI.
        assert!(!needs_node(&m(""), false));
        assert!(needs_node(&m(""), true));
        assert!(
            needs_node(&m("[extend]\n"), false),
            "find_plugin searches with npm"
        );
        // Remote servers are the MCP registry's: npm is not asked.
        assert!(!needs_node(&m("[extend]\nkinds = [\"remote\"]\n"), false));
        assert!(needs_node(
            &m("[extend]\nkinds = [\"remote\", \"pi\"]\n"),
            false
        ));
        assert!(needs_node(
            &m("[[plugin]]\nrutis = \"./r.ts\"\nruntime = \"node\"\n"),
            false
        ));
        assert!(needs_node(
            &m("[[mcp]]\nname = \"s\"\nnpm = \"srv@1.0.0\"\n"),
            false
        ));
        assert!(!needs_node(
            &m("[[mcp]]\nname = \"s\"\nnpm = \"./s.sh\"\n"),
            false
        ));
        assert!(!needs_node(
            &m("[[mcp]]\nname = \"h\"\nurl = \"https://mcp.example/\"\n"),
            false
        ));
    }

    /// The one environment note: on unix the count of what one or more
    /// entries do not get, the credentials named; off unix the credentials
    /// only; nothing when nothing starts or nothing is left out.
    #[test]
    fn the_environment_note_counts_what_is_left_out_and_names_the_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let m = |entries: &str| {
            Manifest::parse(
                &format!("[agent]\nname = \"a\"\nmodel = \"deepseek/deepseek-chat\"\n{entries}"),
                &dir.path().join("a.toml"),
                dir.path(),
            )
            .unwrap()
        };
        let vars = || {
            [
                "PATH", "LC_ALL", "GITHUB_TOKEN", "UNRELATED", "OPENAI_API_KEY", "=x",
            ]
            .map(std::ffi::OsString::from)
        };
        let plugin = m("[[plugin]]\nname = \"p\"\ndsh = \"./p.ts\"\n");
        assert_eq!(
            super::left_out(&plugin, vars(), true),
            ["GITHUB_TOKEN", "OPENAI_API_KEY", "UNRELATED"]
        );
        let said = super::environment_note(&plugin, vars(), true).unwrap();
        assert!(
            said.contains(": 3 variable(s) set here are left out of one or more of them (`check --json` lists them: the agent's `leftOut`), among them the credentials yoagent reads (`OPENAI_API_KEY`); an entry that needs one gets it only from its entry"),
            "{said}"
        );
        // Off unix: the credentials only.
        assert_eq!(super::left_out(&plugin, vars(), false), ["OPENAI_API_KEY"]);
        assert!(super::environment_note(&plugin, vars(), false)
            .unwrap()
            .contains("inherit the whole environment here but the credentials yoagent reads, set empty in their environment (there is no /usr/bin/env here to start them through): `OPENAI_API_KEY`;"));
        // What every entry gets is not left out; with no credential, how
        // to pass one is said instead.
        let granted = m("[[plugin]]\nname = \"p\"\ndsh = \"./p.ts\"\ngrants = { env = [\"GITHUB_TOKEN\", \"OPENAI_API_KEY\"] }\n");
        assert_eq!(super::left_out(&granted, vars(), true), ["UNRELATED"]);
        assert!(super::environment_note(&granted, vars(), true)
            .unwrap()
            .ends_with(": 1 variable(s) set here are left out of one or more of them (`check --json` lists them: the agent's `leftOut`); an `env` grant on a plugin, or a stdio server's `env` table, passes one it needs"));
        // A server that does not set it loses it all the same; one whose
        // `env` sets it, beside a plugin's prefix, does not.
        let both = m("[[plugin]]\nname = \"p\"\ndsh = \"./p.ts\"\ngrants = { env = [\"UNRELATED\"] }\n[[mcp]]\nname = \"s\"\nnpm = \"./s.sh\"\n");
        assert!(super::left_out(&both, vars(), true).contains(&"UNRELATED".to_owned()));
        let all = m("[[plugin]]\nname = \"p\"\ndsh = \"./p.ts\"\ngrants = { env = [\"UNRELATED\", \"GITHUB_*\"] }\n[[mcp]]\nname = \"s\"\nnpm = \"./s.sh\"\nenv = { UNRELATED = \"x\", GITHUB_TOKEN = \"${GITHUB_TOKEN}\" }\n");
        assert_eq!(super::left_out(&all, vars(), true), ["OPENAI_API_KEY"]);
        // The DSH runtime's own home, set by its launch, is not left out
        // of a DSH plugin given one (host services, the DSH host).
        let home = ["DSH_HOME", "DSH_AGENTS_HOME"].map(std::ffi::OsString::from);
        assert_eq!(
            super::left_out(&plugin, home.clone(), true),
            ["DSH_AGENTS_HOME", "DSH_HOME"]
        );
        let hosted = m("[dsh]\nhost_services = true\n[[plugin]]\nname = \"p\"\ndsh = \"./p.ts\"\n");
        assert_eq!(
            super::dsh_launch(&hosted, false),
            ["DSH_HOME", "DSH_AGENTS_HOME"]
        );
        assert_eq!(super::dsh_launch(&hosted, true), ["DSH_HOME"]);
        assert!(super::dsh_launch(&plugin, false).is_empty());
        let granted_dsh =
            m("[[plugin]]\nname = \"p\"\ndsh = \"./p.ts\"\ngrants = { workspace = true }\n");
        assert_eq!(super::dsh_launch(&granted_dsh, true), ["DSH_HOME"]);
        if crate::frontend::host::DshHome::chosen().is_none() {
            assert!(super::left_out(&hosted, home, true).is_empty());
        }
        // Nothing starts, or nothing is left out.
        assert!(super::environment_note(&m(""), vars(), true).is_none());
        let http = m("[[mcp]]\nname = \"h\"\nurl = \"https://mcp.example/\"\n");
        assert!(super::environment_note(&http, vars(), true).is_none());
        assert!(super::environment_note(&plugin, ["PATH".into()], true).is_none());
    }

    /// What an entry's code reads but does not get: a credential never
    /// with "grant it"; a plugin pointed at its grant, a server at its
    /// `env` table; what it gets, what is kept or unset, and (off unix)
    /// anything but a credential, unsaid.
    #[test]
    fn what_an_entrys_code_reads_but_does_not_get_is_said_by_kind() {
        let dir = tempfile::tempdir().unwrap();
        let m = Manifest::parse(
            "[agent]\nname = \"a\"\nmodel = \"deepseek/deepseek-chat\"\n\
             [[plugin]]\nname = \"p\"\ndsh = \"./p.ts\"\ngrants = { env = [\"GIVEN\"] }\n\
             [[mcp]]\nname = \"s\"\nnpm = \"./s.sh\"\nenv = { OWN = \"x\" }\n",
            &dir.path().join("a.toml"),
            dir.path(),
        )
        .unwrap();
        let read: std::collections::BTreeSet<String> = [
            "TAVILY_API_KEY", "OPENAI_API_KEY", "GIVEN", "OWN", "LANG", "UNSET", "NODE_PATH",
        ]
        .map(String::from)
        .into();
        let set = |n: &str| n != "UNSET";
        let credentials = m.key_variables();
        let all = super::starts(&m);
        let plugin = super::reads_notes(all[0], &read, set, &credentials, true);
        assert_eq!(
            plugin,
            [
                "its code reads `NODE_PATH`, which is set here but left out of its environment on purpose: it can load code into its runtime",
                "its code reads `OPENAI_API_KEY`, a credential yoagent reads, taken out of its environment: one handed over is spent unmetered",
                "its code reads `OWN`, which is set here but left out of its environment: grant it (`grants = { env = [\"OWN\"] }`) if it should have it",
                "its code reads `TAVILY_API_KEY`, which is set here but left out of its environment: grant it (`grants = { env = [\"TAVILY_API_KEY\"] }`) if it should have it",
            ]
        );
        let server = super::reads_notes(all[1], &read, set, &credentials, true);
        assert_eq!(server.len(), 4, "{server:?}");
        assert!(server.contains(&"its code reads `GIVEN`, which is set here but left out of its environment: hand it over in its `env` table (`GIVEN = \"${GIVEN}\"`) if it should have it".to_owned()));
        // Off unix only the credential is left out: set empty.
        assert_eq!(
            super::reads_notes(all[0], &read, set, &credentials, false),
            ["its code reads `OPENAI_API_KEY`, a credential yoagent reads, set empty in its environment: one handed over is spent unmetered"]
        );
        assert_eq!(all[0].environment(), ["GIVEN"]);
        assert_eq!(all[1].environment(), ["OWN"]);
    }

    /// A granted name that loads code, or looks like a secret, is said when
    /// set here; a credential is `keys_given`'s, a plain one unsaid.
    #[test]
    fn a_granted_secret_or_code_loader_set_here_is_said() {
        let dir = tempfile::tempdir().unwrap();
        let m = Manifest::parse(
            "[agent]\nname = \"a\"\nmodel = \"deepseek/deepseek-chat\"\n\
             [[plugin]]\nname = \"p\"\ndsh = \"./p.ts\"\n\
             grants = { env = [\"NODE_OPTIONS\", \"TAVILY_*\", \"OPENAI_API_KEY\", \"PLAIN\"] }\n",
            &dir.path().join("a.toml"),
            dir.path(),
        )
        .unwrap();
        let vars = [
            "NODE_OPTIONS", "TAVILY_API_KEY", "TAVILY_REGION", "OPENAI_API_KEY", "PLAIN",
        ]
        .map(std::ffi::OsString::from);
        let notes = super::given_notes(&m.plugins[0], vars, |_| true, &m.key_variables());
        assert_eq!(
            notes,
            [
                "it is given `NODE_OPTIONS`, which can load code into its runtime",
                "it is given `TAVILY_API_KEY`, set here: the owner's own, used as the plugin sees fit",
            ]
        );
    }

    /// A kept proxy holding a password is said, when something starts.
    #[test]
    fn a_proxy_with_a_password_is_said() {
        let dir = tempfile::tempdir().unwrap();
        let m = |entries: &str| {
            Manifest::parse(
                &format!("[agent]\nname = \"a\"\nmodel = \"deepseek/deepseek-chat\"\n{entries}"),
                &dir.path().join("a.toml"),
                dir.path(),
            )
            .unwrap()
        };
        let vars = || {
            [
                ("https_proxy", "me:pw@proxy:3128"),
                ("HTTP_PROXY", "http://proxy:3128"),
                ("ALL_PROXY", "socks5://me:pw@proxy:1080"),
                ("OTHER", "http://me:pw@x"),
            ]
            .map(|(k, v)| (k.into(), v.into()))
        };
        let plugin = m("[[plugin]]\nname = \"p\"\ndsh = \"./p.ts\"\n");
        assert_eq!(
            super::proxy_note(&plugin, vars()).unwrap(),
            "`ALL_PROXY`, `https_proxy` hold(s) a password, and every plugin and stdio MCP server gets it as the owner's network setting"
        );
        assert!(super::proxy_note(&m(""), vars()).is_none());
        let http = m("[[mcp]]\nname = \"h\"\nurl = \"https://mcp.example/\"\n");
        assert!(super::proxy_note(&http, vars()).is_none());
        // A value not UTF-8 is read all the same.
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let odd = std::ffi::OsString::from_vec(b"http://me:p\xffw@proxy".to_vec());
            assert!(super::proxy_note(&plugin, [("HTTP_PROXY".into(), odd)]).is_some());
        }
    }

    /// An `env` grant's prefix is never said unset, and a grant holding one
    /// is not "nothing handed over".
    #[test]
    fn an_env_grants_prefix_is_not_said_unset() {
        let dir = tempfile::tempdir().unwrap();
        let m = |grant: &str| {
            Manifest::parse(
                &format!("[agent]\nname = \"a\"\nmodel = \"deepseek/deepseek-chat\"\n[[plugin]]\nname = \"p\"\ndsh = \"./p.ts\"\ngrants = {{ env = {grant} }}\n"),
                &dir.path().join("a.toml"),
                dir.path(),
            )
            .unwrap()
        };
        let unset = |_: &str| false;
        let both = super::env_unset(&m(r#"["GROQ_API_KEY", "PI_LENS_*"]"#), unset);
        assert_eq!(
            both,
            [super::UnsetGrant {
                plugin: "p".to_owned(),
                names: vec!["GROQ_API_KEY".to_owned()],
                all: false,
            }]
        );
        assert!(super::env_unset(&m(r#"["PI_LENS_*"]"#), unset).is_empty());
    }

    #[test]
    fn the_dyld_note_names_only_the_dyld_variables_an_entry_is_handed_on_macos() {
        let dir = tempfile::tempdir().unwrap();
        let m = |entries: &str| {
            Manifest::parse(
                &format!("[agent]\nname = \"a\"\nmodel = \"deepseek/deepseek-chat\"\n{entries}"),
                &dir.path().join("a.toml"),
                dir.path(),
            )
            .unwrap()
        };
        let note = |m: &Manifest, vars: &[&str], macos: bool| {
            super::dyld_note(m, vars.iter().map(Into::into), macos)
        };
        let said = |names: &str| {
            format!(
                "{names}, handed to a plugin or stdio MCP server, does not reach it while System Integrity Protection is on (macOS's default): every plugin runtime and stdio server starts through the protected /usr/bin/env, which drops `DYLD_*` variables"
            )
        };
        let vars = ["PATH", "DYLD_LIBRARY_PATH", "DYLD_FALLBACK_LIBRARY_PATH"];
        // Set here but handed to nobody: the kept environment leaves them
        // out anyway, nothing to say.
        let plain = m(
            "[[plugin]]\nname = \"p\"\ndsh = \"./p.ts\"\n[[mcp]]\nname = \"s\"\nnpm = \"./s.sh\"\n",
        );
        assert_eq!(note(&plain, &vars, true), None);
        // A granted one set here, and a server's own.
        let handed = m(
            "[[plugin]]\nname = \"p\"\ndsh = \"./p.ts\"\ngrants = { env = [\"DYLD_LIBRARY_PATH\", \"DYLD_INSERT_LIBRARIES\"] }\n\
             [[mcp]]\nname = \"s\"\nnpm = \"./s.sh\"\nenv = { DYLD_X = \"1\" }\n",
        );
        assert_eq!(
            note(&handed, &vars, true),
            Some(said("`DYLD_LIBRARY_PATH`, `DYLD_X`"))
        );
        // Only on macOS.
        assert_eq!(note(&handed, &vars, false), None);
    }

    #[test]
    fn each_entry_says_what_it_loses_only_when_they_lose_different_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let from = |entries: &str, withheld: &[&str]| {
            let m = Manifest::parse(
                &format!("[agent]\nname = \"a\"\nmodel = \"deepseek/deepseek-chat\"\n{entries}"),
                &dir.path().join("a.toml"),
                dir.path(),
            )
            .unwrap();
            let withheld: Vec<String> = withheld.iter().map(|k| k.to_string()).collect();
            super::withheld_from(&m, &withheld)
        };
        let both = ["DEEPSEEK_API_KEY", "OPENAI_API_KEY"];
        let plain = "[[plugin]]\nname = \"p\"\ndsh = \"./p.ts\"\n\
                     [[mcp]]\nname = \"s\"\nnpm = \"./s.sh\"\n\
                     [[mcp]]\nname = \"h\"\nurl = \"https://mcp.example/\"\n";
        // All alike: the combined note says it.
        assert!(from(plain, &both).is_empty());
        let names = |n: &[&str]| n.iter().map(|k| k.to_string()).collect::<Vec<_>>();
        // A server's `env` keeps one: each says its own (the HTTP server
        // starts nothing).
        assert_eq!(
            from(
                "[[plugin]]\nname = \"p\"\ndsh = \"./p.ts\"\n\
                 [[mcp]]\nname = \"s\"\nnpm = \"./s.sh\"\n\
                 env = { OPENAI_API_KEY = \"${MINE}\" }\n\
                 [[mcp]]\nname = \"h\"\nurl = \"https://mcp.example/\"\n",
                &both
            ),
            [
                ("p".to_owned(), names(&both)),
                ("s".to_owned(), names(&["DEEPSEEK_API_KEY"])),
            ]
        );
        // A plugin keeping all it would lose is left out.
        assert_eq!(
            from(
                "[[plugin]]\nname = \"p\"\ndsh = \"./p.ts\"\ngrants = { env = [\"OPENAI_API_KEY\"] }\n\
                 [[mcp]]\nname = \"s\"\nnpm = \"./s.sh\"\n",
                &["OPENAI_API_KEY"]
            ),
            [("s".to_owned(), names(&["OPENAI_API_KEY"]))]
        );
    }

    /// A command whose path holds `=` (which `env` would read as an
    /// assignment) still runs, with its arguments, each name taken out once.
    #[cfg(unix)]
    #[test]
    fn a_server_in_a_folder_named_with_an_equals_sign_still_starts() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("a=b");
        std::fs::create_dir(&folder).unwrap();
        let script = folder.join("s.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\necho \"ran $1 [${HOME+home}] [${YOBUILD_T+set}]\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let withheld = ["YOBUILD_T".to_owned(), "YOBUILD_T".to_owned()];
        let (program, argv, _) =
            super::without(&script, &["x=1".to_owned()], &HashMap::new(), &withheld).unwrap();
        assert_eq!(argv[..2], ["-u", "YOBUILD_T"]);
        assert!(argv.windows(2).any(|w| w == ["/bin/sh", "-c"]));
        assert_eq!(argv.iter().filter(|w| *w == "YOBUILD_T").count(), 1);
        let out = std::process::Command::new(program)
            .args(argv)
            .env("YOBUILD_T", "secret")
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "ran x=1 [home] []\n");
    }

    /// Through `env`, a program that is not there or cannot run would only
    /// show as `env` exiting 127 or 126: it is looked for first, as the
    /// child will (a bare name on the `PATH` the server's `env` sets), and
    /// for a server whatever is withheld.
    #[cfg(unix)]
    #[test]
    fn a_server_that_cannot_start_is_said_before_env_runs() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let withheld = ["YOBUILD_T".to_owned()];
        let start = |command: &Path, env: &[(&str, &Path)]| {
            let env = env
                .iter()
                .map(|(k, v)| (k.to_string(), v.display().to_string()))
                .collect();
            super::without(command, &[], &env, &withheld).map(|(program, argv, _)| (program, argv))
        };
        let missing = dir.path().join("missing.sh");
        assert_eq!(
            start(&missing, &[]).unwrap_err().to_string(),
            format!("yobuild cannot start {}: no such file", missing.display())
        );
        let script = dir.path().join("s.sh");
        std::fs::write(&script, "#!/bin/sh\n").unwrap();
        assert_eq!(
            start(&script, &[]).unwrap_err().to_string(),
            format!(
                "yobuild cannot start {}: it is not executable",
                script.display()
            )
        );
        // A folder is no program, and not said to be missing.
        assert_eq!(
            start(dir.path(), &[]).unwrap_err().to_string(),
            format!(
                "yobuild cannot start {}: it is not a file",
                dir.path().display()
            )
        );
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(start(&script, &[]).unwrap().0, "/usr/bin/env");
        // A bare name: on the `PATH` the server's `env` sets, not this
        // process's.
        let name = Path::new("s.sh");
        assert_eq!(
            start(name, &[]).unwrap_err().to_string(),
            "yobuild cannot start s.sh: it is not on PATH"
        );
        assert!(start(name, &[("PATH", dir.path())]).is_ok());
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            start(name, &[("PATH", dir.path())])
                .unwrap_err()
                .to_string(),
            "yobuild cannot start s.sh: it is on PATH but not executable"
        );
        // Nothing withheld: looked at all the same, in the same words.
        assert_eq!(
            super::without(&missing, &[], &HashMap::new(), &[])
                .unwrap_err()
                .to_string(),
            format!("yobuild cannot start {}: no such file", missing.display())
        );
    }

    #[test]
    fn the_keys_handed_over_are_the_key_variables_an_entry_refers_to_or_is_filled_under() {
        let dir = tempfile::tempdir().unwrap();
        let m = Manifest::parse(
            r#"[agent]
name = "a"
model = "acme/m"
base_url = "https://acme.example/v1"
api_key_env = "ACME_KEY"
[[plugin]]
name = "nested"
dsh = "./d.ts"
config = { apiKey = "${OPENAI_API_KEY}", deep = { list = ["x ${ACME_KEY}", "${OPENAI_API_KEY}"] }, home = "${HOME}" }
[[plugin]]
name = "plain"
dsh = "./e.ts"
config = { home = "${HOME}" }
[[plugin]]
name = "granted"
rutis = "./g.ts"
runtime = "node"
config = { key = "${ACME_KEY}" }
grants = { env = ["OPENAI_API_KEY"] }
[[mcp]]
name = "stdio"
npm = "./s.sh"
args = ["--token", "${GROQ_API_KEY}"]
env = { OPENAI_API_KEY = "${MINE}", LOG = "${DEEPSEEK_API_KEY_FILE}" }
[[mcp]]
name = "web"
url = "https://mcp.example/${API_KEY}"
headers = { Authorization = "Bearer ${ANTHROPIC_API_KEY}" }
"#,
            &dir.path().join("a.toml"),
            dir.path(),
        )
        .unwrap();
        let given = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        assert_eq!(
            super::keys_given(&m, |_| true),
            [
                // Nested, each once; `HOME` is no key; the custom
                // provider's own variable is one.
                ("nested".to_owned(), given(&["OPENAI_API_KEY", "ACME_KEY"])),
                // Its `env` grant first, then its config's.
                ("granted".to_owned(), given(&["OPENAI_API_KEY", "ACME_KEY"])),
                // Filled under a key's name, and in its arguments; a name
                // that only starts like a key's is none.
                (
                    "stdio".to_owned(),
                    given(&["OPENAI_API_KEY", "GROQ_API_KEY"])
                ),
                ("web".to_owned(), given(&["API_KEY", "ANTHROPIC_API_KEY"])),
            ]
        );
        // A name not set (or empty) here hands nothing over: not given —
        // an `env` grant's, a reference's — but an `env` name filled from
        // a set reference is (`MINE` fills `OPENAI_API_KEY`).
        let set = |name: &str| name != "OPENAI_API_KEY";
        assert_eq!(
            super::keys_given(&m, set),
            [
                ("nested".to_owned(), given(&["ACME_KEY"])),
                ("granted".to_owned(), given(&["ACME_KEY"])),
                (
                    "stdio".to_owned(),
                    given(&["OPENAI_API_KEY", "GROQ_API_KEY"])
                ),
                ("web".to_owned(), given(&["API_KEY", "ANTHROPIC_API_KEY"])),
            ]
        );
        let unset = |names: &[&str], all: bool| super::UnsetGrant {
            plugin: "granted".to_owned(),
            names: given(names),
            all,
        };
        assert_eq!(
            super::env_unset(&m, set),
            [unset(&["OPENAI_API_KEY"], true)]
        );
        assert!(super::env_unset(&m, |_| true).is_empty());
        // `MINE` empty: the `env` name it fills is not given either.
        assert_eq!(
            super::keys_given(&m, |name| name != "MINE")[2],
            ("stdio".to_owned(), given(&["GROQ_API_KEY"]))
        );
        // A grant with one of two names set: only the other is said, and
        // not as handing nothing over.
        let two = Manifest::parse(
            "[agent]\nname = \"a\"\nmodel = \"deepseek/deepseek-chat\"\n\
             [[plugin]]\nname = \"granted\"\ndsh = \"./d.ts\"\n\
             grants = { env = [\"ANTHROPIC_API_KEY\", \"OPENAI_API_KEY\"] }\n",
            &dir.path().join("a.toml"),
            dir.path(),
        )
        .unwrap();
        let partial = super::env_unset(&two, set);
        assert_eq!(partial, [unset(&["OPENAI_API_KEY"], false)]);
        assert_eq!(
            super::unset_grant_note(&partial[0]),
            "its `env` grant names `OPENAI_API_KEY`, unset or empty here: not handed over (the rest of the grant is)"
        );
        assert_eq!(
            super::unset_grant_note(&unset(&["OPENAI_API_KEY"], true)),
            "its `env` grant names `OPENAI_API_KEY`, unset or empty here: it gets nothing from it"
        );
    }
}
