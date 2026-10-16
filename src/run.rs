//! `yobuild run`: an agent from its manifest — the model, its limits, the
//! parts, the plugins (by ecosystem), MCP servers, and a frontend.
//!
//! Until yobuild installs packages (M2b), a pinned plugin or server must
//! already be installed, at exactly its version: pi packages and stdio MCP
//! servers in `node_modules/` beside the manifest, DSH plugins in the DSH
//! runtime's (`adapters/dsh`).

pub(crate) mod plugins;
pub(crate) mod script;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rutis::Ctx;
use yoagent::context::ExecutionLimits;
use yoagent::mcp::{HttpTransport, McpClient, McpToolAdapter};
use yoagent::provider::{prices, ModelConfig};
use yoagent::tools::default_tools;
use yoagent::{Agent, AgentTool};
use yoagent_rutis::RutisBridge;

use crate::frontend::host::{DshHome, PluginHost, Row};
use crate::frontend::{services, web, ClientMessage, RunOutcome, ServerMessage, Session};
use crate::manifest::{Gate as GateMode, Manifest, Source, Transport};
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
    /// This repository's.
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
    /// The agent's working folder.
    pub cwd: PathBuf,
    /// The model's replies from this file instead of the provider: a JSON
    /// list of `{ "text": … }` or `{ "tools": [{ "name", "arguments" }] }`.
    pub scripted: Option<PathBuf>,
    /// A first prompt.
    pub task: Option<String>,
    /// Run `task` alone, with no frontend, and return when it ends.
    pub once: bool,
    /// Where the web frontend listens (default `127.0.0.1:8787`; with
    /// `phone`, every interface).
    pub addr: Option<SocketAddr>,
    pub layout: Layout,
}

impl Options {
    pub fn new(manifest: Manifest, cwd: impl Into<PathBuf>) -> Self {
        Self {
            manifest,
            cwd: cwd.into(),
            scripted: None,
            task: None,
            once: false,
            addr: None,
            layout: Layout::default(),
        }
    }
}

/// How a `once` run ended, and the agent (its messages).
#[non_exhaustive]
pub struct Ended {
    pub outcome: RunOutcome,
    pub error: Option<String>,
    pub agent: Agent,
}

/// Runs the agent: until the frontend ends the session (terminal), Ctrl+C
/// (web), or the task's run ends (`once`, returning how it ended).
pub async fn run(root: &Ctx, options: Options) -> Result<Option<Ended>, BoxError> {
    let started = Instant::now();
    let Options {
        manifest,
        cwd,
        scripted,
        task,
        once,
        addr,
        layout,
    } = options;
    let lookup = |name: &str| std::env::var(name).ok();
    let frontend = &manifest.frontend;
    let terminal = frontend.terminal && !once;

    // The model, before anything starts: a missing key fails here.
    let model = &manifest.agent.model;
    let key = match (&scripted, model.api_key()) {
        (Some(_), _) => None,
        (None, Some(key)) => Some(key),
        // A local server: an empty key, so yoagent's fallback key is not sent.
        (None, None) if !model.needs_key() => Some(String::new()),
        (None, None) => {
            return Err(format!(
                "{}/{} needs its key in {}",
                model.provider(),
                model.id(),
                model.key_env().join(" or ")
            )
            .into())
        }
    };
    prices::enable_bundled();
    let config = match &scripted {
        Some(_) => ModelConfig::mock(),
        None => model.config(),
    };

    // A terminal UI ends the session when it quits, as does a `once` run.
    let (session, driver) = Session::new(terminal || once);
    let bridge = RutisBridge::install(root)?;
    services::provide(root, &session)?;

    let scratch = match manifest.parts.scratch {
        true => Some(Scratch::create(&manifest.agent.name)?),
        false => None,
    };
    let plan = plugins::plan(&manifest, &layout, &cwd, &lookup)?;
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
        host.load(plan.rows).await?;
        wait_for(&bridge, &plan.handlers, &host).await?;
    }

    // The tools: yoagent's, `ask_owner`, each MCP server's.
    let mut tools: Vec<Box<dyn AgentTool>> = default_tools();
    let ask_owner = manifest
        .parts
        .ask_owner
        .then(|| AskOwner::new(session.clone()));
    if let Some(ask) = &ask_owner {
        tools.push(Box::new(ask.clone()));
    }
    for server in &manifest.mcp {
        tools.extend(mcp(server, &manifest.dir, &lookup).await?);
    }

    let mut agent = match (&scripted, key) {
        (Some(path), _) => Agent::from_provider(script::provider(path)?, config.clone()),
        (None, key) => Agent::from_config(config.clone()).with_api_key(key.unwrap_or_default()),
    }
    .with_system_prompt(system_prompt(&manifest, &cwd, scratch.as_ref())?)
    .with_tools(tools)
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
    if let Some(usd) = manifest.limits.budget_usd {
        if let Some(budget) = yoagent::extension::Budget::for_model(usd, &config) {
            agent = agent.with_extension(budget);
        }
    }
    eprintln!(
        "{}: ready in {:.1}s",
        manifest.agent.name,
        started.elapsed().as_secs_f64()
    );

    if once {
        // Watching, not a frontend: a question gets its default at once.
        let mut watching = session.observe();
        session.send(ClientMessage::Prompt {
            text: task.unwrap_or_default(),
        });
        let running = tokio::spawn(driver.run(agent));
        let (outcome, error) = loop {
            match watching.messages.recv().await {
                Some(ServerMessage::RunEnd { outcome, error, .. }) => break (outcome, error),
                Some(_) => {}
                None => break (RunOutcome::Error, Some("the session ended".into())),
            }
        };
        session.send(ClientMessage::Quit);
        let agent = running.await?;
        return Ok(Some(Ended {
            outcome,
            error,
            agent,
        }));
    }
    if terminal {
        host.load([Row::new(
            "terminal-ui",
            layout.frontend.join("plugins/terminal-ui.ts").display(),
        )
        .runtime("ui")])
            .await?;
        if let Some(task) = task {
            session.send(ClientMessage::Prompt { text: task });
        }
        drop(driver.run(agent).await);
        return Ok(None);
    }
    let addr = addr.unwrap_or_else(|| match frontend.phone {
        true => "0.0.0.0:8787".parse().expect("an address"),
        false => "127.0.0.1:8787".parse().expect("an address"),
    });
    let served = web::serve(session.clone(), addr).await?;
    let url = match (frontend.phone, lan_ip()) {
        (true, Some(ip)) => format!("http://{ip}:{}/?t={}", served.addr().port(), served.token()),
        _ => served.url(),
    };
    println!("{}: {url}  (Ctrl+C to stop)", manifest.agent.name);
    if let Some(task) = task {
        session.send(ClientMessage::Prompt { text: task });
    }
    tokio::select! {
        _ = driver.run(agent) => {}
        stop = tokio::signal::ctrl_c() => {
            if let Err(e) = stop {
                eprintln!("cannot listen for Ctrl+C ({e}); stopping");
            }
        }
    }
    served.stop();
    Ok(None)
}

/// Waits until the plugins' `handlers` are registered.
async fn wait_for(
    bridge: &RutisBridge,
    handlers: &[&str],
    host: &PluginHost,
) -> Result<(), BoxError> {
    let registry = bridge.registry().clone();
    tokio::time::timeout(Duration::from_secs(60), async {
        while !handlers
            .iter()
            .all(|name| registry.handlers().iter().any(|h| h.name() == *name))
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map_err(|_| {
        format!(
            "plugins did not register their handlers: {:?}",
            host.status()
        )
    })?;
    Ok(())
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

/// The manifest's prompt file (`{cwd}`, `{scratch}` filled in), or a plain
/// one; the report's instruction after it.
fn system_prompt(
    manifest: &Manifest,
    cwd: &Path,
    scratch: Option<&Scratch>,
) -> Result<String, BoxError> {
    let scratch = scratch.map_or_else(
        || cwd.display().to_string(),
        |s| s.path().display().to_string(),
    );
    let mut prompt = match &manifest.agent.prompt {
        Some(path) => std::fs::read_to_string(path)
            .map_err(|e| format!("prompt {}: {e}", path.display()))?
            .replace("{cwd}", &cwd.display().to_string())
            .replace("{scratch}", &scratch),
        None => format!(
            "You are {}, an agent working in {}.",
            manifest.agent.name,
            cwd.display()
        ),
    };
    if let Some(report) = &manifest.parts.report {
        prompt.push_str("\n\n");
        prompt.push_str(&report.instruction());
    }
    Ok(prompt)
}

/// An MCP server's tools, named after the server for display.
async fn mcp(
    server: &crate::manifest::Mcp,
    dir: &Path,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<Vec<Box<dyn AgentTool>>, BoxError> {
    let failed = |e: &dyn std::fmt::Display| format!("mcp {}: {e}", server.name);
    let client = match &server.transport {
        Transport::Stdio {
            source,
            bin,
            args,
            env,
        } => {
            let command = match source {
                Source::Path(path) => path.clone(),
                Source::Package(package) => {
                    let installed = plugins::installed(package, &dir.join("node_modules"))
                        .map_err(|e| failed(&e))?;
                    bin_of(&installed, bin.as_deref()).map_err(|e| failed(&e))?
                }
            };
            let args: Vec<String> = args
                .iter()
                .map(|a| a.resolve_with(lookup))
                .collect::<Result<_, _>>()
                .map_err(|e| failed(&e))?;
            let env: HashMap<String, String> = env
                .iter()
                .map(|(k, v)| Ok((k.clone(), v.resolve_with(lookup)?)))
                .collect::<Result<_, crate::manifest::Unset>>()
                .map_err(|e| failed(&e))?;
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            McpClient::connect_stdio(&command.display().to_string(), &args, Some(env))
                .await
                .map_err(|e| failed(&e))?
        }
        Transport::Http { url, headers } => {
            let url = url.resolve_with(lookup).map_err(|e| failed(&e))?;
            let mut transport = HttpTransport::new(&url).map_err(|e| failed(&e))?;
            for (name, value) in headers {
                let value = value.resolve_with(lookup).map_err(|e| failed(&e))?;
                transport = transport.with_header(name, value).map_err(|e| failed(&e))?;
            }
            McpClient::connect_http_with(transport)
                .await
                .map_err(|e| failed(&e))?
        }
    };
    let client = Arc::new(tokio::sync::Mutex::new(client));
    let tools = McpToolAdapter::from_client_with_prefix(client, server.name.clone())
        .await
        .map_err(|e| failed(&e))?;
    Ok(tools
        .into_iter()
        .map(|t| Box::new(t) as Box<dyn AgentTool>)
        .collect())
}

/// The program an installed package runs: its `bin` (the one named `bin`
/// when it has several).
fn bin_of(dir: &Path, name: Option<&str>) -> Result<PathBuf, String> {
    let text = std::fs::read_to_string(dir.join("package.json")).map_err(|e| e.to_string())?;
    let manifest: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let file = match (&manifest["bin"], name) {
        (serde_json::Value::String(file), _) => file.clone(),
        (serde_json::Value::Object(bins), Some(name)) => bins
            .get(name)
            .and_then(|f| f.as_str())
            .ok_or_else(|| format!("no bin {name:?} in {}", dir.display()))?
            .to_owned(),
        (serde_json::Value::Object(bins), None) if bins.len() == 1 => bins
            .values()
            .next()
            .and_then(|f| f.as_str())
            .unwrap_or_default()
            .to_owned(),
        (serde_json::Value::Object(bins), None) => {
            let names: Vec<&String> = bins.keys().collect();
            return Err(format!(
                "{} has several bins ({names:?}): name one with bin",
                dir.display()
            ));
        }
        _ => return Err(format!("{} has no bin", dir.display())),
    };
    Ok(dir.join(file))
}

/// This machine's address on the local network (no packet is sent).
fn lan_ip() -> Option<std::net::IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    socket.local_addr().ok().map(|a| a.ip())
}
