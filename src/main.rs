//! `yobuild`: run an agent from its manifest.
//!
//! ```text
//! yobuild run <agent.toml> [--scripted FILE] [--once] [--web ADDR] [--] [TASK…]
//! ```
//!
//! - `--scripted FILE`: the model's replies from a file (no provider, no key).
//! - `--once`: run TASK alone, without a frontend; each tool call and notice
//!   goes to stderr, then the agent's last answer and how the run ended
//!   (`Completed`, `Stopped: …`, `Error: …`) to stdout; the exit code is
//!   non-zero unless it completed.
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
use yobuild::run::{run, Options};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const USAGE: &str =
    "usage: yobuild run <agent.toml> [--scripted FILE] [--once] [--web ADDR] [--] [TASK…]";

fn main() -> ExitCode {
    match start() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("yobuild: {e}");
            ExitCode::FAILURE
        }
    }
}

fn start() -> Result<ExitCode, BoxError> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("run") => {}
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
                ExitCode::FAILURE
            }
            // The last run's answer (`--once`: the task's) and how it ended.
            Ok(ended) => {
                let answer = ended.agent.as_ref().and_then(|a| last_answer(a.messages()));
                if let (true, Some(answer)) = (once, answer) {
                    println!("{answer}");
                }
                let error = ended.error.map(|e| format!(": {e}")).unwrap_or_default();
                match (ended.outcome, once) {
                    (Some(outcome), true) => println!("{outcome:?}{error}"),
                    (Some(RunOutcome::Completed) | None, false) | (None, true) => {}
                    (Some(outcome), false) => {
                        eprintln!("yobuild: the last run ended {outcome:?}{error}")
                    }
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

/// The log: stderr, or a file when a terminal UI owns the screen (`agent`).
fn logging(agent: Option<&str>) -> Result<Option<PathBuf>, BoxError> {
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
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    let Some(agent) = agent else {
        builder.with_writer(std::io::stderr).init();
        return Ok(None);
    };
    // One per process: two runs of one agent keep their own.
    let path = std::env::temp_dir().join(format!("yobuild-{agent}-{}.log", std::process::id()));
    let file = std::fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    builder
        .with_ansi(false)
        .with_writer(std::sync::Mutex::new(file))
        .init();
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
