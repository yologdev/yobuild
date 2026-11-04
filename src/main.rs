//! `yobuild`: run an agent from its manifest, or check it.
//!
//! ```text
//! yobuild run <agent.toml> [--scripted FILE] [--once [--report FILE]] [--measure] [--web ADDR] [--locked] [--resume] [--] [TASK…]
//! yobuild check <agent.toml> [--locked | --static] [--json]
//! yobuild log <agent.toml> [-n N] [--json]
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
//! `--static` starts nothing (no Node, npm, runtime or server): the
//! manifest, its prompt and local files, each entry as written (design
//! §18).
//!
//! - `--scripted FILE`: the model's replies from a file (no provider, no key).
//! - `--once`: run TASK alone, without a frontend; each tool call and notice
//!   goes to stderr, then the agent's last answer and how the run ended
//!   (`Completed`, `Stopped: …`, `Error: …`) to stdout; the exit code is
//!   non-zero unless it completed.
//! - `--report FILE` (with `--once`): the agent's last answer written to
//!   FILE, whole or not at all (a run that gave none: a line saying how it
//!   ended) — for a run started by cron or launchd (design §16).
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
//! Every run is logged as it ends (design §16): when, what it was asked,
//! how it ended, its cost, its tools, the facts, the questions waiting on
//! the owner and its report, under `YOBUILD_HOME` (default `~/.yobuild`),
//! `logs/`. `log` prints an agent's last runs (`-n`, default 10), or
//! `--json`.
//!
//! The agent works in the current folder. Its log goes to stderr
//! (`RUST_LOG`, default warnings), or under a terminal UI to
//! `$TMPDIR/yobuild-<agent>-<pid>.log` (named at the end, failed or not).

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use rutis::host::CtxHost;
use yoagent::{AgentMessage, Content, Message};
use yobuild::frontend::RunOutcome;
use yobuild::manifest::Manifest;
use yobuild::run::{run, Layout, Options};

/// `println!`, but a reader gone (`yobuild new --list | head`, a launcher
/// that read the URL and closed the pipe) is no panic: what is left of the
/// text is dropped. Any other failure to write panics, as `println!` does.
macro_rules! out {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        if let Err(e) = writeln!(std::io::stdout(), $($arg)*) {
            if e.kind() != std::io::ErrorKind::BrokenPipe {
                panic!("failed printing to stdout: {e}");
            }
        }
    }};
}

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

const USAGE: &str = "usage: yobuild run <agent.toml> [--scripted FILE] [--once [--report FILE]] [--measure] [--web ADDR] [--locked] [--resume] [--] [TASK…]
       yobuild check <agent.toml> [--locked | --static] [--json]
       yobuild log <agent.toml> [-n N] [--json]
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
        Some("log") => return log(args),
        Some("new") => return new(args),
        Some("skill") => return skill(args),
        Some("-V" | "--version") => {
            out!("yobuild {}", env!("CARGO_PKG_VERSION"));
            return Ok(ExitCode::SUCCESS);
        }
        Some("-h" | "--help") | None => {
            out!("{USAGE}");
            return Ok(ExitCode::SUCCESS);
        }
        Some(other) => return Err(format!("unknown command {other:?}\n{USAGE}").into()),
    }
    let path = match args.next() {
        Some(help) if help == "-h" || help == "--help" => {
            out!("{USAGE}");
            return Ok(ExitCode::SUCCESS);
        }
        Some(path) if !path.starts_with('-') => PathBuf::from(path),
        _ => return Err(USAGE.into()),
    };
    let manifest = Manifest::load(&path)?;
    let mut options = Options::new(manifest);
    let mut task = Vec::new();
    let mut web = false;
    let mut report: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--scripted" => options.scripted = Some(args.next().ok_or(USAGE)?.into()),
            "--once" => options.once = true,
            "--report" => {
                if report.is_some() {
                    return Err("--report given twice".into());
                }
                let file = args.next().ok_or(USAGE)?;
                if file.starts_with('-') {
                    return Err(
                        format!("--report {file}: give the report's file after --report").into(),
                    );
                }
                report = Some(file.into());
            }
            "--measure" => options.measure = true,
            "--locked" => options.locked = true,
            "--resume" => options.resume = true,
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
    if report.is_some() && !options.once {
        return Err("--report needs --once (the task's run's answer is what it writes)".into());
    }
    // Its folder, before anything runs: a report that cannot be written is
    // a run wasted.
    if let Some(path) = &report {
        let folder = folder_of(path);
        if !folder.is_dir() {
            return Err(format!(
                "--report {}: its folder {} is not there",
                path.display(),
                folder.display()
            )
            .into());
        }
    }
    let terminal = options.manifest.frontend.terminal && !options.once;
    if web && (options.once || terminal) {
        return Err("--web: this run has no web frontend (--once, or a terminal manifest)".into());
    }
    let ran = run_and_say(options, &path, report.as_deref(), terminal);
    // Whatever stopped it before the run (no node on PATH under cron, say):
    // no old report left standing as this run's.
    if let (Err(e), Some(path)) = (&ran, &report) {
        stopped_report(path, &e.to_string());
    }
    ran
}

/// `run` once its arguments are read: the log, yobuild's runtime files, the
/// run, then what it says; `report` written as the run ends.
fn run_and_say(
    mut options: Options,
    path: &std::path::Path,
    report: Option<&std::path::Path>,
    terminal: bool,
) -> Result<ExitCode, BoxError> {
    let log = logging(terminal.then_some(options.manifest.agent.name.as_str()))?;
    // yobuild's own runtime files, from the cache (design §13), once the
    // log is there for what installing them warns of.
    options.layout = layout(&options.manifest, terminal)?;
    let once = options.once;

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let root = yobuild::frontend::host::root()?;
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
                // No old report left standing as this run's.
                if let Some(path) = report {
                    stopped_report(path, &e.to_string());
                }
                ExitCode::FAILURE
            }
            // The last run's answer (`--once`: the task's) and how it ended.
            Ok(ended) => {
                let answer = ended.agent.as_ref().and_then(|a| last_answer(a.messages()));
                if let (true, Some(answer)) = (once, &answer) {
                    out!("{answer}");
                }
                // The report file, whatever the outcome: a run that gave no
                // answer says how it ended, so an old report is never left
                // standing as this run's.
                let written = report.map(|path| {
                    let text = match (&answer, ended.outcome) {
                        (Some(answer), _) => format!("{answer}\n"),
                        (None, Some(outcome)) => format!(
                            "No answer: the run ended {outcome:?}{}.\n",
                            ended
                                .error
                                .as_deref()
                                .map(|e| format!(": {e}"))
                                .unwrap_or_default()
                        ),
                        (None, None) => "No answer: no run was made.\n".to_owned(),
                    };
                    write_report(path, &text)
                });
                // (`--once` printed what to do among its notices.)
                let fix = ended
                    .outcome
                    .and_then(|o| yobuild::run::what_to_do(o, ended.error.as_deref()));
                let error = ended.error.map(|e| format!(": {e}")).unwrap_or_default();
                match (ended.outcome, once) {
                    (Some(outcome), true) => out!("{outcome:?}{error}"),
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
                if ended.saved.is_some() {
                    eprintln!(
                        "yobuild: the conversation is saved: `yobuild run {} --resume` continues it",
                        path.display()
                    );
                }
                match (ended.outcome, written) {
                    // A report that could not be written fails the run:
                    // what a scheduled run is for is missing.
                    (_, Some(Err(e))) => {
                        eprintln!("yobuild: --report {e}");
                        ExitCode::FAILURE
                    }
                    (Some(RunOutcome::Completed) | None, _) => ExitCode::SUCCESS,
                    (Some(_), _) => ExitCode::FAILURE,
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

/// The folder `path` is written in.
fn folder_of(path: &std::path::Path) -> &std::path::Path {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => std::path::Path::new("."),
    }
}

/// `text` written to `path` whole or not at all: a temporary file in its
/// folder (0600, as `tempfile` makes it), synced, renamed over it. Errors
/// name the path.
fn write_report(path: &std::path::Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    let failed = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    let mut temp = tempfile::NamedTempFile::new_in(folder_of(path)).map_err(|e| failed(&e))?;
    temp.write_all(text.as_bytes()).map_err(|e| failed(&e))?;
    temp.as_file().sync_all().map_err(|e| failed(&e))?;
    temp.persist(path).map_err(|e| failed(&e.error))?;
    Ok(())
}

/// The report of a run yobuild stopped before or as it started: why.
fn stopped_report(path: &std::path::Path, error: &str) {
    if let Err(e) = write_report(path, &format!("No answer: yobuild stopped: {error}\n")) {
        eprintln!("yobuild: --report {e}");
    }
}

/// `yobuild log <agent.toml> [-n N] [--json]`: the agent's last runs, as
/// its run log keeps them (design §16) — oldest first, so the latest is
/// last; `--json` the entries as logged.
fn log(mut args: impl Iterator<Item = String>) -> Result<ExitCode, BoxError> {
    use std::io::Write;
    use yobuild::run_log::RunLog;
    let path = match args.next() {
        Some(help) if help == "-h" || help == "--help" => {
            out!("{USAGE}");
            return Ok(ExitCode::SUCCESS);
        }
        Some(path) if !path.starts_with('-') => PathBuf::from(path),
        _ => return Err(USAGE.into()),
    };
    let manifest = Manifest::load(&path)?;
    let (mut n, mut json) = (10, false);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json = true,
            "-n" => {
                let given = args.next().ok_or(USAGE)?;
                n = given
                    .parse()
                    .ok()
                    .filter(|&n| n > 0)
                    .ok_or_else(|| format!("-n {given}: a number of runs, 1 or more"))?;
            }
            other => return Err(format!("unknown option {other}\n{USAGE}").into()),
        }
    }
    let log = RunLog::of(&manifest, None)?;
    let read = log.read(n)?;
    let text = match json {
        true => format!("{}\n", serde_json::to_string_pretty(&read.entries)?),
        false if read.entries.is_empty() => format!(
            "no runs of {} logged yet (in {})\n",
            manifest.agent.name,
            log.dir().display()
        ),
        false => format!(
            "the last {} run(s) of {} (in {}):\n{}",
            read.entries.len(),
            manifest.agent.name,
            log.dir().display(),
            yobuild::run_log::text(&read.entries, 6)
        ),
    };
    if read.unread > 0 {
        eprintln!(
            "yobuild: {} line(s) of the log could not be read (another yobuild's, or broken): skipped",
            read.unread
        );
    }
    // A reader gone (a closed pipe) is not an error; any other failure is.
    if let Err(e) = std::io::stdout().write_all(text.as_bytes()) {
        if e.kind() != std::io::ErrorKind::BrokenPipe {
            return Err(format!("writing the log: {e}").into());
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `yobuild new <folder> [--from <agent>]`: a new agent's folder, the
/// starter or a built-in agent copied (`yobuild::agents`); `--list` names
/// the built-in ones.
fn new(args: impl Iterator<Item = String>) -> Result<ExitCode, BoxError> {
    use yobuild::agents::{new_agent, BUILTIN};
    let args: Vec<String> = args.collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        out!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    if args == ["--list"] {
        let width = BUILTIN.iter().map(|a| a.name.len()).max().unwrap_or(0);
        for kind in ["gallery", "example"] {
            out!("{kind}:");
            for agent in BUILTIN.iter().filter(|a| a.kind == kind) {
                out!("  {:<width$} {}", agent.name, agent.summary);
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
        Some(from) => out!("made {} from the built-in agent `{from}`:", dir.display()),
        None => out!("made {}, a starter to fill in:", dir.display()),
    }
    for file in &made.files {
        out!("  {file}");
    }
    for change in &made.changed {
        out!("  changed: {change}");
    }
    // A rutis plugin of its own runs in its package, which yobuild does not
    // install: `npm ci` in each one copied with its lock, first.
    for file in &made.files {
        if let Some(package) = file.strip_suffix("package-lock.json") {
            out!(
                "first, install its plugin's package (once):\n  (cd {} && npm ci --ignore-scripts)",
                dir.join(package.trim_end_matches('/')).display()
            );
        }
    }
    let manifest = dir.join("agent.toml");
    let demo = dir.join("scripts/demo.json");
    let task = dir.join("scripts/demo.task");
    out!(
        "next, from the folder it should work in (its own, or a project's):\n  yobuild check {m}\n  yobuild run {m} --scripted {d} --once \"$(cat {t})\"   # no key needed\n  DEEPSEEK_API_KEY=… yobuild run {m}",
        m = manifest.display(),
        d = demo.display(),
        t = task.display()
    );
    if from.is_some() && dir.join("README.md").is_file() {
        out!(
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
        out!("{USAGE}");
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
        Done::Installed => out!("installed the yobuild skill in {}", dir.display()),
        Done::Updated { from } => out!(
            "updated the yobuild skill in {} (it was yobuild {from}'s)",
            dir.display()
        ),
        Done::AlreadyThere => out!("the yobuild skill is in {} already", dir.display()),
        Done::Replaced => out!("wrote the yobuild skill afresh in {}", dir.display()),
        _ => out!("the yobuild skill is in {}", dir.display()),
    }
    if target == Target::User || target == Target::Project {
        out!("Claude Code reads it there: ask it to build an agent (\"make me an agent that …\").");
    }
    Ok(ExitCode::SUCCESS)
}

/// `yobuild check`: the report on stdout (text, or JSON with `--json`);
/// exit 1 when anything has a problem.
fn check(mut args: impl Iterator<Item = String>) -> Result<ExitCode, BoxError> {
    use std::io::Write;
    use yobuild::run::check::{check, check_static, CheckOptions};
    let path = match args.next() {
        Some(help) if help == "-h" || help == "--help" => {
            out!("{USAGE}");
            return Ok(ExitCode::SUCCESS);
        }
        Some(path) if !path.starts_with('-') => PathBuf::from(path),
        _ => return Err(USAGE.into()),
    };
    let mut options = CheckOptions::new(Manifest::load(&path)?);
    let mut json = false;
    let mut only_read = false;
    for arg in args {
        match arg.as_str() {
            "--locked" => options.locked = true,
            "--json" => json = true,
            "--static" => only_read = true,
            other => return Err(format!("unknown option {other}\n{USAGE}").into()),
        }
    }
    if only_read && options.locked {
        return Err(
            "--locked and --static: a static check installs nothing, so has no lock to hold to"
                .into(),
        );
    }
    logging(None)?;
    let runtime = tokio::runtime::Runtime::new()?;
    let checked = match only_read {
        true => runtime.block_on(async { check_static(&options.manifest) }),
        false => {
            // As `run`'s: after the options are read and the log is there.
            options.layout = layout(&options.manifest, false)?;
            // Ctrl+C (SIGHUP, SIGTERM) from the start: the servers `check`
            // starts run in process groups of their own, and are stopped
            // only when dropped.
            runtime.block_on(async {
                let mut stops = yobuild::stops::Stops::listen()?;
                stops.until(check(options)).await
            })
        }
    };
    let report = match checked {
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
    // needs none (a real provider's missing key `run` refuses first): kept
    // quiet under a `RUST_LOG` of the person's too, unless it names the
    // registry itself.
    const REGISTRY: &str = "yoagent::provider::registry";
    let quiet = format!("{REGISTRY}=error");
    let default = format!("warn,{quiet}");
    let filter = match std::env::var("RUST_LOG") {
        Ok(set) if set.trim().is_empty() => EnvFilter::new(&default),
        Ok(set) => {
            let with = match set.contains(REGISTRY) {
                true => set.clone(),
                false => format!("{set},{quiet}"),
            };
            EnvFilter::try_new(&with).unwrap_or_else(|e| {
                eprintln!("yobuild: RUST_LOG={set:?} is not a log filter ({e}); using {default:?}");
                EnvFilter::new(&default)
            })
        }
        Err(_) => EnvFilter::new(&default),
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

/// The text of the agent's last answer (a blank one is none).
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
            // A blank answer is none.
            let text = text.join("\n");
            (!text.trim().is_empty()).then_some(text)
        }
        _ => None,
    })
}
