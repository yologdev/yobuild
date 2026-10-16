//! `yobuild run`: an agent from its manifest — the model, its limits, the
//! parts, the plugins (by ecosystem), MCP servers, and a frontend.
//!
//! In three steps: every check first, before anything starts (a missing key
//! or package, a prompt that cannot be filled, a plugin yobuild cannot load
//! whole); then the runtimes, servers and tools (refusing two tools of one
//! name); then the frontend.
//!
//! Until yobuild installs packages (M2b), a pinned plugin or server must
//! already be installed, at exactly its version: pi packages, stdio MCP
//! servers and the rutis runtime in `node_modules/` beside the manifest, DSH
//! plugins in the DSH runtime's (`adapters/dsh`).
//!
//! The agent works in the process's working folder: yoagent's file tools
//! read relative paths from there, so the gate, facts and bash do too.

pub(crate) mod plugins;
pub(crate) mod script;

use std::collections::HashMap;
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

use crate::frontend::host::{DshHome, PluginHost, Row};
use crate::frontend::{
    services, web, ClientMessage, NoticeLevel, RunOutcome, ServerMessage, Session,
};
use crate::manifest::{Gate as GateMode, Manifest, Mcp, Source, Transport};
use crate::parts::{AskOwner, Facts, Gate, Scratch};

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
    /// Where the web frontend listens, when the frontend is the web page
    /// (default `127.0.0.1:8787`; with `phone`, `0.0.0.0:8787`).
    pub addr: Option<SocketAddr>,
    pub layout: Layout,
}

impl Options {
    pub fn new(manifest: Manifest) -> Self {
        Self {
            manifest,
            scripted: None,
            task: None,
            once: false,
            addr: None,
            layout: Layout::default(),
        }
    }
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
        addr,
        layout,
        ..
    } = options;
    let name = &manifest.agent.name;
    for note in &checked.notes {
        eprintln!("{name}: note: {note}");
    }
    let terminal = manifest.frontend.terminal && !once;

    // A terminal UI ends the session when it quits, as does a `once` run.
    let (session, driver) = Session::new(terminal || once);
    let bridge = RutisBridge::install(root)?;
    services::provide(root, &session)?;
    let scratch = match manifest.parts.scratch {
        true => Some(Scratch::create(name)?),
        false => None,
    };

    // The runtimes and their plugins.
    let plan = &checked.plan;
    let mut builder = PluginHost::builder();
    if terminal {
        // The terminal UI reads the keyboard: its runtime keeps the terminal.
        builder = builder
            .node("ui", layout.frontend.join("plugins"))
            .terminal("ui");
    }
    if plan.pi {
        builder = builder.node("pi", layout.adapters.join("pi"));
    }
    // DSH's own host services, on request: in a home of their own.
    let dsh_home = match plan.dsh && manifest.dsh.host_services {
        true => Some(DshHome::new()?),
        false => None,
    };
    if plan.dsh {
        builder = builder.node("dsh", layout.adapters.join("dsh"));
        for (name, value) in dsh_home.iter().flat_map(DshHome::env) {
            builder = builder.env("dsh", name, value);
        }
    }
    if plan.rutis {
        builder = builder.node("rutis", &manifest.dir);
    }
    let mut host = builder
        .share("yoagent")
        .share(services::FRONTEND)
        .share(services::UI)
        .start(root)
        .await?;
    if !plan.rows.is_empty() {
        host.load(plan.rows.clone()).await?;
        wait_for(&bridge, &plan.handlers, &host).await?;
    }
    if !plan.dsh_modules.is_empty() {
        dsh_services(&mut host, &layout, plan, manifest.dsh.host_services).await?;
    }

    // The tools: yoagent's (in `cwd`), `ask_owner`, each MCP server's; no
    // two of one name, a plugin's included.
    let ask_owner = manifest
        .parts
        .ask_owner
        .then(|| AskOwner::new(session.clone()));
    let mut tools = Tools::default();
    tools.add("yoagent", tools_in(&cwd));
    if let Some(ask) = &ask_owner {
        tools.add("ask_owner", vec![Box::new(ask.clone())]);
    }
    for server in &manifest.mcp {
        let found = mcp(server, &checked.servers[&server.name]).await?;
        tools.add(&format!("mcp {}", server.name), found);
    }
    tools.check(&bridge.registry().tool_names())?;

    let prompt = checked.prompt.fill(&cwd, scratch.as_ref());
    let mut agent = match checked.provider {
        Some(provider) => Agent::from_provider(provider, checked.config.clone()),
        None => {
            Agent::from_config(checked.config.clone()).with_api_key(checked.key.unwrap_or_default())
        }
    }
    .with_system_prompt(prompt)
    .with_tools(tools.list)
    .with_execution_limits(limits(&manifest))
    .with_extension(bridge.extension());
    if manifest.parts.gate == GateMode::AllowList {
        let mut gate = Gate::new(session.clone(), &cwd);
        if let Some(scratch) = &scratch {
            gate = gate.with_scratch(scratch);
        }
        agent = agent.with_extension(gate);
    }
    if let Some(report) = &manifest.parts.report {
        agent = agent.with_extension(report.clone());
    }
    if manifest.parts.facts {
        let mut facts = Facts::new(session.clone(), &cwd);
        if let Some(ask) = &ask_owner {
            facts = facts.with_open_questions(ask.open_questions());
        }
        agent = agent.with_extension(facts);
    }
    if let Some(budget) = checked.budget {
        agent = agent.with_extension(budget);
    }
    eprintln!("{name}: ready in {:.1}s", started.elapsed().as_secs_f64());

    // What the frontends see; in `once`, written out as it comes.
    let (seen, mut ended_runs, watching) = watch(&session, once);
    if once {
        session.send(ClientMessage::Prompt {
            text: task.unwrap_or_default(),
        });
        let running = tokio::spawn(driver.run(agent));
        // The task's run ends (or the session does, without it).
        while *ended_runs.borrow_and_update() == 0 && !seen.lock().unwrap().closed {
            if ended_runs.changed().await.is_err() {
                break;
            }
        }
        session.send(ClientMessage::Quit);
        let agent = running.await?;
        let mut ended = finish(seen, watching, Some(agent)).await;
        if ended.outcome.is_none() {
            ended.outcome = Some(RunOutcome::Error);
            ended.error = Some("the session ended before the run did".into());
        }
        return Ok(ended);
    }
    if terminal {
        let ui = "terminal-ui";
        host.load([
            Row::new(ui, layout.frontend.join("plugins/terminal-ui.ts").display()).runtime("ui"),
        ])
        .await?;
        if let Some(task) = task {
            session.send(ClientMessage::Prompt { text: task });
        }
        let driving = driver.run(agent);
        tokio::pin!(driving);
        // The terminal UI ends the session when it quits; one whose runtime
        // stopped would leave it waiting with nobody there.
        let agent = loop {
            tokio::select! {
                agent = &mut driving => break agent,
                _ = tokio::time::sleep(Duration::from_millis(250)) => {
                    if !host.is_running(ui) {
                        session.send(ClientMessage::Quit);
                        drop(driving.await);
                        drop(finish(seen, watching, None).await);
                        return Err(format!("the terminal UI stopped: {:?}", host.status()).into());
                    }
                }
            }
        };
        return Ok(finish(seen, watching, Some(agent)).await);
    }
    let phone = manifest.frontend.phone;
    let addr = addr.unwrap_or_else(|| match phone {
        true => "0.0.0.0:8787".parse().expect("an address"),
        false => "127.0.0.1:8787".parse().expect("an address"),
    });
    let served = web::serve(session.clone(), addr).await?;
    // Every interface: a link the phone can open.
    let url = match (served.addr().ip().is_unspecified(), lan_ip()) {
        (true, Some(ip)) => format!("http://{ip}:{}/?t={}", served.addr().port(), served.token()),
        _ => served.url(),
    };
    println!("{name}: {url}  (Ctrl+C to stop)");
    if let Some(task) = task {
        session.send(ClientMessage::Prompt { text: task });
    }
    // Whether a run was going is read as Ctrl+C comes: `select!` drops the
    // driver (ending that run as an error) before its branch runs.
    let interrupt = async {
        let stop = tokio::signal::ctrl_c().await;
        let mut seen = seen.lock().unwrap();
        seen.interrupted = seen.running;
        stop
    };
    let agent = tokio::select! {
        agent = driver.run(agent) => Some(agent),
        stop = interrupt => {
            if let Err(e) = stop {
                eprintln!("cannot listen for Ctrl+C ({e}); stopping");
            }
            None
        }
    };
    served.stop();
    Ok(finish(seen, watching, agent).await)
}

/// What `run` needs that can be checked before anything starts.
struct Checked {
    key: Option<String>,
    config: ModelConfig,
    /// `--scripted`'s replies.
    provider: Option<MockProvider>,
    budget: Option<Budget>,
    prompt: Prompt,
    plan: plugins::Plan,
    servers: HashMap<String, Launch>,
    notes: Vec<String>,
}

fn check(options: &Options, cwd: &Path) -> Result<Checked, BoxError> {
    let manifest = &options.manifest;
    let lookup = |name: &str| std::env::var(name).ok();
    if options.once && options.task.as_deref().is_none_or(|t| t.trim().is_empty()) {
        return Err("a once run needs a task".into());
    }
    let mut notes = Vec::new();

    // The model: a missing key fails here.
    let model = &manifest.agent.model;
    let (key, provider) = match &options.scripted {
        Some(path) => (None, Some(script::provider(path)?)),
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
    if manifest.limits.budget_usd.is_some() && budget.is_none() {
        notes.push("budget_usd is not kept: a scripted model has no price".into());
    }

    let prompt = Prompt::read(manifest)?;
    let plan = plugins::plan(manifest, &options.layout, cwd, &lookup)?;
    notes.extend(plan.notes.iter().cloned());
    let servers = manifest
        .mcp
        .iter()
        .map(|server| Ok((server.name.clone(), launch(server, &manifest.dir, &lookup)?)))
        .collect::<Result<_, BoxError>>()?;
    Ok(Checked {
        key,
        config,
        provider,
        budget,
        prompt,
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
                if text.contains("{scratch}") && !manifest.parts.scratch {
                    return Err(format!(
                        "prompt {}: {{scratch}}, but [parts] scratch is off",
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
        if let Some(report) = &manifest.parts.report {
            prompt.text.push_str("\n\n");
            prompt.text.push_str(&report.instruction());
        }
        Ok(prompt)
    }

    fn fill(&self, cwd: &Path, scratch: Option<&Scratch>) -> String {
        let cwd = cwd.display().to_string();
        match &self.plain {
            Some(name) => format!("You are {name}, an agent working in {cwd}.{}", self.text),
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

    /// No two tools of one name — the plugins' (`plugin`) included — and
    /// none named `ask_owner` but yobuild's: the gate lets that one run
    /// without asking.
    fn check(&self, plugin: &[String]) -> Result<(), BoxError> {
        let plugin = plugin
            .iter()
            .map(|name| (name.clone(), "a plugin".to_owned()));
        let mut seen: HashMap<String, String> = HashMap::new();
        for (name, source) in self.from.iter().cloned().chain(plugin) {
            if name == "ask_owner" && source != "ask_owner" {
                return Err(format!(
                    "tool ask_owner from {source}: the name is yobuild's ([parts] ask_owner)"
                )
                .into());
            }
            if let Some(first) = seen.get(&name) {
                return Err(format!("two tools named {name}: from {first} and {source}").into());
            }
            seen.insert(name, source);
        }
        Ok(())
    }
}

/// yoagent's coding tools; bash and search start in `cwd` (the file tools
/// read relative paths from the process's working folder, which is `cwd`).
fn tools_in(cwd: &Path) -> Vec<Box<dyn AgentTool>> {
    let cwd = cwd.display().to_string();
    vec![
        Box::new(BashTool::default().with_cwd(cwd.clone())),
        Box::new(ReadFileTool::default()),
        Box::new(WriteFileTool::new()),
        Box::new(EditFileTool::new()),
        Box::new(ListFilesTool::default()),
        Box::new(SearchTool::default().with_root(cwd)),
    ]
}

/// What the frontends saw, as an observer sees it (a question asked with
/// only observers there gets its default answer at once).
#[derive(Default)]
struct Seen {
    /// A run started and has not ended.
    running: bool,
    /// Ctrl+C stopped the session during a run.
    interrupted: bool,
    last: Option<(RunOutcome, Option<String>)>,
    notices: Vec<(NoticeLevel, String)>,
    closed: bool,
}

type Shared = Arc<std::sync::Mutex<Seen>>;

/// Watches the session: how many runs ended (`ended`), and what was seen;
/// with `print`, each tool call and notice on stderr. Ends with the
/// session.
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
    let task = tokio::spawn(async move {
        loop {
            let message = watching.messages.recv().await;
            let mut seen = record.lock().unwrap();
            match message {
                Some(ServerMessage::RunStart { .. }) => seen.running = true,
                Some(ServerMessage::RunEnd { outcome, error, .. }) => {
                    seen.running = false;
                    seen.last = Some((outcome, error));
                    ended.send_modify(|n| *n += 1);
                }
                Some(ServerMessage::Event { event, .. }) if print => {
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
                        let first = text.lines().next().unwrap_or_default();
                        match is_error {
                            true => eprintln!("  {tool_name} ✗ {first}"),
                            false => eprintln!("  {tool_name} ✓"),
                        }
                    }
                }
                Some(ServerMessage::Notice { level, message }) => {
                    if print {
                        eprintln!("  {level:?}: {message}");
                    }
                    seen.notices.push((level, message));
                }
                Some(ServerMessage::Closed) | None => {
                    seen.closed = true;
                    // Wakes a `once` run waiting for its run's end.
                    ended.send_modify(|_| {});
                    return;
                }
                Some(_) => {}
            }
        }
    });
    (seen, ended_rx, task)
}

/// How the session ended, once the watch saw it close.
async fn finish(
    seen: Shared,
    watching: tokio::task::JoinHandle<()>,
    agent: Option<Agent>,
) -> Ended {
    // The session closes once its driver is gone; the watch then ends.
    let _ = tokio::time::timeout(Duration::from_secs(5), watching).await;
    let mut seen = std::mem::take(&mut *seen.lock().unwrap());
    // Its run then ends as an error (the driver is gone): it was stopped.
    if seen.interrupted {
        seen.last = Some((RunOutcome::Aborted, Some("stopped with Ctrl+C".into())));
    }
    let (outcome, error) = seen
        .last
        .map_or((None, None), |(outcome, error)| (Some(outcome), error));
    Ended {
        outcome,
        error,
        notices: seen.notices,
        agent,
    }
}

/// Whether the services the DSH plugins' rows inject (`modules`) exist in
/// the DSH runtime: a plugin missing one never starts, while its row reads
/// as started (the waiting is inside DSH's own loader) — so ask from inside,
/// with the adapters' service probe.
async fn dsh_services(
    host: &mut PluginHost,
    layout: &Layout,
    plan: &plugins::Plan,
    host_services: bool,
) -> Result<(), BoxError> {
    let modules = &plan.dsh_modules;
    let base = layout.adapters.join("dsh");
    let out = tempfile::NamedTempFile::new()?.into_temp_path();
    std::fs::remove_file(&out)?;
    let id = "yobuild:dsh-services";
    host.load([Row::new(id, base.join("probe-services.ts").display())
        .runtime("dsh")
        .config(serde_json::json!({
            "modules": modules,
            "base": base,
            "out": &*out,
            "waitMs": 1000,
        }))])
    .await?;
    let deadline = Instant::now() + Duration::from_secs(15);
    let found = loop {
        if let Ok(text) = std::fs::read_to_string(&out) {
            if let Ok(found) = serde_json::from_str::<serde_json::Value>(&text) {
                break found;
            }
        }
        if Instant::now() >= deadline {
            return Err("the DSH service check gave no answer in 15 s".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    host.unload(id).await?;
    if let Some(errors) = found["importErrors"].as_object().filter(|e| !e.is_empty()) {
        return Err(format!(
            "DSH plugins that cannot be imported: {}",
            serde_json::Value::Object(errors.clone())
        )
        .into());
    }
    let missing: Vec<&str> = found["missing"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m.as_str())
        .collect();
    if !missing.is_empty() {
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
        return Err(format!(
            "DSH plugins wait for services nobody provides: {}{hints}",
            missing.join(", ")
        )
        .into());
    }
    Ok(())
}

/// Waits until the plugins' `handlers` are registered; a plugin that failed
/// meanwhile fails it at once.
async fn wait_for(
    bridge: &RutisBridge,
    handlers: &[&str],
    host: &PluginHost,
) -> Result<(), BoxError> {
    let registry = bridge.registry().clone();
    let missing = || -> Vec<&str> {
        let have = registry.handlers();
        handlers
            .iter()
            .copied()
            .filter(|name| !have.iter().any(|h| h.name() == *name))
            .collect()
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let failures = host.failures();
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
    dir: &Path,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<Launch, BoxError> {
    let failed =
        |e: &dyn std::fmt::Display| -> BoxError { format!("mcp {}: {e}", server.name).into() };
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
                    let install = format!("npm install {} in {}", package.spec(), dir.display());
                    let installed =
                        plugins::installed(package, &dir.join("node_modules"), &install)
                            .map_err(|e| failed(&e))?;
                    bin_of(&installed, package.name(), bin.as_deref()).map_err(|e| failed(&e))?
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

/// An MCP server's tools (named as the server names them).
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
    let tools = McpToolAdapter::from_client(client)
        .await
        .map_err(|e| failed(&e))?;
    if tools.is_empty() {
        return Err(failed(&"the server lists no tools").into());
    }
    Ok(tools
        .into_iter()
        .map(|t| Box::new(t) as Box<dyn AgentTool>)
        .collect())
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
    use super::*;

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
            "prompt = \"./p.md\"\n[parts]\nscratch = true\nreport = [\"Done\"]\n",
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
            .ends_with("{scratch}, but [parts] scratch is off"));
        let plain = Prompt::read(&manifest(dir.path(), ""))
            .unwrap()
            .fill(Path::new("/w"), None);
        assert_eq!(plain, "You are a, an agent working in /w.");
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
        tools.add("yoagent", tools_in(dir.path()));
        assert!(tools.check(&["todo".into()]).is_ok());
        let e = tools.check(&["bash".into()]).unwrap_err().to_string();
        assert_eq!(e, "two tools named bash: from yoagent and a plugin");
        let e = tools.check(&["ask_owner".into()]).unwrap_err().to_string();
        assert!(e.starts_with("tool ask_owner from a plugin"), "{e}");
        let e = tools
            .check(&["todo".into(), "todo".into()])
            .unwrap_err()
            .to_string();
        assert_eq!(e, "two tools named todo: from a plugin and a plugin");
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
