//! EXPERIMENTAL. A personal handoff agent: hand it a task — research,
//! writing, planning or code — and walk away.
//!
//! - It plans with a pi extension, unchanged (`@juicesharp/rpiv-todo`'s
//!   `todo` tool; the `todo-plan` UI plugin draws the plan in the browser),
//!   researches with DSH's free web search, and works with yoagent's file
//!   and shell tools in the directory it was started in.
//! - It asks without waiting: `ask_owner` puts a question to every frontend
//!   — kept open while nobody is there (`Session::ask_later`) — and returns
//!   at once, so the agent carries on with what does not depend on it. The
//!   answer, whenever it comes, is sent in as a steer message: picked up by
//!   the run in progress, or starting a new run when the agent had stopped.
//! - Guardrails, not a sandbox: the gate (`yobuild::builtins::Gate`, in Rust)
//!   is an allow-list — reading, building, testing, committing and writing
//!   new files run; anything else, above all deleting, discarding or
//!   overwriting what the owner has, waits for their yes, and is refused
//!   with nobody there;
//!   runs are capped in turns, tokens and minutes, and in dollars when the
//!   model has a price.
//! - Every run ends with a handoff report (Done / Waiting on you /
//!   Decisions / Next — the agent is sent back until it writes one), and
//!   the host adds what the model cannot make up: the files changed, the
//!   tokens, tool calls and time, the questions still open.
//!
//! ```bash
//! cd my-project                                       # it works where it starts
//! path/to/yobuild/scripts/try-handoff.sh --live "…"    # DeepSeek (DEEPSEEK_API_KEY)
//! path/to/yobuild/scripts/try-handoff.sh --phone "…"   # serve on the LAN, for a phone
//! path/to/yobuild/scripts/try-handoff.sh --demo        # headless, scripted (CI)
//! ```
//!
//! (`cargo run` needs `--manifest-path path/to/yobuild/Cargo.toml`
//! when started from the project directory.)
//!
//! `--web ADDR` (default 127.0.0.1:8787), `--no-dsh`, `--dsh-services` (DSH's own host services, for DSH plugins that need them, in a DSH home of their own), `--max-minutes N`
//! (30) and `--max-tokens N` (4M) per run, `--budget-usd X` (2; priced
//! models only). Without a task, give it
//! one in the page. `--phone` serves on every interface: anyone on the
//! network with the printed URL (it carries the token) drives the agent.
//!
//! Setup: `scripts/try-coding-agent.sh --setup-only` (Node 22.19+, npm).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use rutis::Ctx;
use serde_json::{json, Value as Json};
use yoagent::context::ExecutionLimits;
use yoagent::provider::mock::{MockResponse, MockToolCall};
use yoagent::provider::{prices, MockProvider, ModelConfig};
use yoagent::tools::default_tools;
use yoagent::{Agent, AgentEvent, AgentMessage, Content, Message};
use yoagent_rutis::RutisBridge;
use yobuild::builtins::{AskOwner, Facts, Gate, Report, Scratch};
use yobuild::frontend::host::{rows_from_file, DshHome, PluginHost, Row};
use yobuild::frontend::{services, web, ClientMessage, ServerMessage, Session};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The plugin tools that only plan or search: rpiv-todo's plan and
/// dsh-free-search's searches (not its `npm` or test tools), and
/// `ask_owner` (yoagent's read-only tools are free by default).
const FREE_TOOLS: [&str; 15] = [
    AskOwner::TOOL,
    "todo",
    "multi_search",
    "research",
    "web_search",
    "search_web_pages",
    "advanced_search",
    "global_search",
    "domain_search",
    "platform_search",
    "video_search",
    "zhihu_search",
    "cache_search",
    "search_pro",
    "web_search_exa",
];

/// `frontend/`, where the TypeScript plugins live.
fn here() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("frontend")
}
fn rutis_plugins() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("adapters")
}

struct Options {
    key: Option<String>,
    addr: SocketAddr,
    phone: bool,
    demo: bool,
    dsh: bool,
    /// DSH's own host services (`--dsh-services`), for DSH plugins that need them.
    dsh_services: bool,
    task: Option<String>,
    max_minutes: u64,
    max_tokens: usize,
    budget_usd: f64,
}

fn main() -> Result<(), BoxError> {
    let mut options = Options {
        key: None,
        addr: "127.0.0.1:8787".parse()?,
        phone: false,
        demo: false,
        dsh: rutis_plugins().join("dsh/node_modules").exists(),
        dsh_services: false,
        task: None,
        max_minutes: 30,
        // Research reads a lot: yoagent's default (1M per run, the whole
        // prompt counted each turn) stops a long one before it writes up.
        max_tokens: 4_000_000,
        budget_usd: 2.0,
    };
    let mut live = false;
    let mut words = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--live" => live = true,
            "--demo" => options.demo = true,
            "--no-dsh" => options.dsh = false,
            "--dsh-services" => options.dsh_services = true,
            "--phone" => {
                options.phone = true;
                options.addr = "0.0.0.0:8787".parse()?;
            }
            "--web" => options.addr = args.next().ok_or("--web needs ADDR")?.parse()?,
            "--max-minutes" => {
                options.max_minutes = args.next().ok_or("--max-minutes needs N")?.parse()?
            }
            "--max-tokens" => {
                options.max_tokens = args.next().ok_or("--max-tokens needs N")?.parse()?
            }
            "--budget-usd" => {
                options.budget_usd = args.next().ok_or("--budget-usd needs X")?.parse()?
            }
            flag if flag.starts_with("--") => return Err(format!("unknown argument {flag}").into()),
            word => words.push(word.to_owned()),
        }
    }
    options.task = (!words.is_empty()).then(|| words.join(" "));
    for dir in [here().join("plugins"), rutis_plugins().join("pi")] {
        if !dir.join("node_modules").exists() {
            return Err(format!(
                "{} is not installed: scripts/try-coding-agent.sh --setup-only",
                dir.display()
            )
            .into());
        }
    }
    if live {
        match std::env::var("DEEPSEEK_API_KEY") {
            Ok(key) if !key.trim().is_empty() => options.key = Some(key.trim().to_owned()),
            _ => return Err("--live needs DEEPSEEK_API_KEY".into()),
        }
    }
    if std::env::var_os("NODE_OPTIONS").is_none() {
        std::env::set_var("NODE_OPTIONS", "--no-warnings");
    }
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new("warn,yoagent::provider::registry=error")
    });
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let root = Ctx::root()?;
            let result = run(&root, options).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
            if let Err(e) = root.shutdown().await {
                match &result {
                    Ok(()) => return Err(e.into()),
                    Err(_) => eprintln!("shutting the plugins down also failed: {e}"),
                }
            }
            result
        })
}

async fn run(root: &Ctx, options: Options) -> Result<(), BoxError> {
    let launched = Instant::now();
    let cwd = std::env::current_dir()?;
    // The agent's own scratch (downloads, helper scripts): not the owner's folder.
    let scratch = Scratch::create("yobuild-handoff")?;
    // The demo ends itself with `Quit`: the agent's own tools hold session
    // handles, so the driver would not end when the demo drops its own.
    let (session, driver) = Session::new(options.demo);
    let bridge = RutisBridge::install(root)?;
    services::provide(root, &session)?;

    let mut builder = PluginHost::builder()
        .node("ui", here().join("plugins"))
        .node("pi", rutis_plugins().join("pi"));
    // DSH's own host services, on request: in a home of their own, never
    // the user's `~/.dsh` (see `DshHome` for what that does not cover).
    let dsh_home = match options.dsh && options.dsh_services {
        true => Some(DshHome::new()?),
        false => None,
    };
    if options.dsh {
        builder = builder.node("dsh", rutis_plugins().join("dsh"));
        for (name, value) in dsh_home.iter().flat_map(DshHome::env) {
            builder = builder.env("dsh", name, value);
        }
    }
    let mut host = builder
        .share("yoagent")
        .share(services::FRONTEND)
        .share(services::UI)
        .start(root)
        .await?;
    let mut rows = vec![
        // pi extensions, unchanged: the plan.
        Row::new(
            "pi",
            rutis_plugins()
                .join("pi/pi-extensions-adapter.ts")
                .display(),
        )
        .runtime("pi")
        .config(json!({
            "extensions": [
                here().join("plugins/node_modules/@juicesharp/rpiv-todo/index.ts"),
            ],
            "cwd": cwd,
        })),
        Row::new(
            "pi-host-ui",
            rutis_plugins().join("pi/host-ui.ts").display(),
        )
        .runtime("pi"),
        Row::new("todo-plan", here().join("plugins/todo-plan.ts").display()).runtime("ui"),
    ];
    if options.dsh {
        // The services a headless DSH host runs, for plugins that need them
        // (none of the rows below does).
        if options.dsh_services {
            rows.extend(rows_from_file(
                &yoagent_rutis::adapters::adapters()
                    .dsh
                    .join("host-services.json"),
                "dsh",
            )?);
        }
        let dsh = |id: &str, name: &str| Row::new(id, name).runtime("dsh");
        rows.extend([
            dsh("dsh-web", "@deepseek-ai/dsh-web"),
            dsh("dsh-system-prompt", "@deepseek-ai/dsh-system-prompt"),
            dsh("dsh-tools", "@deepseek-ai/dsh-tools"),
            dsh("dsh-free-search", "dsh-free-search").config(
                json!({ "provider": "bing", "disabledEngines": [], "bingMarket": "en-US" }),
            ),
            Row::new(
                "dsh",
                rutis_plugins().join("dsh/dsh-tools-adapter.ts").display(),
            )
            .runtime("dsh"),
            Row::new(
                "dsh-host-dialogs",
                rutis_plugins().join("dsh/host-dialogs.ts").display(),
            )
            .runtime("dsh"),
        ]);
    }
    host.load(rows).await?;
    // A TypeScript row runs before its async start-up is done: wait for the
    // handlers the agent depends on.
    let mut needed = vec!["pi-extensions"];
    if options.dsh {
        needed.push("dsh-tools");
    }
    let registry = bridge.registry().clone();
    tokio::time::timeout(Duration::from_secs(60), async {
        while !needed
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
    println!(
        "plugins ready in {:.1}s ({} Node runtimes)",
        launched.elapsed().as_secs_f64(),
        if options.dsh { 3 } else { 2 }
    );

    // Prices are opt-in; a model with none gets no dollar cap (turns and minutes still apply).
    prices::enable_bundled();
    let config = match &options.key {
        Some(_) => ModelConfig::deepseek("deepseek-flash", "DeepSeek Flash"),
        None => ModelConfig::mock(),
    };
    let budget = yoagent::extension::Budget::for_model(options.budget_usd, &config)
        .map(|budget| budget.across_runs());
    let report = Report::handoff();
    let ask_owner = AskOwner::new(session.clone());
    // Snapshot the folder before the agent touches it.
    let facts = Facts::new(session.clone(), &cwd).with_open_questions(ask_owner.open_questions());
    let mut agent = match &options.key {
        Some(key) => Agent::from_config(config).with_api_key(key.clone()),
        None => Agent::from_provider(scripted(), config),
    }
    .with_system_prompt(system_prompt(&cwd, scratch.path(), &report))
    .with_tools({
        // `with_tools` sets the whole list: yoagent's tools and `ask_owner`.
        let mut tools = default_tools();
        tools.push(Box::new(ask_owner));
        tools
    })
    .with_execution_limits(
        ExecutionLimits::default()
            .with_max_turns(80)
            .with_max_total_tokens(options.max_tokens)
            .with_max_duration(Duration::from_secs(options.max_minutes * 60)),
    )
    .with_extension(bridge.extension())
    // The approval gate: what it knows to be harmless runs; the agent
    // deletes and overwrites freely in its scratch and in what it created;
    // everything else asks.
    .with_extension(
        Gate::new(session.clone(), &cwd)
            .with_scratch(&scratch)
            // Tools that only plan or look things up; any other plugin tool asks.
            .with_allowed_tools(FREE_TOOLS),
    )
    .with_extension(report)
    .with_extension(facts);
    let priced = budget.is_some();
    if let Some(budget) = budget {
        agent = agent.with_extension(budget);
    }

    if options.demo {
        return demo(session, driver, agent, options.task).await;
    }
    let served = web::serve(session.clone(), options.addr).await?;
    let url = match (options.phone, lan_ip()) {
        (true, Some(ip)) => format!("http://{ip}:{}/?t={}", served.addr().port(), served.token()),
        _ => served.url(),
    };
    println!("handoff agent: {url}");
    println!(
        "  limits: {} min per run{}  (Ctrl+C to stop)",
        options.max_minutes,
        if priced {
            format!(", ${:.2} in all", options.budget_usd)
        } else {
            ", no dollar cap (this model has no price)".into()
        }
    );
    if let Some(task) = options.task {
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
    Ok(())
}

fn system_prompt(cwd: &std::path::Path, scratch: &std::path::Path, report: &Report) -> String {
    format!(
        "You are the owner's handoff agent. The owner handed you a task — research, writing, \
         planning, code, whatever it is — and went away; you finish it on your own. You can \
         search the web, and read, write and run things in {cwd}.\n\
         - Plan with the `todo` tool: create the steps first, mark each in progress and done.\n\
         - Put what you make in files in {cwd} (notes, reports, plans, code) so the owner finds \
         it there, and only that: keep your scratch (downloaded pages, helper scripts, notes to \
         yourself) in {scratch}. Write the deliverable early and update it as you go, so \
         stopping early still leaves something useful.\n\
         - For research, use the search tools (they return extracted text) rather than \
         downloading whole pages; never print a whole page. Cite your sources with links and \
         say how sure you are.\n\
         - Decide what you can find out or reasonably choose yourself. Ask with `ask_owner` only \
         what is the owner's to decide: preferences, budgets, taste, trade-offs only they can \
         weigh. Never wait for an answer: keep going with work that does not depend on it. It \
         arrives later as a message starting with `[Owner's answer to #N]` (or `[Owner \
         skipped #N]`: then decide yourself and say so). If everything left depends on an \
         answer, stop.\n\
         - Do nothing that cannot be undone or that acts for the owner (deleting their files, \
         spending money, sending messages, signing up for things) without their answer first. \
         Some commands ask the owner for approval and may be refused.\n\
         - {report} Under Waiting on you, list the open questions by number.",
        cwd = cwd.display(),
        scratch = scratch.display(),
        report = report.instruction(),
    )
}

/// This machine's address on the local network (no packet is sent).
fn lan_ip() -> Option<std::net::IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    socket.local_addr().ok().map(|a| a.ip())
}

fn call(name: &str, arguments: Json) -> MockToolCall {
    MockToolCall {
        provider_metadata: None,
        name: name.into(),
        arguments,
    }
}

/// Without `--live`: a scripted model that does what its reports say. Run
/// 1 plans, asks which format the notes should take (without waiting),
/// lists the folder, and reports; run 2, once the owner answered, writes
/// `notes.md` in that format and reports.
fn scripted() -> MockProvider {
    MockProvider::new(vec![
        MockResponse::ToolCalls(vec![call(
            "todo",
            json!({ "action": "create", "subject": "List the folder and write notes.md" }),
        )]),
        MockResponse::ToolCalls(vec![
            call(
                "ask_owner",
                json!({
                    "question": "Should notes.md be a bullet list or a short paragraph?",
                    "why": "Only the format of the notes depends on it.",
                    "options": ["Bullet list", "Short paragraph"]
                }),
            ),
            call("list_files", json!({ "path": "." })),
        ]),
        MockResponse::Text(
            "(scripted) ## Done\n- Listed the folder.\n\
             ## Waiting on you\n- #1 Should notes.md be a bullet list or a short paragraph?\n\
             ## Decisions\n- None yet.\n## Next\n- Write notes.md once #1 is answered."
                .into(),
        ),
        MockResponse::ToolCalls(vec![
            call(
                "write_file",
                json!({
                    "path": "notes.md",
                    "content": "# Notes\n\n- Written by the handoff agent's scripted demo.\n- A bullet list, as the owner chose.\n"
                }),
            ),
            call("todo", json!({ "action": "update", "id": 1, "status": "completed" })),
        ]),
        MockResponse::Text(
            "(scripted) ## Done\n- Wrote notes.md as a bullet list.\n## Waiting on you\n- Nothing.\n\
             ## Decisions\n- A bullet list, as you answered #1.\n## Next\n- Nothing."
                .into(),
        ),
    ])
}

/// `--demo`: no browser; a frontend plays the owner — it waits for the run
/// to stop, then answers the question, which starts the second run.
async fn demo(
    session: Session,
    driver: yobuild::frontend::Driver,
    agent: Agent,
    task: Option<String>,
) -> Result<(), BoxError> {
    let mut owner = session.connect();
    let running = tokio::spawn(driver.run(agent));
    let task = task.unwrap_or_else(|| "Look at this folder and write notes.md about it.".into());
    println!("> {task}");
    session.send(ClientMessage::Prompt { text: task });
    let mut question = None;
    let mut ended = 0;
    let mut facts = Vec::new();
    let mut started = std::collections::HashMap::new();
    // When the model's last message ended (its tool calls then go through the policies).
    let mut asked: Option<Instant> = None;
    while ended < 2 {
        let message = tokio::time::timeout(Duration::from_secs(120), owner.messages.recv())
            .await
            .map_err(|_| "the demo stalled")?
            .ok_or("the session ended early")?;
        match message {
            ServerMessage::Event { event, .. } => match *event {
                AgentEvent::ToolExecutionStart { tool_call_id, .. } => {
                    // From the model's message to the tool starting: the loop's
                    // own steps and every policy (the pi adapter's, across the
                    // bridge) — `ToolExecutionStart` comes after `before_tool`.
                    let policy = asked.map(|at| at.elapsed()).unwrap_or_default();
                    started.insert(tool_call_id, (Instant::now(), policy));
                }
                AgentEvent::ToolExecutionEnd {
                    tool_call_id,
                    tool_name,
                    is_error,
                    result,
                    ..
                } => {
                    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
                    let took = started
                        .remove(&tool_call_id)
                        .map(|(at, policy)| {
                            format!(
                                " (loop + policy {:.1} ms, tool {:.1} ms)",
                                ms(policy),
                                ms(at.elapsed())
                            )
                        })
                        .unwrap_or_default();
                    let why = if is_error {
                        let text: String = result
                            .content
                            .iter()
                            .filter_map(|c| match c {
                                Content::Text { text } => Some(text.as_str()),
                                _ => None,
                            })
                            .collect();
                        format!(": {}", text.chars().take(160).collect::<String>())
                    } else {
                        String::new()
                    };
                    println!(
                        "  {} {tool_name}{took}{why}",
                        if is_error { "✗" } else { "✓" }
                    );
                }
                AgentEvent::MessageEnd {
                    message: AgentMessage::Llm(Message::Assistant { content, .. }),
                } => {
                    asked = Some(Instant::now());
                    for c in &content {
                        if let Content::Text { text } = c {
                            if !text.trim().is_empty() {
                                println!("{}", text.trim());
                            }
                        }
                    }
                }
                _ => {}
            },
            ServerMessage::UiRequest { id, request } => {
                println!("  ? (to the owner) {}", request.title());
                question = Some(id);
            }
            ServerMessage::Notice { message, .. } => {
                println!("  [{message}]");
                facts.push(message);
            }
            ServerMessage::RunEnd { outcome, error, .. } => {
                ended += 1;
                println!(
                    "  — run {ended} ended: {outcome:?}{}",
                    error.map(|e| format!(" ({e})")).unwrap_or_default()
                );
                if ended == 1 {
                    // The owner comes back, and answers.
                    let id = question.ok_or("the agent asked the owner nothing")?;
                    println!("  ! (the owner answers #1) Bullet list");
                    session.send(ClientMessage::UiResponse {
                        id,
                        value: json!("Bullet list"),
                    });
                }
            }
            _ => {}
        }
    }
    drop(owner);
    session.send(ClientMessage::Quit);
    let agent = tokio::time::timeout(Duration::from_secs(30), running)
        .await
        .map_err(|_| "the session did not end")??;
    let answered = agent.messages().iter().any(|m| {
        matches!(m, AgentMessage::Llm(Message::User { content, .. })
            if content.iter().any(|c| matches!(c, Content::Text { text } if text.starts_with("[Owner's answer to #1]"))))
    });
    if !answered {
        return Err("the owner's answer never reached the agent".into());
    }
    if facts.iter().filter(|f| f.starts_with("Facts:")).count() != 2 {
        return Err(format!("expected the facts after each run: {facts:?}").into());
    }
    // What the reports claim was done.
    if !std::path::Path::new("notes.md").exists() {
        return Err("the second run did not write notes.md".into());
    }
    if !facts.last().is_some_and(|f| f.contains("new: notes.md")) {
        return Err(format!("the facts do not name notes.md: {facts:?}").into());
    }
    println!("ok: asked without waiting, answered later, wrote notes.md, two reports");
    Ok(())
}
