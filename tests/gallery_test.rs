//! The gallery (`gallery/<name>/`, design §11): each agent `check`ed
//! `--locked`, and each of its reply files (`scripts/<x>.json`, with its task
//! in `scripts/<x>.task`) run scripted under `--once --locked`, in a temp
//! folder: `Completed`, every call it scripts made, none denied but those
//! the agent leaves asking. A tool that ran and failed on its own (`✗`, not
//! `Tool call denied: …`) is a third party's day, not the agent's: it does
//! not fail the test. Unix (as `cli_test`).
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The tests' cache of installed packages, shared with `cli_test`.
const CACHE: &str = concat!(env!("CARGO_TARGET_TMPDIR"), "/yobuild-home");

fn yobuild(args: &[&str], cwd: &Path, pi_home: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_yobuild"))
        .env("YOBUILD_HOME", CACHE)
        // A pi plugin's config and cache (`~/.pi/agent`): the test's own,
        // so a developer's config neither changes nor gets anything.
        .env("PI_CODING_AGENT_DIR", pi_home)
        .args(args)
        .current_dir(cwd)
        .env_remove("RUST_LOG")
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The agent's gallery folder, when the adapters it loads are installed
/// (required in CI).
fn agent(name: &str) -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    if ["adapters/pi/node_modules", "adapters/dsh/node_modules"]
        .iter()
        .all(|d| root.join(d).is_dir())
    {
        return Some(root.join("gallery").join(name));
    }
    if std::env::var_os("YOAGENT_RUTIS_REQUIRE_RUNTIMES").is_some_and(|v| v == "1") {
        panic!("the gallery needs the adapters: scripts/copy-adapters.sh, then npm ci in adapters/pi and adapters/dsh");
    }
    eprintln!("SKIPPED: scripts/copy-adapters.sh, then npm ci in adapters/pi and adapters/dsh");
    None
}

/// What a gallery agent is held to.
struct Expect<'a> {
    /// Tools the agent leaves asking: under `--once` nobody answers.
    asks: &'a [&'a str],
    /// Files put in the agent's folder before each run (path, contents).
    files: &'a [(&'a str, &'a str)],
}

/// `check --locked --json` → ok; then every reply file, scripted.
fn holds(dir: &Path, expect: Expect) {
    let manifest = dir.join("agent.toml").display().to_string();
    let pi_home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let out = yobuild(
        &["check", &manifest, "--locked", "--json"],
        work.path(),
        pi_home.path(),
    );
    let report: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("check's JSON ({e}): {}", text(&out.stderr)));
    assert_eq!(report["ok"], true, "{report:#}");

    let mut scripts: Vec<PathBuf> = std::fs::read_dir(dir.join("scripts"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    scripts.sort();
    assert!(!scripts.is_empty(), "{}: no reply file", dir.display());
    for script in scripts {
        let task_file = script.with_extension("task");
        let task = std::fs::read_to_string(&task_file)
            .unwrap_or_else(|e| panic!("{}: {e}", task_file.display()));
        let work = tempfile::tempdir().unwrap();
        for (path, contents) in expect.files {
            std::fs::write(work.path().join(path), contents).unwrap();
        }
        let out = yobuild(
            &[
                "run",
                &manifest,
                "--scripted",
                &script.display().to_string(),
                "--once",
                "--locked",
                task.trim(),
            ],
            work.path(),
            pi_home.path(),
        );
        let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
        let name = script.file_name().unwrap().to_string_lossy();
        assert!(
            out.status.success() && stdout.ends_with("Completed\n"),
            "{name}: {stdout}{stderr}"
        );
        let replies: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&script).unwrap()).unwrap();
        for call in replies
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["tools"].as_array())
            .flatten()
        {
            let tool = call["name"].as_str().unwrap();
            let denied = format!("  {tool} ✗ Tool call denied: ");
            if expect.asks.contains(&tool) {
                assert!(
                    stderr.contains(&format!(
                        "{denied}not allowed: the owner declined, or nobody answered"
                    )),
                    "{name}: {tool} should ask: {stderr}"
                );
            } else {
                assert!(
                    (stderr.contains(&format!("  {tool} ✓"))
                        || stderr.contains(&format!("  {tool} ✗ ")))
                        && !stderr.contains(&denied),
                    "{name}: {tool} should run: {stderr}"
                );
            }
        }
    }
}

#[test]
fn web_research() {
    let Some(dir) = agent("web-research") else {
        return;
    };
    holds(
        &dir,
        Expect {
            asks: &[],
            files: &[],
        },
    );
}
