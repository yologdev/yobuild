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
pub(crate) mod host;
pub(crate) mod measure;
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

use crate::builtins::{AskOwner, Facts, Gate, OfferedCheck, Scratch};
use crate::frontend::host::{DshHome, PluginHost, Row};
use crate::frontend::session::UI_TIMEOUT;
use crate::frontend::{
    services, web, ClientMessage, NoticeLevel, RunOutcome, ServerMessage, Session,
};
use crate::install;
use crate::manifest::{
    Gate as GateMode, Manifest, Mcp, PluginKind, Source, Tools as ToolSet, Transport,
};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Where yobuild's own plugin files are: the frontend's (`frontend/plugins`)
/// and the adapters copied from yoagent-rutis (`adapters/`,
/// `scripts/copy-adapters.sh`).
#[derive(Debug, Clone)]
pub struct Layout {
    pub frontend: PathBuf,
    pub adapters: PathBuf,
}

impl Default for Layout {
    /// The checkout this crate was built from: `yobuild run` needs its
    /// `frontend/plugins` and `adapters/` (set up by
    /// `scripts/try-coding-agent.sh --setup-only`).
    fn default() -> Self {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        Self {
            frontend: repo.join("frontend"),
            adapters: repo.join("adapters"),
        }
    }
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
}

/// Runs the agent: until the frontend ends the session (terminal; an error
/// when the terminal UI stops on its own), Ctrl+C (web), or the task's run
/// ends (`once`).
pub async fn run(root: &Ctx, options: Options) -> Result<Ended, BoxError> {
    let started = Instant::now();
    let cwd = std::env::current_dir()?;
    let checked = check(&options, &cwd)?;
    let Options {
        manifest,
        task,
        once,
        measure: options_measure,
        addr,
        layout,
        ..
    } = options;
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
    // What changed in `agent.lock`.
    for said in checked.installed.said() {
        tracing::info!("{said}");
        session.notice(NoticeLevel::Info, said.clone());
    }
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
    } = start(
        root,
        &manifest,
        &checked.plan,
        &layout,
        &session,
        terminal,
        &cwd,
        scratch.as_ref(),
        once,
    )
    .await?;
    let plan = &checked.plan;
    let mut host = host;

    // The tools: yoagent's (in `cwd`), `ask_owner`, each MCP server's; no
    // two of one name. (A plugin's tools come with each run; yoagent keeps
    // one of these over a plugin's of the same name, and says so.)
    let mut tools = Tools::default();
    tools.add("yoagent", tools_for(manifest.builtins.tools, &cwd));
    if let Some(ask) = &ask_owner {
        tools.add(AskOwner::TOOL, vec![Box::new(ask.clone())]);
    }
    for server in &manifest.mcp {
        let found = mcp(server, &checked.servers[&server.name]).await?;
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
    .with_execution_limits(limits(&manifest))
    .with_extension({
        // A plugin that is the approval policy: no run goes on unguarded —
        // without a policy plugin up, every call is denied.
        let extension = bridge.extension();
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
    if let Some(gate) = judge.map(|g| agent_gate(g, &manifest, offered)) {
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
    eprintln!("{name}: ready in {:.1}s", started.elapsed().as_secs_f64());

    let ui = "terminal-ui";
    let mut served = None;
    if terminal {
        host.load([
            Row::new(ui, layout.frontend.join("plugins/terminal-ui.ts").display()).runtime("ui"),
        ])
        .await?;
    } else if !once {
        let phone = manifest.frontend.phone;
        let addr = addr.unwrap_or_else(|| match phone {
            true => "0.0.0.0:8787".parse().expect("an address"),
            false => "127.0.0.1:8787".parse().expect("an address"),
        });
        let web = web::serve(session.clone(), addr).await?;
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
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let mut reported = std::collections::HashSet::new();
    let mut quitting = false;
    // Until the session ends: the frontend quits (terminal), its run ends
    // (`once`), or Ctrl+C. Meanwhile a plugin that stops is said.
    let end = loop {
        tokio::select! {
            joined = &mut driving => break End::Driven(Box::new(joined)),
            stop = &mut ctrl_c => {
                // As the driver sees it: the observer's copy may lag.
                let interrupted = session.is_running();
                break match stop {
                    Ok(()) => End::Interrupted { running: interrupted },
                    Err(e) => End::NoSignal(e),
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
                // A row that fails, stops, or loses its runtime: said once.
                // (The terminal UI's own row is watched below.)
                for stopped in host.stopped(&[ui]) {
                    if reported.insert(stopped.clone()) {
                        tracing::error!("{stopped}");
                        session.notice(NoticeLevel::Error, format!("A plugin stopped: {stopped}"));
                        if let Some(fix) = what_to_do_at_start(&format!("a plugin stopped: {stopped}")) {
                            session.notice(NoticeLevel::Info, format!("What to do: {fix}."));
                        }
                    }
                }
                if terminal && !host.is_running(ui) {
                    break End::UiGone;
                }
            }
        }
    };
    let (agent, interrupted, failure) = match end {
        End::Driven(joined) => match *joined {
            Ok(agent) => (Some(agent), false, None),
            Err(e) => (None, false, Some(format!("the agent's task failed: {e}"))),
        },
        End::Interrupted { running } => {
            driving.abort();
            drop(driving.await);
            (None, running, None)
        }
        End::NoSignal(e) => {
            driving.abort();
            drop(driving.await);
            (None, false, Some(format!("cannot listen for Ctrl+C: {e}")))
        }
        End::UiGone => {
            // Nobody is there to quit: the session would wait for ever.
            session.send(ClientMessage::Quit);
            let agent = driving.await.ok();
            let why = host.stopped(&[]).join("; ");
            (
                agent,
                false,
                Some(format!("the terminal UI stopped: {why}")),
            )
        }
    };
    drop(served);
    let ended = finish(seen, watching, agent, once, interrupted).await;
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
    /// Ctrl+C; whether a run was in progress.
    Interrupted { running: bool },
    /// Ctrl+C cannot be listened for.
    NoSignal(std::io::Error),
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
        host::Host::new(session.clone(), manifest, cwd, judge.clone(), once),
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
    // DSH's own host services, on request: in a home of their own.
    let dsh_home = match plan.dsh && manifest.dsh.host_services {
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
    let mut host = builder.start(root).await?;
    if !plan.rows.is_empty() {
        host.load(plan.rows.clone())
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
    notes.extend(plan.notes.iter().cloned());
    let servers = manifest
        .mcp
        .iter()
        .map(|server| Ok((server.name.clone(), launch(server, &installed, &lookup)?)))
        .collect::<Result<_, BoxError>>()?;
    Ok(Prepared {
        installed,
        plan,
        servers,
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

    /// No two tools of one name, and none named `ask_owner` but yobuild's
    /// (a server's would be allowed by the gate when the built-in is on).
    fn check(&self) -> Result<(), BoxError> {
        let mut seen: HashMap<&str, &str> = HashMap::new();
        for (name, source) in &self.from {
            if name == AskOwner::TOOL && source != AskOwner::TOOL {
                return Err(format!(
                    "tool ask_owner from {source}: the name is yobuild's ([builtins] ask_owner)"
                )
                .into());
            }
            if let Some(first) = seen.insert(name, source) {
                return Err(format!("two tools named {name}: from {first} and {source}").into());
            }
        }
        Ok(())
    }
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
    interrupted: bool,
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
    if interrupted {
        (outcome, error) = (
            Some(RunOutcome::Aborted),
            Some("stopped with Ctrl+C".into()),
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
        missing = probe_services(host, &plan.dsh_dir, &plan.dsh_modules, wait).await?;
        if missing.is_empty() {
            return Ok(());
        }
    }
    Err(dsh_missing(&missing, plan, host_services))
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

/// One look by `adapters/dsh/probe-services.ts` after `wait_ms`: the
/// services missing; an error for modules that cannot be imported, an
/// answer not in its shape, or a probe that fails or does not answer. The
/// probe is unloaded however it went.
async fn probe_services(
    host: &mut PluginHost,
    base: &Path,
    modules: &[String],
    wait_ms: u64,
) -> Result<Vec<String>, BoxError> {
    let out = tempfile::NamedTempFile::new()?.into_temp_path();
    std::fs::remove_file(&out)?;
    let id = "yobuild:dsh-services";
    host.load([Row::new(id, base.join("probe-services.ts").display())
        .runtime("dsh")
        .config(serde_json::json!({
            "modules": modules,
            "base": base,
            "out": &*out,
            "waitMs": wait_ms,
        }))])
    .await?;
    let deadline = Instant::now() + Duration::from_millis(wait_ms) + Duration::from_secs(15);
    let answer = loop {
        if let Ok(text) = std::fs::read_to_string(&out) {
            // Written whole once: an unreadable answer is a broken one.
            break Ok(text);
        }
        let failures = host.stopped(&[]);
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
    host.unload(id).await?;
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
    },
    Http {
        url: String,
        headers: Vec<(String, String)>,
    },
}

fn launch(
    server: &Mcp,
    installed: &install::Installed,
    lookup: &dyn Fn(&str) -> Option<String>,
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

/// An MCP server's tools, each `<server>__<tool>` (the server is called with
/// its own names); those its `allow` names must be among them.
async fn mcp(server: &Mcp, launch: &Launch) -> Result<Vec<Box<dyn AgentTool>>, BoxError> {
    let failed = |e: &dyn std::fmt::Display| format!("mcp {}: {e}", server.name);
    let client = match launch {
        Launch::Stdio { command, args, env } => {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            McpClient::connect_stdio(&command.display().to_string(), &args, Some(env.clone()))
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
}
