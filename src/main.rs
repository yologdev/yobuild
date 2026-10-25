//! `yobuild`: run an agent from its manifest, or check it.
//!
//! ```text
//! yobuild run <agent.toml> [--scripted FILE] [--once] [--measure] [--web ADDR] [--locked] [--] [TASK…]
//! yobuild check <agent.toml> [--locked] [--json]
//! yobuild new <folder> [--from <agent>]
//! yobuild new --list
//! yobuild skill install [--project | --dir <folder>] [--force]
//! yobuild --version
//! ```
//!
//! `new` makes an agent's folder (design §13): a starter whose every other
//! key is shown commented, or (`--from`) a copy of an agent built into
//! yobuild — its gallery's and its examples' (`--list`), named after the
//! folder. `skill install` writes the yobuild skill where a coding agent
//! reads it: `~/.claude/skills/yobuild/` (Claude Code), the project's
//! `.claude/skills/yobuild/`, or `--dir`; it updates a copy nobody edited
//! and refuses one someone changed (`--force`).
//!
//! `check` vets every plugin and MCP server without a model or a key (each
//! alone, then the agent together; design §4): a report on stdout, as text
//! or `--json`; the exit code is non-zero when anything has a problem.
//!
//! - `--scripted FILE`: the model's replies from a file (no provider, no key).
//! - `--once`: run TASK alone, without a frontend; each tool call and notice
//!   goes to stderr, then the agent's last answer and how the run ended
//!   (`Completed`, `Stopped: …`, `Error: …`) to stdout; the exit code is
//!   non-zero unless it completed.
//! - `--measure`: after each run, say each plugin's hook time and each
//!   plugin runtime's memory to every frontend (with `--once` always; else
//!   in the log, at info).
//! - `--locked`: refuse a lock (beside the manifest, `<stem>.lock`) with a
//!   set that is missing, stale, disagrees with itself or is no longer
//!   needed (CI; an agent someone checked), instead of resolving it again.
//!   Pinned packages are installed into `YOBUILD_HOME` (default
//!   `~/.yobuild`).
//! - `--web ADDR`: where the web frontend listens, for a manifest whose
//!   frontend is the web page (default 127.0.0.1:8787; 0.0.0.0:8787 with
//!   `phone`).
//!
//! The exit code is non-zero unless the last run completed (or none was
//! made): with `--once` the task's run, with the web page or the terminal
//! UI the session's last — a run Ctrl+C stopped is aborted, and a terminal
//! UI that stopped on its own is an error.
//!
//! The agent works in the current folder. Its log goes to stderr
//! (`RUST_LOG`, default warnings), or under a terminal UI to
//! `$TMPDIR/yobuild-<agent>-<pid>.log` (named at the end, failed or not).

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use rutis::Ctx;
use yoagent::{AgentMessage, Content, Message};
use yobuild::frontend::RunOutcome;
use yobuild::manifest::Manifest;
use yobuild::run::{run, Layout, Options};

/// What yobuild says on stderr while it gets ready (an install of its own
/// runtime files).
fn say(text: &str) {
    eprintln!("yobuild: {text}");
}

/// yobuild's own runtime files: the cache's (design §13), or — with
/// `YOBUILD_RUNTIMES=checkout`, for developing yobuild — the checkout it was
/// built from, whose `adapters/` holds the fixtures a test loads beside the
/// runtime that must resolve their imports.
fn layout(manifest: &Manifest, terminal: bool) -> Result<Layout, BoxError> {
    // Unset or empty: the cache (as `YOBUILD_HOME` reads an empty one).
    match std::env::var_os("YOBUILD_RUNTIMES").filter(|v| !v.is_empty()) {
        None => Layout::installed(manifest, None, terminal, &say),
        Some(v) if v == "checkout" => Ok(Layout::checkout()),
        Some(other) => Err(format!(
            "YOBUILD_RUNTIMES={:?}: only \"checkout\" is known (or leave it unset)",
            other.to_string_lossy()
        )
        .into()),
    }
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const USAGE: &str = "usage: yobuild run <agent.toml> [--scripted FILE] [--once] [--measure] [--web ADDR] [--locked] [--] [TASK…]
       yobuild check <agent.toml> [--locked] [--json]
       yobuild new <folder> [--from <agent>]
       yobuild new --list
       yobuild skill install [--project | --dir <folder>] [--force]
       yobuild --version | -V";

fn main() -> ExitCode {
    match start() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("yobuild: {e}");
            if let Some(fix) = yobuild::run::what_to_do_at_start(&e.to_string()) {
                eprintln!("  what to do: {fix}");
            }
            ExitCode::FAILURE
        }
    }
}

fn start() -> Result<ExitCode, BoxError> {
    let mut args = std::env::args().skip(1);
    let command = args.next();
    // Not tested on Windows yet: no kept environment there, a terminal UI
    // that reads /dev/tty (design §13 "Distribution"). `new` and `skill`
    // only write files, and run anywhere.
    if cfg!(windows) && matches!(command.as_deref(), Some("run" | "check")) {
        return Err("Windows is not supported yet (yobuild 0.1: macOS and Linux)".into());
    }
    match command.as_deref() {
        Some("run") => {}
        Some("check") => return check(args),
        Some("new") => return new(args),
        Some("skill") => return skill(args),
        Some("-V" | "--version") => {
            println!("yobuild {}", env!("CARGO_PKG_VERSION"));
            return Ok(ExitCode::SUCCESS);
        }
        Some("-h" | "--help") | None => {
            println!("{USAGE}");
            return Ok(ExitCode::SUCCESS);
        }
        Some(other) => return Err(format!("unknown command {other:?}\n{USAGE}").into()),
    }
    let path = match args.next() {
        Some(help) if help == "-h" || help == "--help" => {
            println!("{USAGE}");
            return Ok(ExitCode::SUCCESS);
        }
        Some(path) if !path.starts_with('-') => PathBuf::from(path),
        _ => return Err(USAGE.into()),
    };
    let manifest = Manifest::load(&path)?;
    let mut options = Options::new(manifest);
    let mut task = Vec::new();
    let mut web = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--scripted" => options.scripted = Some(args.next().ok_or(USAGE)?.into()),
            "--once" => options.once = true,
            "--measure" => options.measure = true,
            "--locked" => options.locked = true,
            "--web" => {
                let addr = args.next().ok_or(USAGE)?;
                options.addr = Some(addr.parse().map_err(|e| format!("--web {addr}: {e}"))?);
                web = true;
            }
            "--" => task.extend(args.by_ref()),
            flag if flag.starts_with('-') => {
                return Err(format!("unknown option {flag}\n{USAGE}").into())
            }
            word => task.push(word.to_owned()),
        }
    }
    let task = task.join(" ");
    options.task = (!task.trim().is_empty()).then_some(task);
    if options.once && options.task.is_none() {
        return Err("--once needs a task".into());
    }
    let terminal = options.manifest.frontend.terminal && !options.once;
    if web && (options.once || terminal) {
        return Err("--web: this run has no web frontend (--once, or a terminal manifest)".into());
    }
    let log = logging(terminal.then_some(options.manifest.agent.name.as_str()))?;
    // yobuild's own runtime files, from the cache (design §13), once the
    // log is there for what installing them warns of.
    options.layout = layout(&options.manifest, terminal)?;
    let once = options.once;

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let root = Ctx::root()?;
        let ended = run(&root, options).await;
        // A runtime that does not stop is said after the run's own error;
        // then where the log is.
        let stopped = match tokio::time::timeout(Duration::from_secs(10), root.shutdown()).await {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(format!("shutting down: {e}")),
            Err(_) => Some("shutting down: plugin runtimes still running after 10 s".to_owned()),
        };
        let code = match ended {
            Err(e) => {
                eprintln!("yobuild: {e}");
                if let Some(fix) = yobuild::run::what_to_do_at_start(&e.to_string()) {
                    eprintln!("  what to do: {fix}");
                }
                ExitCode::FAILURE
            }
            // The last run's answer (`--once`: the task's) and how it ended.
            Ok(ended) => {
                let answer = ended.agent.as_ref().and_then(|a| last_answer(a.messages()));
                if let (true, Some(answer)) = (once, answer) {
                    println!("{answer}");
                }
                // (`--once` printed what to do among its notices.)
                let fix = ended
                    .outcome
                    .and_then(|o| yobuild::run::what_to_do(o, ended.error.as_deref()));
                let error = ended.error.map(|e| format!(": {e}")).unwrap_or_default();
                match (ended.outcome, once) {
                    (Some(outcome), true) => println!("{outcome:?}{error}"),
                    (Some(RunOutcome::Completed) | None, false) | (None, true) => {}
                    (Some(outcome), false) => {
                        eprintln!("yobuild: the last run ended {outcome:?}{error}");
                        if let Some(fix) = fix {
                            eprintln!("  what to do: {fix}");
                        }
                    }
                }
                // What the agent loaded, once the runtimes (the terminal
                // UI among them) have stopped: how to keep it.
                if let Some(loaded) = &ended.loaded {
                    eprintln!("{loaded}");
                }
                match ended.outcome {
                    Some(RunOutcome::Completed) | None => ExitCode::SUCCESS,
                    Some(_) => ExitCode::FAILURE,
                }
            }
        };
        if let Some(stopped) = stopped {
            eprintln!("yobuild: {stopped}");
        }
        if let Some(log) = log {
            eprintln!("yobuild: log in {}", log.display());
        }
        Ok(code)
    })
}

/// `yobuild new <folder> [--from <agent>]`: a new agent's folder, the
/// starter or a built-in agent copied (`yobuild::agents`); `--list` names
/// the built-in ones.
fn new(args: impl Iterator<Item = String>) -> Result<ExitCode, BoxError> {
    use yobuild::agents::{new_agent, BUILTIN};
    let args: Vec<String> = args.collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    if args == ["--list"] {
        for kind in ["gallery", "example"] {
            println!("{kind}:");
            for agent in BUILTIN.iter().filter(|a| a.kind == kind) {
                println!("  {:<18} {}", agent.name, agent.summary);
            }
        }
        return Ok(ExitCode::SUCCESS);
    }
    let (folder, from) = match args.as_slice() {
        [folder] if !folder.starts_with('-') => (folder, None),
        [folder, flag, from] if flag == "--from" && !folder.starts_with('-') => {
            (folder, Some(from.as_str()))
        }
        _ => return Err(USAGE.into()),
    };
    let dir = PathBuf::from(folder);
    let made = new_agent(&dir, from)?;
    // Absolute: the commands below run from the folder the agent works in.
    let dir = std::path::absolute(&dir)?;
    match from {
        Some(from) => println!("made {} from the built-in agent `{from}`:", dir.display()),
        None => println!("made {}, a starter to fill in:", dir.display()),
    }
    for file in &made.files {
        println!("  {file}");
    }
    for change in &made.changed {
        println!("  changed: {change}");
    }
    let manifest = dir.join("agent.toml");
    let demo = dir.join("scripts/demo.json");
    let task = dir.join("scripts/demo.task");
    println!(
        "next, from the folder it should work in (its own, or a project's):\n  yobuild check {m}\n  yobuild run {m} --scripted {d} --once \"$(cat {t})\"   # no key needed\n  DEEPSEEK_API_KEY=… yobuild run {m}",
        m = manifest.display(),
        d = demo.display(),
        t = task.display()
    );
    if from.is_some() && dir.join("README.md").is_file() {
        println!(
            "its README says what it needs and does: {}",
            dir.join("README.md").display()
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// `yobuild skill install [--project | --dir <folder>] [--force]`: the
/// yobuild skill where a coding agent reads it (`yobuild::skill`).
fn skill(args: impl Iterator<Item = String>) -> Result<ExitCode, BoxError> {
    use yobuild::skill::{install, Done, Target};
    let args: Vec<String> = args.collect();
    let Some((command, rest)) = args.split_first() else {
        return Err(USAGE.into());
    };
    if command == "-h" || command == "--help" {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    if command != "install" {
        return Err(format!("unknown command skill {command:?}\n{USAGE}").into());
    }
    let mut target = Target::User;
    let mut force = false;
    let mut rest = rest.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--project" if target == Target::User => target = Target::Project,
            "--dir" if target == Target::User => {
                target = Target::Dir(rest.next().ok_or(USAGE)?.into())
            }
            "--force" => force = true,
            "--project" | "--dir" => {
                return Err(format!("one of --project and --dir\n{USAGE}").into())
            }
            other => return Err(format!("unknown option {other}\n{USAGE}").into()),
        }
    }
    let dir = target.folder()?;
    match install(&dir, force)? {
        Done::Installed => println!("installed the yobuild skill in {}", dir.display()),
        Done::Updated { from } => println!(
            "updated the yobuild skill in {} (it was yobuild {from}'s)",
            dir.display()
        ),
        Done::AlreadyThere => println!("the yobuild skill is in {} already", dir.display()),
        Done::Replaced => println!("wrote the yobuild skill afresh in {}", dir.display()),
        _ => println!("the yobuild skill is in {}", dir.display()),
    }
    if target == Target::User || target == Target::Project {
        println!(
            "Claude Code reads it there: ask it to build an agent (\"make me an agent that …\")."
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// `yobuild check`: the report on stdout (text, or JSON with `--json`);
/// exit 1 when anything has a problem.
fn check(mut args: impl Iterator<Item = String>) -> Result<ExitCode, BoxError> {
    use std::io::Write;
    use yobuild::run::check::{check, CheckOptions};
    let path = match args.next() {
        Some(help) if help == "-h" || help == "--help" => {
            println!("{USAGE}");
            return Ok(ExitCode::SUCCESS);
        }
        Some(path) if !path.starts_with('-') => PathBuf::from(path),
        _ => return Err(USAGE.into()),
    };
    let mut options = CheckOptions::new(Manifest::load(&path)?);
    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--locked" => options.locked = true,
            "--json" => json = true,
            other => return Err(format!("unknown option {other}\n{USAGE}").into()),
        }
    }
    logging(None)?;
    // As `run`'s: after the options are read and the log is there.
    options.layout = layout(&options.manifest, false)?;
    let runtime = tokio::runtime::Runtime::new()?;
    let report = match runtime.block_on(check(options)) {
        Ok(report) => report,
        // As `run`'s refusals: the fix when yobuild knows one.
        Err(e) => {
            eprintln!("yobuild: {e}");
            if let Some(fix) = yobuild::run::what_to_do_at_start(&e.to_string()) {
                eprintln!("  what to do: {fix}");
            }
            return Ok(ExitCode::FAILURE);
        }
    };
    // Written so that a reader gone (a closed pipe) is not a panic.
    let text = match json {
        true => format!("{}\n", serde_json::to_string_pretty(&report)?),
        false => report.text(),
    };
    // A reader gone (a closed pipe) is not an error; any other failure is.
    if let Err(e) = std::io::stdout().write_all(text.as_bytes()) {
        if e.kind() != std::io::ErrorKind::BrokenPipe {
            return Err(format!("writing the report: {e}").into());
        }
    }
    Ok(match report.ok {
        true => ExitCode::SUCCESS,
        false => ExitCode::FAILURE,
    })
}

/// The log: stderr, or a file when a terminal UI owns the screen (`agent`).
fn logging(agent: Option<&str>) -> Result<Option<PathBuf>, BoxError> {
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::EnvFilter;
    // The registry warns of no key for `--scripted`'s mock provider, which
    // needs none.
    let default = "warn,yoagent::provider::registry=error";
    let filter = match std::env::var("RUST_LOG") {
        Ok(set) => EnvFilter::try_new(&set).unwrap_or_else(|e| {
            eprintln!("yobuild: RUST_LOG={set:?} is not a log filter ({e}); using {default:?}");
            EnvFilter::new(default)
        }),
        Err(_) => EnvFilter::new(default),
    };
    // Beside the log, whatever its filter: what plugins log that the
    // session says (`run::plugin_log`).
    let registry = tracing_subscriber::registry().with(yobuild::run::plugin_log::layer());
    let Some(agent) = agent else {
        let log = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);
        registry.with(log.with_filter(filter)).init();
        return Ok(None);
    };
    // One per process: two runs of one agent keep their own.
    let path = std::env::temp_dir().join(format!("yobuild-{agent}-{}.log", std::process::id()));
    let file = std::fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let log = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(std::sync::Mutex::new(file));
    registry.with(log.with_filter(filter)).init();
    Ok(Some(path))
}

/// The text of the agent's last answer.
fn last_answer(messages: &[AgentMessage]) -> Option<String> {
    messages.iter().rev().find_map(|m| match m {
        AgentMessage::Llm(Message::Assistant { content, .. }) => {
            let text: Vec<&str> = content
                .iter()
                .filter_map(|c| match c {
                    Content::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            (!text.is_empty()).then(|| text.join("\n"))
        }
        _ => None,
    })
}
