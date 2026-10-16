//! `yobuild`: run an agent from its manifest.
//!
//! ```text
//! yobuild run <agent.toml> [--scripted FILE] [--once] [--web ADDR] [TASK…]
//! ```
//!
//! - `--scripted FILE`: the model's replies from a file (no provider, no key).
//! - `--once`: run TASK alone, without a frontend, print how it ended, exit
//!   (non-zero unless it completed).
//! - `--web ADDR`: where the web frontend listens (default 127.0.0.1:8787).

use std::path::PathBuf;
use std::process::ExitCode;

use rutis::Ctx;
use yobuild::frontend::RunOutcome;
use yobuild::manifest::Manifest;
use yobuild::run::{run, Options};

const USAGE: &str =
    "usage: yobuild run <agent.toml> [--scripted FILE] [--once] [--web ADDR] [TASK…]";

fn main() -> ExitCode {
    match start() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("yobuild: {e}");
            ExitCode::FAILURE
        }
    }
}

fn start() -> Result<ExitCode, Box<dyn std::error::Error + Send + Sync>> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("run") => {}
        Some("-h" | "--help") | None => {
            println!("{USAGE}");
            return Ok(ExitCode::SUCCESS);
        }
        Some(other) => return Err(format!("unknown command {other:?}\n{USAGE}").into()),
    }
    let path = PathBuf::from(args.next().ok_or(USAGE)?);
    let manifest = Manifest::load(&path)?;
    let mut options = Options::new(manifest, std::env::current_dir()?);
    let mut task = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--scripted" => options.scripted = Some(args.next().ok_or(USAGE)?.into()),
            "--once" => options.once = true,
            "--web" => options.addr = Some(args.next().ok_or(USAGE)?.parse()?),
            flag if flag.starts_with("--") => {
                return Err(format!("unknown option {flag}\n{USAGE}").into())
            }
            word => task.push(word.to_owned()),
        }
    }
    if !task.is_empty() {
        options.task = Some(task.join(" "));
    }
    if options.once && options.task.is_none() {
        return Err("--once needs a task".into());
    }
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let root = Ctx::root()?;
        let ended = run(&root, options).await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(10), root.shutdown()).await;
        Ok(match ended? {
            Some(ended) => {
                println!(
                    "{:?}{}",
                    ended.outcome,
                    ended.error.map(|e| format!(": {e}")).unwrap_or_default()
                );
                match ended.outcome {
                    RunOutcome::Completed => ExitCode::SUCCESS,
                    _ => ExitCode::FAILURE,
                }
            }
            None => ExitCode::SUCCESS,
        })
    })
}
