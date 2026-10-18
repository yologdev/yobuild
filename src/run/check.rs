//! `yobuild check`: every plugin and MCP server of a manifest vetted
//! without a model (design §4, "Vet"). In three steps: read and install as
//! `run` does; each plugin alone — pi and DSH through yoagent-rutis's
//! `probe` (the compatibility matrix's verdict), a rutis plugin in its own
//! runtime, an MCP server asked for its tools; then, when none had a
//! problem, the agent together through `run`'s own start, and one run's
//! hooks called directly for the tools it would be offered. Loading runs
//! the plugins' code, as `run` would: not a sandbox.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rutis::Ctx;
use serde::Serialize;
use tokio_util::sync::CancellationToken;
use yoagent::extension::{RunContext, RunEnd, RunOutcome};
use yoagent::{Extension, Message, TurnContext};
use yoagent_rutis::RutisBridge;

use super::{
    install, mcp, offerer, offerers, plugins, prepare, start, tools_for, Adapters, Layout,
    Prepared, Started,
};
use crate::builtins::{AskOwner, READ_ONLY_TOOLS};
use crate::frontend::host::PluginHost;
use crate::frontend::{services, Session};
use crate::install::Set;
use crate::manifest::{Gate as GateMode, Manifest, PluginKind, Source, Transport};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The packages yoagent-rutis's DSH probe loads as its base besides the
/// host services (`dsh_base_rows`): a plugin made of one of them is loaded
/// twice when probed alone, which DSH refuses, so it is checked together
/// only.
const DSH_PROBE_BASE: [&str; 4] = [
    "@deepseek-ai/dsh-web",
    "@deepseek-ai/dsh-system-prompt",
    "@deepseek-ai/dsh-tools",
    "@deepseek-ai/dsh-user-questions",
];

/// How long a rutis plugin alone gets to register its handlers once its row
/// runs (a TypeScript row is active before its async `apply` ends).
const SETTLE: Duration = Duration::from_secs(3);

/// What to check.
#[derive(Debug)]
#[non_exhaustive]
pub struct CheckOptions {
    pub manifest: Manifest,
    pub layout: Layout,
    /// Refuse a lock that is not current (as `run --locked`).
    pub locked: bool,
    /// yobuild's home (`None`: `YOBUILD_HOME`, else `~/.yobuild`).
    pub home: Option<PathBuf>,
}

impl CheckOptions {
    pub fn new(manifest: Manifest) -> Self {
        Self {
            manifest,
            layout: Layout::default(),
            locked: false,
            home: None,
        }
    }
}

/// How the gate treats a tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ToolKind {
    /// Read-only: never asks.
    Free,
    /// Judged call by call (bash, the file writers).
    Judged,
    /// Allowed by an entry's `allow` (or the built-in's).
    Allowed,
    /// Asks the owner.
    Asks,
    /// `gate = "off"`: runs as the agent asks.
    Runs,
}

impl ToolKind {
    fn word(self) -> &'static str {
        match self {
            ToolKind::Free => "free",
            ToolKind::Judged => "judged per call",
            ToolKind::Allowed => "allowed",
            ToolKind::Asks => "asks",
            ToolKind::Runs => "runs",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Tool {
    pub name: String,
    pub kind: ToolKind,
}

/// One plugin or MCP server, alone.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct Entry {
    /// `plugin` or `mcp`.
    pub kind: &'static str,
    pub name: String,
    /// `pi`, `dsh`, `rutis`, or `stdio` / `http` for a server.
    pub ecosystem: &'static str,
    /// Its package spec, path or URL.
    pub source: String,
    /// What keeps it from giving the agent what it offers (`run` would
    /// refuse it, or it would not work).
    pub problems: Vec<String>,
    /// What is worth knowing and changes nothing (skills not loaded, …).
    pub notes: Vec<String>,
    pub tools: Vec<Tool>,
    /// A rutis plugin's handlers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handlers: Option<Vec<String>>,
    /// The prompt guidance a DSH plugin adds, in characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guidance_chars: Option<usize>,
    /// Its installed packages with install scripts, which were not run.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub install_scripts: Vec<String>,
}

/// The agent, together.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct Agent {
    pub name: String,
    /// Whether the agent was started together (only when no entry had a
    /// problem).
    pub started: bool,
    pub problems: Vec<String>,
    pub notes: Vec<String>,
    /// Every tool a run would be offered, and how the gate treats it.
    pub tools: Vec<Tool>,
    /// The DSH base's packages with install scripts, which were not run.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub install_scripts: Vec<String>,
}

/// What `check` found.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct Report {
    pub entries: Vec<Entry>,
    pub agent: Agent,
}

impl Report {
    /// No entry and not the agent has a problem.
    pub fn ok(&self) -> bool {
        self.agent.problems.is_empty() && self.entries.iter().all(|e| e.problems.is_empty())
    }

    /// The report as text: a block per entry, then the agent.
    pub fn text(&self) -> String {
        let mut out = String::new();
        let verdict = |problems: &[String]| match problems.len() {
            0 => "ok".to_owned(),
            1 => "1 problem".to_owned(),
            n => format!("{n} problems"),
        };
        let tools = |tools: &[Tool]| {
            tools
                .iter()
                .map(|t| format!("{} ({})", t.name, t.kind.word()))
                .collect::<Vec<_>>()
                .join(", ")
        };
        for entry in &self.entries {
            out.push_str(&format!(
                "{} {}  {} {}  {}\n",
                entry.kind,
                entry.name,
                entry.ecosystem,
                entry.source,
                verdict(&entry.problems)
            ));
            for problem in &entry.problems {
                out.push_str(&format!("  problem: {problem}\n"));
            }
            if !entry.tools.is_empty() {
                out.push_str(&format!("  tools: {}\n", tools(&entry.tools)));
            } else if entry.handlers.is_none()
                && entry.problems.is_empty()
                && !entry
                    .notes
                    .iter()
                    .any(|n| n.starts_with("not probed alone"))
            {
                out.push_str("  tools: none\n");
            }
            if let Some(handlers) = &entry.handlers {
                match handlers.is_empty() {
                    true => out.push_str("  handlers: none (it offers no tool)\n"),
                    false => out.push_str(&format!("  handlers: {}\n", handlers.join(", "))),
                }
            }
            if let Some(chars) = entry.guidance_chars.filter(|c| *c > 0) {
                out.push_str(&format!("  prompt guidance: {chars} characters\n"));
            }
            for note in &entry.notes {
                out.push_str(&format!("  note: {note}\n"));
            }
            if !entry.install_scripts.is_empty() {
                out.push_str(&format!(
                    "  install scripts not run: {}\n",
                    entry.install_scripts.join(", ")
                ));
            }
        }
        let agent = &self.agent;
        let verdict = match (agent.started, self.ok()) {
            (_, true) => "ok".to_owned(),
            (false, false) if agent.problems.is_empty() => {
                "not started together: an entry has a problem".to_owned()
            }
            _ => verdict(&agent.problems),
        };
        out.push_str(&format!("agent {}: {verdict}\n", agent.name));
        for problem in &agent.problems {
            out.push_str(&format!("  problem: {problem}\n"));
        }
        if !agent.tools.is_empty() {
            out.push_str(&format!("  tools: {}\n", tools(&agent.tools)));
        }
        for note in &agent.notes {
            out.push_str(&format!("  note: {note}\n"));
        }
        if !agent.install_scripts.is_empty() {
            out.push_str(&format!(
                "  install scripts not run (the DSH base): {}\n",
                agent.install_scripts.join(", ")
            ));
        }
        out
    }
}

/// Checks the manifest's plugins and servers (no model, no key). An error
/// is what stops `run` before anything starts too (the manifest's install,
/// a plugin that cannot be planned, a server that cannot be launched).
pub async fn check(options: CheckOptions) -> Result<Report, BoxError> {
    let cwd = std::env::current_dir()?;
    let CheckOptions {
        manifest,
        layout,
        locked,
        home,
    } = options;
    let Prepared {
        installed,
        plan,
        servers,
        notes,
    } = prepare(&manifest, &layout, home.as_deref(), locked, &cwd)?;
    let scripts = install::install_scripts(&manifest, &layout.adapters)?;
    let gate_off = manifest.builtins.gate == GateMode::Off;
    let kind_of = |allowed: bool| match (gate_off, allowed) {
        (true, _) => ToolKind::Runs,
        (false, true) => ToolKind::Allowed,
        (false, false) => ToolKind::Asks,
    };
    // Notes naming a plugin go to its entry; the rest to the agent.
    let mut agent_notes: Vec<String> = Vec::new();
    let mut entry_notes: Vec<(String, String)> = Vec::new();
    for note in notes.iter().chain(installed.said()) {
        match manifest
            .plugins
            .iter()
            .find(|p| note.starts_with(&format!("plugin {}: ", p.name)))
        {
            Some(p) => entry_notes.push((p.name.clone(), note.clone())),
            None => agent_notes.push(note.clone()),
        }
    }

    // Each plugin alone.
    let session = Session::new(false).0;
    let mut entries = Vec::new();
    for plugin in &manifest.plugins {
        let (ecosystem, source) = match &plugin.kind {
            PluginKind::Pi(source) => ("pi", source_text(source)),
            PluginKind::Dsh(source) => ("dsh", source_text(source)),
            PluginKind::Rutis { path, .. } => ("rutis", path.display().to_string()),
        };
        let mut entry = Entry {
            kind: "plugin",
            name: plugin.name.clone(),
            ecosystem,
            source,
            problems: Vec::new(),
            notes: entry_notes
                .iter()
                .filter(|(name, _)| *name == plugin.name)
                .map(|(_, note)| note.clone())
                .collect(),
            tools: Vec::new(),
            handlers: None,
            guidance_chars: None,
            install_scripts: Vec::new(),
        };
        let names = match &plugin.kind {
            PluginKind::Pi(source) => {
                if let Source::Package(p) = source {
                    entry.install_scripts = scripts_of(&scripts, Set::Pi, p.name());
                }
                let files: Vec<String> = plan
                    .pi_files
                    .iter()
                    .filter(|(name, _)| *name == plugin.name)
                    .flat_map(|(_, files)| files.iter().map(|f| f.display().to_string()))
                    .collect();
                let report =
                    yoagent_rutis::probe::pi(&layout.adapters, &cwd, &files, &[], None).await;
                entry.problems.extend(report.problem());
                for builtin in &report.without {
                    entry.problems.push(format!(
                        "its tool `{builtin}` is named like yoagent's built-in: `run` refuses it"
                    ));
                }
                report.tools
            }
            PluginKind::Dsh(source) => {
                if let Source::Package(p) = source {
                    entry.install_scripts = scripts_of(&scripts, Set::Dsh, p.name());
                }
                let ids = plugins::dsh_plugin_rows(&plan, &plugin.name);
                let rows: Vec<serde_json::Value> = plan
                    .rows
                    .iter()
                    .filter(|row| ids.iter().any(|id| id == row.id()))
                    // As `run` loads them: the row's config, `{}` by default.
                    .map(|row| {
                        serde_json::json!({ "id": row.id(), "name": row.name(), "config": row.config_json() })
                    })
                    .collect();
                // A row the probe's base loads too: checked together only.
                let mut base: Vec<String> = DSH_PROBE_BASE.iter().map(|s| s.to_string()).collect();
                if let Ok(rows) = crate::frontend::host::rows_from_file(
                    &layout.adapters.join("dsh/host-services.json"),
                    "dsh",
                ) {
                    base.extend(rows.iter().map(|row| row.name().to_owned()));
                }
                let in_base: Vec<&str> = rows
                    .iter()
                    .filter_map(|row| row["name"].as_str())
                    .filter(|name| base.iter().any(|b| b == name))
                    .collect();
                if !in_base.is_empty() {
                    entry.notes.push(format!(
                        "not probed alone: the probe's DSH base loads {} itself; checked with the agent together",
                        in_base.join(", ")
                    ));
                    entry.tools = Vec::new();
                    entries.push(entry);
                    continue;
                }
                let report = yoagent_rutis::probe::dsh(
                    &layout.adapters,
                    &plan.dsh_dir,
                    &serde_json::Value::Array(rows),
                    &[],
                    None,
                )
                .await;
                entry.problems.extend(report.problem());
                entry.guidance_chars = Some(report.guidance_chars);
                report.tools
            }
            PluginKind::Rutis { .. } => {
                match rutis_alone(&manifest, &plan, &session, &plugin.name).await {
                    Ok((handlers, tools)) => {
                        entry.handlers = Some(handlers);
                        tools
                    }
                    Err(e) => {
                        entry.problems.push(e.to_string());
                        Vec::new()
                    }
                }
            }
        };
        entry.tools = names
            .into_iter()
            .map(|name| {
                let allowed = plugin.allow.contains(&name);
                Tool {
                    kind: kind_of(allowed),
                    name,
                }
            })
            .collect();
        entries.push(entry);
    }
    // Each MCP server: its tools, none called.
    let mut server_tools: Vec<String> = Vec::new();
    for server in &manifest.mcp {
        let (ecosystem, source) = match &server.transport {
            Transport::Stdio { source, .. } => ("stdio", source_text(source)),
            Transport::Http { url, .. } => ("http", url.as_str().to_owned()),
        };
        let mut entry = Entry {
            kind: "mcp",
            name: server.name.clone(),
            ecosystem,
            source,
            problems: Vec::new(),
            notes: Vec::new(),
            tools: Vec::new(),
            handlers: None,
            guidance_chars: None,
            install_scripts: Vec::new(),
        };
        if let Transport::Stdio {
            source: Source::Package(p),
            ..
        } = &server.transport
        {
            entry.install_scripts = scripts_of(&scripts, Set::Mcp, p.name());
        }
        match mcp(server, &servers[&server.name]).await {
            Ok(tools) => {
                let prefix = format!("{}__", server.name);
                for tool in tools {
                    let name = tool.name().to_owned();
                    let own = name.strip_prefix(&prefix).unwrap_or(&name);
                    let allowed = server.allow.iter().any(|a| a == own);
                    server_tools.push(name.clone());
                    entry.tools.push(Tool {
                        name,
                        kind: kind_of(allowed),
                    });
                }
            }
            Err(e) => entry.problems.push(e.to_string()),
        }
        entries.push(entry);
    }

    let mut agent = Agent {
        name: manifest.agent.name.clone(),
        notes: agent_notes,
        install_scripts: scripts.base.clone(),
        ..Agent::default()
    };
    if entries.iter().all(|e| e.problems.is_empty()) {
        together(
            &manifest, &plan, &layout, &session, &cwd, &server_tools, &entries, &mut agent,
        )
        .await;
    }
    Ok(Report { entries, agent })
}

fn source_text(source: &Source) -> String {
    match source {
        Source::Package(p) => p.spec(),
        Source::Path(path) => path.display().to_string(),
    }
}

fn scripts_of(scripts: &install::InstallScripts, set: Set, pin: &str) -> Vec<String> {
    scripts
        .by_pin
        .get(&(set, pin.to_owned()))
        .cloned()
        .unwrap_or_default()
}

/// The tools one run is offered, each with who offered it (read before
/// the run finishes: the registry forgets a run's offers then): the
/// bridge's hooks called directly (`start_run`, `tools`, one
/// `before_model`, `finish`), as the probe does — no model asked.
async fn offered(
    bridge: &RutisBridge,
) -> Result<Vec<(String, Option<yoagent_rutis::ToolOwner>)>, BoxError> {
    let cancel = CancellationToken::new();
    let prompt = [Message::user("check")];
    let run_id = "yobuild-check";
    let run = RunContext::new(run_id, &prompt, &cancel);
    let mut hooks = bridge.extension().start_run(&run).await?;
    let names: Vec<String> = hooks
        .tools(&run)
        .await
        .iter()
        .map(|t| t.name().to_owned())
        .collect();
    let turn = TurnContext::new("", &prompt, &[], run_id).with_run_prompts(&prompt);
    let _ = hooks.before_model(&turn).await;
    let owned = names
        .into_iter()
        .map(|name| {
            let owner = bridge.registry().offered_by(run_id, &name);
            (name, owner)
        })
        .collect();
    hooks
        .finish(&RunOutcome::new(RunEnd::Completed, None))
        .await;
    Ok(owned)
}

/// A root of its own with yobuild's services on it (`run`'s), for a host
/// started apart.
fn services_on(root: &Ctx, session: &Session, manifest: &Manifest) -> Result<(), BoxError> {
    services::provide(root, session)?;
    let ask_owner = manifest
        .builtins
        .ask_owner
        .then(|| AskOwner::new(session.clone()));
    services::provide_ask_owner(root, ask_owner)?;
    Ok(())
}

/// A rutis plugin alone, in its own runtime as `run` loads it: the handlers
/// its row registers (given [`SETTLE`]) and the tools a run is offered.
async fn rutis_alone(
    manifest: &Manifest,
    plan: &plugins::Plan,
    session: &Session,
    name: &str,
) -> Result<(Vec<String>, Vec<String>), BoxError> {
    let id = plugins::row_id(name);
    let row = plan
        .rows
        .iter()
        .find(|row| row.id() == id)
        .ok_or_else(|| format!("no row {id}"))?
        .clone();
    let (runtime, dir) = plan
        .rutis
        .iter()
        .enumerate()
        .map(|(at, dir)| (plugins::rutis_runtime_name(at), dir))
        .find(|(runtime, _)| row.runtime_is(runtime))
        .ok_or_else(|| format!("no runtime for {id}"))?;
    let root = Ctx::root()?;
    let result = async {
        let bridge = RutisBridge::install(&root)?;
        services_on(&root, session, manifest)?;
        let mut host = PluginHost::builder()
            .node(&runtime, dir)
            .share("yoagent")
            .share(services::FRONTEND)
            .share(services::UI)
            .share(services::ASK_OWNER)
            .start(&root)
            .await?;
        host.load([row]).await?;
        let mine = |bridge: &RutisBridge| -> Vec<String> {
            let mut names: Vec<String> = bridge
                .registry()
                .handlers()
                .into_iter()
                .filter(|h| h.owner() == id)
                .map(|h| h.name().to_owned())
                .collect();
            names.sort();
            names
        };
        let deadline = Instant::now() + SETTLE;
        while mine(&bridge).is_empty() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let handlers = mine(&bridge);
        let tools = offered(&bridge)
            .await?
            .into_iter()
            .filter(|(_, owner)| owner.as_ref().is_some_and(|o| o.owner() == id))
            .map(|(name, _)| name)
            .collect();
        drop(host);
        Ok::<_, BoxError>((handlers, tools))
    }
    .await;
    let _ = tokio::time::timeout(Duration::from_secs(10), root.shutdown()).await;
    result
}

/// The agent together, through `run`'s own start: a refusal there is a
/// problem; then the tools one run is offered, how the gate treats each,
/// and what `run` would only warn about (notes).
#[allow(clippy::too_many_arguments)]
async fn together(
    manifest: &Manifest,
    plan: &plugins::Plan,
    layout: &Layout,
    session: &Session,
    cwd: &Path,
    server_tools: &[String],
    entries: &[Entry],
    agent: &mut Agent,
) {
    let root = match Ctx::root() {
        Ok(root) => root,
        Err(e) => {
            agent.problems.push(e.to_string());
            return;
        }
    };
    let result = async {
        let Started {
            bridge,
            host,
            ask_owner: _,
            runtimes: _,
            dsh_home: _dsh_home,
        } = match start(&root, manifest, plan, layout, session, false).await {
            Ok(started) => started,
            Err(e) => {
                agent.started = false;
                agent
                    .problems
                    .push(format!("`run` would refuse it at start: {e}"));
                return Ok(());
            }
        };
        agent.started = true;
        // A rutis plugin that registered handlers alone registers them here
        // too, in its own time.
        let expected: Vec<String> = entries
            .iter()
            .filter(|e| e.handlers.as_ref().is_some_and(|h| !h.is_empty()))
            .map(|e| plugins::row_id(&e.name))
            .collect();
        let deadline = Instant::now() + SETTLE;
        loop {
            let owners: HashSet<String> = bridge
                .registry()
                .handlers()
                .iter()
                .map(|h| h.owner().to_owned())
                .collect();
            if expected.iter().all(|row| owners.contains(row)) || Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let offered = offered(&bridge).await?;
        // The agent's own tools: yoagent's, ask_owner, the servers'.
        let mut own: Vec<String> = tools_for(manifest.builtins.tools, cwd)
            .iter()
            .map(|t| t.name().to_owned())
            .collect();
        let yoagents = own.clone();
        if manifest.builtins.ask_owner {
            own.push(AskOwner::TOOL.to_owned());
        }
        own.extend(server_tools.iter().cloned());
        let own_set: HashSet<String> = own.iter().cloned().collect();
        let adapters = Adapters {
            pi: plan.pi_handler.clone(),
            dsh: plan.dsh_handler.clone(),
        };
        // The gate's rule (`offered_check`), on the owners kept from the run.
        let offerers = offerers(manifest, plan);
        let allowed = |name: &str, owner: &Option<yoagent_rutis::ToolOwner>| {
            !own_set.contains(name)
                && owner.as_ref().is_some_and(|owner| {
                    offerers
                        .get(name)
                        .is_some_and(|all| all.iter().any(|o| o.offers(owner, &adapters)))
                })
        };
        let gate_off = manifest.builtins.gate == GateMode::Off;
        let mut tools = Vec::new();
        for name in &yoagents {
            let kind = match (gate_off, READ_ONLY_TOOLS.contains(&name.as_str())) {
                (true, _) => ToolKind::Runs,
                (false, true) => ToolKind::Free,
                (false, false) => ToolKind::Judged,
            };
            tools.push(Tool {
                name: name.clone(),
                kind,
            });
        }
        if manifest.builtins.ask_owner {
            tools.push(Tool {
                name: AskOwner::TOOL.to_owned(),
                kind: if gate_off { ToolKind::Runs } else { ToolKind::Allowed },
            });
        }
        for entry in entries.iter().filter(|e| e.kind == "mcp") {
            tools.extend(entry.tools.iter().cloned());
        }
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for (name, owner) in &offered {
            if !seen.insert(name) {
                agent.notes.push(format!(
                    "two plugins offer a tool named `{name}`: a run gets one of them"
                ));
                continue;
            }
            if own_set.contains(name) {
                agent.notes.push(format!(
                    "`{name}`, offered by {}, is named like the agent's own tool: yoagent keeps its own",
                    owner.as_ref().map_or("no plugin row", |o| o.owner())
                ));
                continue;
            }
            let kind = match gate_off {
                true => ToolKind::Runs,
                false if allowed(name, owner) => ToolKind::Allowed,
                false => ToolKind::Asks,
            };
            tools.push(Tool {
                name: name.clone(),
                kind,
            });
        }
        agent.tools = tools;
        // Each plugin's allowed names: offered by it, as the gate requires.
        for plugin in &manifest.plugins {
            let accepts = offerer(plugin, plan);
            for name in &plugin.allow {
                let owner = offered
                    .iter()
                    .find(|(offered, _)| offered == name)
                    .and_then(|(_, owner)| owner.as_ref());
                match owner {
                    None => agent.notes.push(format!(
                        "plugin {} allows `{name}`, which no plugin offered (a typo, or offered only in some runs)",
                        plugin.name
                    )),
                    Some(owner) if !accepts.offers(owner, &adapters) => agent.notes.push(format!(
                        "plugin {} allows `{name}`, but it was offered by `{}`, not by it: the tool asks",
                        plugin.name,
                        owner.owner()
                    )),
                    Some(_) => {}
                }
            }
        }
        drop(host);
        Ok::<_, BoxError>(())
    }
    .await;
    if let Err(e) = result {
        agent.problems.push(e.to_string());
    }
    let _ = tokio::time::timeout(Duration::from_secs(10), root.shutdown()).await;
}
