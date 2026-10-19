//! `yobuild check`: every plugin and MCP server of a manifest vetted
//! without a model (design §4, "Vet"). In three steps: read and install as
//! `run` does; each plugin alone — pi and DSH through yoagent-rutis's
//! `probe` (the compatibility matrix's verdict), a rutis plugin in its own
//! runtime, an MCP server asked for its tools; then, when none had a
//! problem, the agent together through `run`'s own start, and one run's
//! hooks called directly for the tools it would be offered. Loading runs
//! the plugins' code, as `run` would: not a sandbox.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rutis::Ctx;
use serde::Serialize;
use tokio_util::sync::CancellationToken;
use yoagent::extension::{RunContext, RunEnd, RunOutcome, TurnDecision};
use yoagent::{Extension, Message, TurnContext};
use yoagent_rutis::{RutisBridge, ToolOwner};

use super::{
    install, mcp, offerers, plugins, prepare, start, tools_for, Adapters, Layout, Prepared, Prompt,
    Started,
};
use crate::builtins::{AskOwner, JUDGED_TOOLS, READ_ONLY_TOOLS};
use crate::frontend::host::{rows_from_file, PluginHost};
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

/// How long a rutis plugin's handlers may take to appear once its row runs
/// (a TypeScript row is active before its async `apply` ends), alone and
/// together; the list counts as settled once it is unchanged for
/// [`STILL`].
const SETTLE: Duration = Duration::from_secs(3);
const STILL: Duration = Duration::from_millis(500);

/// The report's shape, for whoever reads `--json`.
const REPORT_VERSION: u32 = 2;

/// The run id `check` starts its one run with.
const RUN: &str = "yobuild-check";

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
    /// Judged call by call (`bash`, the writers and editors), by name,
    /// whoever offers it.
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
#[non_exhaustive]
pub struct Tool {
    pub name: String,
    pub kind: ToolKind,
    /// What the tool says it does (`--json` only; when the tool was seen).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Its arguments' JSON Schema, as the model is given it (`--json` only;
    /// for a scripted reply's `arguments`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
}

impl Tool {
    fn new(name: impl Into<String>, kind: ToolKind) -> Tool {
        Tool {
            name: name.into(),
            kind,
            description: None,
            parameters: None,
        }
    }
}

/// Each tool's description and schema, by name, from the tool objects seen.
type Schemas = HashMap<String, (String, serde_json::Value)>;

fn schema_of(tool: &dyn yoagent::AgentTool) -> (String, (String, serde_json::Value)) {
    (
        tool.name().to_owned(),
        (tool.description().to_owned(), tool.parameters_schema()),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum EntryKind {
    Plugin,
    Mcp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Ecosystem {
    Pi,
    Dsh,
    Rutis,
    /// An MCP server over stdio.
    Stdio,
    /// An MCP server over HTTP.
    Http,
}

impl Ecosystem {
    fn word(self) -> &'static str {
        match self {
            Ecosystem::Pi => "pi",
            Ecosystem::Dsh => "dsh",
            Ecosystem::Rutis => "rutis",
            Ecosystem::Stdio => "stdio",
            Ecosystem::Http => "http",
        }
    }
}

/// A problem, and what to do about it (from yobuild's table of fixes,
/// design §11), when known.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Problem {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

impl Problem {
    fn new(message: impl Into<String>) -> Problem {
        let message = message.into();
        let fix =
            crate::hints::hint(crate::hints::Scope::Start, &message).map(|h| h.fix.to_owned());
        Problem { message, fix }
    }
}

/// One plugin or MCP server, alone.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Entry {
    pub kind: EntryKind,
    pub name: String,
    pub ecosystem: Ecosystem,
    /// Its package spec, path or URL.
    pub source: String,
    /// Whether it was checked alone (a DSH plugin the probe's own base
    /// loads, or a rutis plugin waiting for a service another may provide,
    /// is checked together only).
    pub checked_alone: bool,
    /// What keeps it from giving the agent what it offers (`run` would
    /// refuse it, or it would not work), each with its fix when yobuild
    /// knows one.
    pub problems: Vec<Problem>,
    /// What is worth knowing and changes nothing (skills not loaded, …).
    pub notes: Vec<String>,
    /// Its tools as its entry names them: allowed when its `allow` lists
    /// them (whether the gate accepts who offered each is the agent's).
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
    /// What its host gives it beyond every plugin (the manifest's `grants`;
    /// design §12), as the manifest names them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub grants: Vec<String>,
}

/// Whether the agent was started together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Together {
    /// Not tried: an entry has a problem (a plugin that fails alone fails
    /// together too).
    #[default]
    Skipped,
    /// Tried, and `run` would refuse it at start (its problems say why).
    Refused,
    Started,
}

/// The agent, together.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Agent {
    pub name: String,
    pub together: Together,
    pub problems: Vec<Problem>,
    pub notes: Vec<String>,
    /// The tools a run gets and how the gate treats each: yoagent's,
    /// `ask_owner`, the servers', then the plugins' (one named like the
    /// agent's own is a note instead: yoagent keeps its own; one name from
    /// two plugins is a note too, and a run gets one of them).
    pub tools: Vec<Tool>,
    /// The DSH base's packages with install scripts, which were not run.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub install_scripts: Vec<String>,
}

/// What `check` found.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Report {
    /// The report's shape.
    pub version: u32,
    /// No entry and not the agent has a problem, and the agent started.
    pub ok: bool,
    pub entries: Vec<Entry>,
    pub agent: Agent,
}

impl Report {
    fn new(entries: Vec<Entry>, agent: Agent) -> Report {
        let ok = agent.problems.is_empty()
            && agent.together == Together::Started
            && entries.iter().all(|e| e.problems.is_empty());
        Report {
            version: REPORT_VERSION,
            ok,
            entries,
            agent,
        }
    }

    /// The report as text: a block per entry, then the agent.
    pub fn text(&self) -> String {
        let mut out = String::new();
        let said = |out: &mut String, problem: &Problem| {
            out.push_str(&format!("  problem: {}\n", problem.message));
            if let Some(fix) = &problem.fix {
                out.push_str(&format!("  fix: {fix}\n"));
            }
        };
        let count = |problems: &[Problem]| match problems.len() {
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
        let started = self.agent.together == Together::Started;
        for entry in &self.entries {
            let verdict = match (entry.checked_alone, started) {
                // Checked together only, and the agent was not started.
                (false, false) if entry.problems.is_empty() => "not checked".to_owned(),
                _ => count(&entry.problems),
            };
            let kind = match entry.kind {
                EntryKind::Plugin => "plugin",
                EntryKind::Mcp => "mcp",
            };
            out.push_str(&format!(
                "{kind} {}  {} {}  {verdict}\n",
                entry.name,
                entry.ecosystem.word(),
                entry.source,
            ));
            for problem in &entry.problems {
                said(&mut out, problem);
            }
            if !entry.tools.is_empty() {
                out.push_str(&format!("  tools: {}\n", tools(&entry.tools)));
            } else if entry.handlers.is_none() && entry.problems.is_empty() && entry.checked_alone {
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
            if !entry.grants.is_empty() {
                out.push_str(&format!("  grants: {}\n", entry.grants.join(", ")));
            }
            if !entry.install_scripts.is_empty() {
                out.push_str(&format!(
                    "  install scripts not run: {}\n",
                    entry.install_scripts.join(", ")
                ));
            }
        }
        let agent = &self.agent;
        let verdict = match agent.together {
            Together::Skipped => "not started together: an entry has a problem".to_owned(),
            _ => count(&agent.problems),
        };
        out.push_str(&format!("agent {}: {verdict}\n", agent.name));
        for problem in &agent.problems {
            said(&mut out, problem);
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

/// How the gate treats a tool by its name alone: one it judges whoever
/// offers it, else `otherwise`; everything runs with the gate off.
fn kind_of(gate_off: bool, name: &str, otherwise: ToolKind) -> ToolKind {
    match (gate_off, JUDGED_TOOLS.contains(&name)) {
        (true, _) => ToolKind::Runs,
        (false, true) => ToolKind::Judged,
        (false, false) => otherwise,
    }
}

/// Checks the manifest's plugins and servers (no model, no key). An error
/// is what stops `run` before anything starts too (the prompt, the
/// manifest's install, a plugin that cannot be planned, a server that
/// cannot be launched).
pub async fn check(options: CheckOptions) -> Result<Report, BoxError> {
    let cwd = std::env::current_dir().map_err(|e| format!("the current folder: {e}"))?;
    let CheckOptions {
        manifest,
        layout,
        locked,
        home,
    } = options;
    // What `run` reads before anything starts, the key and budgets aside.
    Prompt::read(&manifest)?;
    let Prepared {
        installed,
        plan,
        servers,
        notes,
    } = prepare(&manifest, &layout, home.as_deref(), locked, &cwd)?;
    let gate_off = manifest.builtins.gate == GateMode::Off;
    let allowed_or_asks = |allowed: bool| match allowed {
        true => ToolKind::Allowed,
        false => ToolKind::Asks,
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
    // Install scripts: what the lock says (only said; a lock that cannot
    // be read for it is a note, never a refusal).
    let scripts = match install::install_scripts(&manifest, &layout.adapters) {
        Ok(scripts) => scripts,
        Err(e) => {
            agent_notes.push(format!("install scripts unknown: {e}"));
            install::InstallScripts::default()
        }
    };
    // The packages the DSH probe loads as its base, read as it reads them.
    let dsh_probe_base = match plan.dsh_modules.is_empty() {
        true => Vec::new(),
        false => {
            let mut base: Vec<String> = DSH_PROBE_BASE.iter().map(|s| s.to_string()).collect();
            let rows = rows_from_file(&layout.adapters.join("dsh/host-services.json"), "dsh")?;
            base.extend(rows.iter().map(|row| row.name().to_owned()));
            base
        }
    };

    // The session `run` would have, with no frontend: open (its driver
    // kept), so a plugin that greets or asks as it loads gets what it would
    // with nobody there.
    let (session, _driver) = Session::new(false);
    let mut entries = Vec::new();
    for plugin in &manifest.plugins {
        let (ecosystem, source) = match &plugin.kind {
            PluginKind::Pi(source) => (Ecosystem::Pi, source_text(source)),
            PluginKind::Dsh(source) => (Ecosystem::Dsh, source_text(source)),
            PluginKind::Rutis { path, .. } => (Ecosystem::Rutis, path.display().to_string()),
        };
        let mut entry = Entry {
            kind: EntryKind::Plugin,
            name: plugin.name.clone(),
            ecosystem,
            source,
            checked_alone: true,
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
            grants: plugin
                .grants
                .names()
                .into_iter()
                .map(String::from)
                .collect(),
        };
        let names = match &plugin.kind {
            PluginKind::Pi(source) => {
                if let Source::Package(p) = source {
                    entry.install_scripts = scripts.of(Set::Pi, p.name());
                }
                let files: Vec<String> = plan
                    .pi_files
                    .iter()
                    .filter(|(name, _)| *name == plugin.name)
                    .flat_map(|(_, files)| files.iter().map(|f| f.display().to_string()))
                    .collect();
                let report =
                    yoagent_rutis::probe::pi(&layout.adapters, &cwd, &files, &[], None).await;
                entry.problems.extend(report.problem().map(Problem::new));
                for builtin in &report.without {
                    entry.problems.push(Problem::new(format!(
                        "its tool `{builtin}` is named like yoagent's built-in: `run` refuses it"
                    )));
                }
                report.tools
            }
            PluginKind::Dsh(source) => {
                if let Source::Package(p) = source {
                    entry.install_scripts = scripts.of(Set::Dsh, p.name());
                }
                let ids = plugins::dsh_plugin_rows(&plan, &plugin.name);
                // As `run` loads them: the row's config, `{}` by default.
                let rows: Vec<serde_json::Value> = plan
                    .rows
                    .iter()
                    .filter(|row| ids.iter().any(|id| id == row.id()))
                    .map(|row| {
                        serde_json::json!({ "id": row.id(), "name": row.name(), "config": row.config_json() })
                    })
                    .collect();
                // A row the probe's base loads too: checked together only.
                let in_base: Vec<&str> = rows
                    .iter()
                    .filter_map(|row| row["name"].as_str())
                    .filter(|name| dsh_probe_base.iter().any(|b| b == name))
                    .collect();
                if in_base.is_empty() {
                    let report = yoagent_rutis::probe::dsh(
                        &layout.adapters,
                        &plan.dsh_dir,
                        &serde_json::Value::Array(rows),
                        &[],
                        None,
                    )
                    .await;
                    entry.problems.extend(report.problem().map(Problem::new));
                    entry.guidance_chars = Some(report.guidance_chars);
                    report.tools
                } else {
                    entry.checked_alone = false;
                    entry.notes.push(format!(
                        "not probed alone: the probe's DSH base loads {} itself; checked with the agent together",
                        in_base.join(", ")
                    ));
                    Vec::new()
                }
            }
            PluginKind::Rutis { .. } => {
                match rutis_alone(&manifest, &plan, &session, &plugin.name, &cwd).await {
                    Ok(alone) => {
                        entry.handlers = Some(alone.handlers);
                        entry.notes.extend(alone.notes);
                        entry
                            .problems
                            .extend(alone.problems.into_iter().map(Problem::new));
                        alone.tools
                    }
                    // Waiting for a service another plugin may provide:
                    // `run` would load it with the others.
                    Err(e) if e.to_string().contains("nothing provides") => {
                        entry.checked_alone = false;
                        entry.notes.push(format!(
                            "not checked alone: it waits for a service another plugin may provide ({e}); checked with the agent together"
                        ));
                        Vec::new()
                    }
                    Err(e) => {
                        entry.problems.push(Problem::new(e.to_string()));
                        Vec::new()
                    }
                }
            }
        };
        entry.tools = names
            .into_iter()
            .map(|name| {
                let kind = kind_of(
                    gate_off,
                    &name,
                    allowed_or_asks(plugin.allow.contains(&name)),
                );
                Tool::new(name, kind)
            })
            .collect();
        entries.push(entry);
    }
    // Each MCP server: its tools, none called.
    let mut server_tools: Vec<Tool> = Vec::new();
    let mut schemas: Schemas = HashMap::new();
    for server in &manifest.mcp {
        let (ecosystem, source) = match &server.transport {
            Transport::Stdio { source, .. } => (Ecosystem::Stdio, source_text(source)),
            Transport::Http { url, .. } => (Ecosystem::Http, url.as_str().to_owned()),
        };
        let mut entry = Entry {
            kind: EntryKind::Mcp,
            name: server.name.clone(),
            ecosystem,
            source,
            checked_alone: true,
            problems: Vec::new(),
            notes: Vec::new(),
            tools: Vec::new(),
            handlers: None,
            guidance_chars: None,
            install_scripts: Vec::new(),
            grants: Vec::new(),
        };
        if let Transport::Stdio {
            source: Source::Package(p),
            ..
        } = &server.transport
        {
            entry.install_scripts = scripts.of(Set::Mcp, p.name());
        }
        match mcp(server, &servers[&server.name]).await {
            Ok(tools) => {
                let prefix = format!("{}__", server.name);
                for tool in tools {
                    schemas.extend([schema_of(tool.as_ref())]);
                    let name = tool.name().to_owned();
                    let own = name.strip_prefix(&prefix).unwrap_or(&name);
                    // A server's names are its own: never judged by name.
                    let kind = match gate_off {
                        true => ToolKind::Runs,
                        false => allowed_or_asks(server.allow.iter().any(|a| a == own)),
                    };
                    entry.tools.push(Tool::new(name, kind));
                }
                server_tools.extend(entry.tools.iter().cloned());
            }
            Err(e) => entry.problems.push(Problem::new(e.to_string())),
        }
        entries.push(entry);
    }

    // One name from two plugins: a run is offered one of them (the
    // bridge keeps one per name), so each entry's own list tells.
    let mut by_name: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for entry in entries.iter().filter(|e| e.kind == EntryKind::Plugin) {
        for tool in &entry.tools {
            by_name.entry(&tool.name).or_default().push(&entry.name);
        }
    }
    for (name, plugins) in by_name.iter().filter(|(_, p)| p.len() > 1) {
        agent_notes.push(format!(
            "plugins {} all offer a tool named `{name}`: a run gets one of them",
            plugins.join(", ")
        ));
    }
    let mut agent = Agent {
        name: manifest.agent.name.clone(),
        notes: agent_notes,
        install_scripts: scripts.base.clone(),
        ..Agent::default()
    };
    if entries.iter().all(|e| e.problems.is_empty()) {
        let expected: Vec<String> = entries
            .iter()
            .filter(|e| e.handlers.as_ref().is_some_and(|h| !h.is_empty()))
            .map(|e| plugins::row_id(&e.name))
            .collect();
        let context = Together_ {
            manifest: &manifest,
            plan: &plan,
            layout: &layout,
            session: &session,
            cwd: &cwd,
            server_tools: &server_tools,
            expected: &expected,
            schemas: &std::sync::Mutex::new(Schemas::new()),
        };
        together(&context, &mut agent).await;
        schemas.extend(context.schemas.lock().unwrap().drain());
    }
    // Each tool seen with its description and arguments' schema.
    let fill = |tools: &mut Vec<Tool>| {
        for tool in tools.iter_mut() {
            if let Some((description, parameters)) = schemas.get(&tool.name) {
                tool.description = Some(description.clone());
                tool.parameters = Some(parameters.clone());
            }
        }
    };
    for entry in &mut entries {
        fill(&mut entry.tools);
    }
    fill(&mut agent.tools);
    Ok(Report::new(entries, agent))
}

fn source_text(source: &Source) -> String {
    match source {
        Source::Package(p) => p.spec(),
        Source::Path(path) => path.display().to_string(),
    }
}

/// The tools one run is offered, each with who offered it (read before
/// the run finishes: the registry forgets a run's offers then): the
/// bridge's hooks called directly (`start_run`, `tools`, one
/// `before_model`, `finish`), as the probe does — no model asked. A run
/// stopped or failed at its first request is an error: every run would be.
/// A tool one run is offered, and who offered it.
struct Offer {
    name: String,
    owner: Option<ToolOwner>,
    description: String,
    parameters: serde_json::Value,
}

async fn offered(bridge: &RutisBridge) -> Result<Vec<Offer>, BoxError> {
    let cancel = CancellationToken::new();
    let prompt = [Message::user("check")];
    let run = RunContext::new(RUN, &prompt, &cancel);
    let mut hooks = bridge
        .extension()
        .start_run(&run)
        .await
        .map_err(|e| format!("starting a run: {e}"))?;
    let tools: Vec<(String, String, serde_json::Value)> = hooks
        .tools(&run)
        .await
        .iter()
        .map(|t| {
            (
                t.name().to_owned(),
                t.description().to_owned(),
                t.parameters_schema(),
            )
        })
        .collect();
    let turn = TurnContext::new("", &prompt, &[], RUN).with_run_prompts(&prompt);
    let stopped = match hooks.before_model(&turn).await {
        TurnDecision::Stop(why) | TurnDecision::Fail(why) => Some(why),
        _ => None,
    };
    let owned = tools
        .into_iter()
        .map(|(name, description, parameters)| Offer {
            owner: bridge.registry().offered_by(RUN, &name),
            name,
            description,
            parameters,
        })
        .collect();
    hooks
        .finish(&RunOutcome::new(RunEnd::Completed, None))
        .await;
    match stopped {
        Some(why) => Err(format!("a run would stop at its first request: {why}").into()),
        None => Ok(owned),
    }
}

/// Waits until the handlers `of` returns stop changing (unchanged for
/// [`STILL`] once there are some), at most [`SETTLE`]; their names.
async fn settled(of: impl Fn() -> Vec<String>) -> Vec<String> {
    let deadline = Instant::now() + SETTLE;
    let mut last = of();
    let mut since = Instant::now();
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let now = of();
        if now != last {
            last = now;
            since = Instant::now();
        } else if !last.is_empty() && since.elapsed() >= STILL {
            break;
        }
    }
    last
}

/// Shuts a root of `check`'s down; one whose runtimes do not stop is said.
async fn shut(root: &Ctx) -> Option<String> {
    match tokio::time::timeout(Duration::from_secs(10), root.shutdown()).await {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(format!("shutting its runtimes down: {e}")),
        Err(_) => Some("its runtimes were still running after 10 s".into()),
    }
}

/// A root of its own with yobuild's services on it (`run`'s, with no
/// scratch — `check` makes none — and not `--once`), for a host started
/// apart.
fn services_on(
    root: &Ctx,
    session: &Session,
    manifest: &Manifest,
    cwd: &Path,
) -> Result<(), BoxError> {
    services::provide(root, session)?;
    super::host::provide(
        root,
        super::host::Host::new(
            session.clone(),
            manifest,
            cwd,
            super::gate(manifest, session, cwd, None),
            false,
        ),
    )?;
    let ask_owner = manifest
        .builtins
        .ask_owner
        .then(|| AskOwner::new(session.clone()));
    services::provide_ask_owner(root, ask_owner)?;
    Ok(())
}

/// What a rutis plugin did alone.
struct Alone {
    handlers: Vec<String>,
    tools: Vec<String>,
    problems: Vec<String>,
    notes: Vec<String>,
}

/// A rutis plugin alone, in its own runtime as `run` loads it: the handlers
/// its row registers (given [`SETTLE`]), the tools a run is offered from
/// its row; a row that stopped meanwhile, or a run that would stop, a
/// problem; tools offered from no row (a Cordis child of a plugin that does
/// not inject `yoagent`) a note.
async fn rutis_alone(
    manifest: &Manifest,
    plan: &plugins::Plan,
    session: &Session,
    name: &str,
    cwd: &Path,
) -> Result<Alone, BoxError> {
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
        services_on(&root, session, manifest, cwd)?;
        let mut builder = PluginHost::builder().node(&runtime, dir);
        for service in super::SHARED {
            builder = builder.share(service);
        }
        let mut host = builder.start(&root).await?;
        host.load([row]).await?;
        let handlers = settled(|| {
            let mut names: Vec<String> = bridge
                .registry()
                .handlers()
                .into_iter()
                .filter(|h| h.owner() == id)
                .map(|h| h.name().to_owned())
                .collect();
            names.sort();
            names
        })
        .await;
        let mut alone = Alone {
            handlers,
            tools: Vec::new(),
            problems: host
                .stopped(&[])
                .into_iter()
                .map(|stopped| format!("it stopped: {stopped}"))
                .collect(),
            notes: Vec::new(),
        };
        match offered(&bridge).await {
            Ok(offered) => {
                for Offer {
                    name: tool, owner, ..
                } in offered
                {
                    match owner {
                        Some(owner) if owner.owner() == id => alone.tools.push(tool),
                        None => alone.notes.push(format!(
                            "it offers `{tool}` from no plugin row (a child plugin's, its parent not injecting `yoagent`): the tool asks"
                        )),
                        Some(_) => {}
                    }
                }
            }
            Err(e) => alone.problems.push(e.to_string()),
        }
        drop(host);
        Ok::<_, BoxError>(alone)
    }
    .await;
    let shut = shut(&root).await;
    let mut alone = result?;
    alone.notes.extend(shut);
    Ok(alone)
}

/// What `together` needs (one argument).
#[allow(non_camel_case_types)]
struct Together_<'a> {
    manifest: &'a Manifest,
    plan: &'a plugins::Plan,
    layout: &'a Layout,
    session: &'a Session,
    cwd: &'a Path,
    /// The servers' tools, as their entries judged them.
    server_tools: &'a [Tool],
    /// The rows that registered handlers alone.
    expected: &'a [String],
    /// Where the tools' descriptions and schemas go.
    schemas: &'a std::sync::Mutex<Schemas>,
}

/// The agent together, through `run`'s own start: a refusal there is a
/// problem; then the tools one run is offered, how the gate treats each,
/// and what `run` would only warn about (notes).
async fn together(context: &Together_<'_>, agent: &mut Agent) {
    let Together_ {
        manifest,
        plan,
        layout,
        session,
        cwd,
        server_tools,
        expected,
        schemas,
    } = *context;
    let root = match Ctx::root() {
        Ok(root) => root,
        Err(e) => {
            agent.problems.push(Problem::new(e.to_string()));
            return;
        }
    };
    let result = async {
        // `_dsh_home` keeps DSH's private home while the runtime uses it
        // (`_` alone would drop it at once).
        let Started {
            bridge,
            host,
            dsh_home: _dsh_home,
            ..
        } = match start(&root, manifest, plan, layout, session, false, cwd, None, false).await {
            Ok(started) => started,
            Err(e) => {
                agent.together = Together::Refused;
                agent
                    .problems
                    .push(Problem::new(format!("`run` would refuse it at start: {e}")));
                return Ok(());
            }
        };
        agent.together = Together::Started;
        // A rutis plugin that registered handlers alone registers them here
        // too, in its own time; one that does not is a problem.
        let owners = settled(|| {
            let mut owners: Vec<String> = bridge
                .registry()
                .handlers()
                .iter()
                .map(|h| h.owner().to_owned())
                .filter(|owner| expected.contains(owner))
                .collect();
            owners.sort();
            owners.dedup();
            owners
        })
        .await;
        for row in expected.iter().filter(|row| !owners.contains(row)) {
            agent.problems.push(Problem::new(format!(
                "{row} registered its handlers alone, but none with the other plugins"
            )));
        }
        for stopped in host.stopped(&[]) {
            agent.problems.push(Problem::new(format!("a plugin stopped: {stopped}")));
        }
        let offered = offered(&bridge).await?;
        // The agent's own tools: yoagent's, ask_owner, the servers'.
        let own_tools = tools_for(manifest.builtins.tools, cwd);
        let yoagents: Vec<String> = own_tools.iter().map(|t| t.name().to_owned()).collect();
        let mut seen: Schemas = own_tools.iter().map(|t| schema_of(t.as_ref())).collect();
        if manifest.builtins.ask_owner {
            seen.extend([schema_of(&AskOwner::new(session.clone()))]);
        }
        seen.extend(offered.iter().map(|o| (o.name.clone(), (o.description.clone(), o.parameters.clone()))));
        schemas.lock().unwrap().extend(seen);
        let mut own: HashSet<String> = yoagents.iter().cloned().collect();
        if manifest.builtins.ask_owner {
            own.insert(AskOwner::TOOL.to_owned());
        }
        own.extend(server_tools.iter().map(|t| t.name.clone()));
        let adapters = Adapters {
            pi: plan.pi_handler.clone(),
            dsh: plan.dsh_handler.clone(),
        };
        // The gate's rule (`offered_check`), on the owners kept from the
        // run: not one of the agent's own, and offered by an owner an entry
        // allowing the name accepts.
        let offerers = offerers(manifest, plan);
        let accepted = |name: &str, owner: Option<&ToolOwner>| {
            owner.is_some_and(|owner| {
                offerers
                    .get(name)
                    .is_some_and(|all| all.iter().any(|o| o.offers(owner, &adapters)))
            })
        };
        let gate_off = manifest.builtins.gate == GateMode::Off;
        let mut tools = Vec::new();
        for name in &yoagents {
            let otherwise = match READ_ONLY_TOOLS.contains(&name.as_str()) {
                true => ToolKind::Free,
                false => ToolKind::Judged,
            };
            tools.push(Tool::new(name.clone(), kind_of(gate_off, name, otherwise)));
        }
        if manifest.builtins.ask_owner {
            tools.push(Tool::new(
                AskOwner::TOOL,
                kind_of(gate_off, AskOwner::TOOL, ToolKind::Allowed),
            ));
        }
        tools.extend(server_tools.iter().cloned());
        for Offer { name, owner, .. } in &offered {
            let by = owner.as_ref().map_or("no plugin row", |o| o.owner());
            if own.contains(name) {
                agent.notes.push(format!(
                    "`{name}`, offered by {by}, is named like the agent's own tool: yoagent keeps its own"
                ));
                continue;
            }
            let otherwise = match accepted(name, owner.as_ref()) {
                true => ToolKind::Allowed,
                false => ToolKind::Asks,
            };
            tools.push(Tool::new(name.clone(), kind_of(gate_off, name, otherwise)));
        }
        agent.tools = tools;
        // Each plugin's allowed names, as `run`'s `AllowedCheck` says them
        // after a run: one nobody offered, and one offered only by an owner
        // no entry allowing it accepts.
        for plugin in &manifest.plugins {
            for name in &plugin.allow {
                let Some(Offer { owner, .. }) = offered.iter().find(|o| o.name == *name) else {
                    agent.notes.push(format!(
                        "plugin {} allows `{name}`, which no plugin offered (a typo, or offered only in some runs)",
                        plugin.name
                    ));
                    continue;
                };
                if accepted(name, owner.as_ref()) {
                    continue;
                }
                agent.notes.push(match owner {
                    Some(owner) => format!(
                        "plugin {} allows `{name}`, but it was offered by `{}`, not by it: the tool asks",
                        plugin.name,
                        owner.owner()
                    ),
                    None => format!(
                        "plugin {} allows `{name}`, but it was offered from no plugin row (it must inject `yoagent`: a child plugin registers through its parent's row only if the parent does)",
                        plugin.name
                    ),
                });
            }
        }
        drop(host);
        Ok::<_, BoxError>(())
    }
    .await;
    if let Err(e) = result {
        agent.problems.push(Problem::new(e.to_string()));
    }
    if let Some(shut) = shut(&root).await {
        agent.notes.push(shut);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, ecosystem: Ecosystem) -> Entry {
        Entry {
            kind: EntryKind::Plugin,
            name: name.into(),
            ecosystem,
            source: format!("./{name}.ts"),
            checked_alone: true,
            problems: Vec::new(),
            notes: Vec::new(),
            tools: Vec::new(),
            handlers: None,
            guidance_chars: None,
            install_scripts: Vec::new(),
            grants: Vec::new(),
        }
    }

    fn tool(name: &str, kind: ToolKind) -> Tool {
        Tool::new(name, kind)
    }

    #[test]
    fn the_text_says_each_entry_then_the_agent() {
        let mut pi = entry("p", Ecosystem::Pi);
        pi.tools = vec![
            tool("todo", ToolKind::Allowed),
            tool("edit", ToolKind::Judged),
        ];
        pi.install_scripts = vec!["esbuild".into()];
        let mut dsh = entry("d", Ecosystem::Dsh);
        dsh.guidance_chars = Some(12);
        dsh.notes = vec!["a note".into()];
        let mut rutis = entry("r", Ecosystem::Rutis);
        rutis.handlers = Some(vec!["a".into(), "b".into()]);
        rutis.grants = vec!["workspace".into(), "wake".into()];
        let mut empty = entry("e", Ecosystem::Rutis);
        empty.handlers = Some(Vec::new());
        let agent = Agent {
            name: "x".into(),
            together: Together::Started,
            tools: vec![tool("read_file", ToolKind::Free)],
            install_scripts: vec!["koffi".into()],
            ..Agent::default()
        };
        let report = Report::new(vec![pi, dsh, rutis, empty], agent);
        assert!(report.ok);
        assert_eq!(
            report.text(),
            "plugin p  pi ./p.ts  ok\n\
             \x20 tools: todo (allowed), edit (judged per call)\n\
             \x20 install scripts not run: esbuild\n\
             plugin d  dsh ./d.ts  ok\n\
             \x20 tools: none\n\
             \x20 prompt guidance: 12 characters\n\
             \x20 note: a note\n\
             plugin r  rutis ./r.ts  ok\n\
             \x20 handlers: a, b\n\
             \x20 grants: workspace, wake\n\
             plugin e  rutis ./e.ts  ok\n\
             \x20 handlers: none (it offers no tool)\n\
             agent x: ok\n\
             \x20 tools: read_file (free)\n\
             \x20 install scripts not run (the DSH base): koffi\n"
        );
        // In the JSON too; an entry with none has no `grants`.
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(
            json["entries"][2]["grants"],
            serde_json::json!(["workspace", "wake"])
        );
        assert!(json["entries"][0].get("grants").is_none());
    }

    #[test]
    fn problems_and_entries_not_checked_are_said() {
        let mut bad = entry("b", Ecosystem::Pi);
        bad.problems = vec![Problem::new("one"), Problem::new("two")];
        let mut skipped = entry("s", Ecosystem::Dsh);
        skipped.checked_alone = false;
        skipped.notes = vec!["not probed alone".into()];
        let report = Report::new(
            vec![bad, skipped.clone()],
            Agent {
                name: "x".into(),
                ..Agent::default()
            },
        );
        assert!(!report.ok);
        assert_eq!(
            report.text(),
            "plugin b  pi ./b.ts  2 problems\n\
             \x20 problem: one\n\
             \x20 problem: two\n\
             plugin s  dsh ./s.ts  not checked\n\
             \x20 note: not probed alone\n\
             agent x: not started together: an entry has a problem\n"
        );
        // Refused together: one problem, and the skipped entry is not "ok".
        let refused = Report::new(
            vec![skipped],
            Agent {
                name: "x".into(),
                together: Together::Refused,
                problems: vec![Problem::new("`run` would refuse it")],
                ..Agent::default()
            },
        );
        assert!(!refused.ok);
        assert!(refused
            .text()
            .ends_with("not checked\n  note: not probed alone\nagent x: 1 problem\n  problem: `run` would refuse it\n"));
    }

    #[test]
    fn json_is_camel_case_with_a_verdict_and_a_version() {
        let mut e = entry("d", Ecosystem::Dsh);
        e.guidance_chars = Some(3);
        let report = Report::new(
            vec![e],
            Agent {
                name: "x".into(),
                together: Together::Started,
                ..Agent::default()
            },
        );
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["version"], 2);
        assert_eq!(json["ok"], true);
        assert_eq!(json["agent"]["together"], "started");
        assert_eq!(json["entries"][0]["kind"], "plugin");
        assert_eq!(json["entries"][0]["ecosystem"], "dsh");
        assert_eq!(json["entries"][0]["checkedAlone"], true);
        assert_eq!(json["entries"][0]["guidanceChars"], 3);
        let mut bad = entry("b", Ecosystem::Pi);
        bad.problems = vec![Problem::new("one")];
        let json = serde_json::to_value(Report::new(vec![bad], Agent::default())).unwrap();
        assert_eq!(
            json["entries"][0]["problems"][0],
            serde_json::json!({ "message": "one" })
        );
    }

    #[test]
    fn a_tool_the_gate_judges_by_name_is_judged_whoever_offers_it() {
        for name in JUDGED_TOOLS {
            assert_eq!(kind_of(false, name, ToolKind::Asks), ToolKind::Judged);
            assert_eq!(kind_of(true, name, ToolKind::Asks), ToolKind::Runs);
        }
        assert_eq!(kind_of(false, "todo", ToolKind::Allowed), ToolKind::Allowed);
    }
}
