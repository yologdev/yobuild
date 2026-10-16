//! The `yobuild` binary: its arguments, what `--once` prints, its exit
//! codes, and what needs a process of its own — the working folder (the
//! agent's, read by the file tools, the gate and facts) and the environment
//! (a missing key). Unix (the fixture's MCP server is bash).
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/echo");

fn yobuild(args: &[&str], cwd: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_yobuild"))
        .args(args)
        .current_dir(cwd)
        .env_remove("RUST_LOG")
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The fixture's manifest and script, by path.
fn fixture() -> (String, String) {
    (
        format!("{FIXTURE}/agent.toml"),
        format!("{FIXTURE}/script.json"),
    )
}

#[test]
fn a_once_run_prints_its_answer_and_how_it_ended() {
    let (agent, script) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let out = yobuild(
        &["run", &agent, "--scripted", &script, "--once", "echo", "hi"],
        dir.path(),
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "done\nCompleted\n");
    let stderr = text(&out.stderr);
    assert!(stderr.contains("  echo ✓"), "each tool call: {stderr}");
}

#[test]
fn arguments_are_checked_before_anything_runs() {
    let (agent, script) = fixture();
    let here = Path::new(FIXTURE);
    let usage = yobuild(&[], here);
    assert!(usage.status.success() && text(&usage.stdout).starts_with("usage: yobuild run"));
    let help = yobuild(&["run", "--help"], here);
    assert!(help.status.success() && text(&help.stdout).starts_with("usage: yobuild run"));
    for (args, error) in [
        (vec!["frob"], "yobuild: unknown command \"frob\""),
        (vec!["run"], "yobuild: usage: yobuild run"),
        (
            vec!["run", &agent, "--once"],
            "yobuild: --once needs a task",
        ),
        (
            vec!["run", &agent, "--once", " "],
            "yobuild: --once needs a task",
        ),
        (
            vec!["run", &agent, "--frob"],
            "yobuild: unknown option --frob",
        ),
        (
            vec!["run", &agent, "--web", "nowhere"],
            "yobuild: --web nowhere: ",
        ),
        (
            vec!["run", &agent, "--web", "127.0.0.1:0", "--once", "x"],
            "yobuild: --web: this run has no web frontend",
        ),
        (vec!["run", "missing.toml"], "yobuild: missing.toml"),
    ] {
        let out = yobuild(&args, here);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        let stderr = text(&out.stderr);
        assert!(stderr.starts_with(error), "{args:?}: {stderr}");
    }
    // `--` ends the options: the rest is the task.
    let out = yobuild(
        &[
            "run", &agent, "--scripted", &script, "--once", "--", "--echo",
        ],
        here,
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
}

#[test]
fn a_missing_key_stops_the_run_before_it_starts() {
    let (agent, _) = fixture();
    let out = Command::new(env!("CARGO_BIN_EXE_yobuild"))
        .args(["run", &agent, "--once", "x"])
        .env_remove("DEEPSEEK_API_KEY")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        "yobuild: deepseek/deepseek-flash needs its key in DEEPSEEK_API_KEY\n"
    );
}

/// A project in a git repository, with a manifest and a script.
fn project(manifest: &str, script: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let ok = Command::new("git")
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(args)
            .current_dir(dir.path())
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["commit", "-q", "--allow-empty", "-m", "start"]);
    std::fs::write(
        dir.path().join("agent.toml"),
        format!("[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n{manifest}"),
    )
    .unwrap();
    std::fs::write(dir.path().join("script.json"), script).unwrap();
    dir
}

#[test]
fn the_agent_works_in_the_current_folder_and_facts_say_what_it_made() {
    let dir = project(
        "[parts]\ngate = \"off\"\nfacts = true\n",
        r#"[{ "tools": [{ "name": "write_file", "arguments": { "path": "notes.md", "content": "hi\n" } }] }, { "text": "done" }]"#,
    );
    let out = yobuild(
        &[
            "run", "agent.toml", "--scripted", "script.json", "--once", "write",
        ],
        dir.path(),
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    // A relative path: the file tools read it from the current folder.
    assert_eq!(
        std::fs::read_to_string(dir.path().join("notes.md")).unwrap(),
        "hi\n"
    );
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("Info: Facts: since the start:") && stderr.contains("new: notes.md"),
        "{stderr}"
    );
}

#[test]
fn the_gate_judges_the_current_folder() {
    // The gate is on: a new file inside the project is free; one outside asks
    // and, with nobody there, is refused.
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("x.md");
    let dir = project(
        "",
        &format!(
            r#"[{{ "tools": [{{ "name": "write_file", "arguments": {{ "path": "inside.md", "content": "a" }} }}] }},
               {{ "tools": [{{ "name": "write_file", "arguments": {{ "path": "{}", "content": "b" }} }}] }},
               {{ "text": "done" }}]"#,
            target.display()
        ),
    );
    let out = yobuild(
        &[
            "run", "agent.toml", "--scripted", "script.json", "--once", "write",
        ],
        dir.path(),
    );
    let stderr = text(&out.stderr);
    assert!(dir.path().join("inside.md").exists(), "{stderr}");
    assert!(!target.exists(), "{stderr}");
    assert!(
        stderr.contains("  write_file ✓")
            && stderr.contains("  write_file ✗ Tool call denied: not allowed"),
        "{stderr}"
    );
}

#[test]
fn a_run_that_did_not_complete_exits_non_zero() {
    let echo =
        r#"{ "tools": [{ "name": "write_file", "arguments": { "path": "a", "content": "a" } }] }"#;
    let dir = project(
        "[limits]\nmax_turns = 1\n[parts]\ngate = \"off\"\n",
        &format!("[{echo}, {echo}, {{ \"text\": \"done\" }}]"),
    );
    let out = yobuild(
        &[
            "run", "agent.toml", "--scripted", "script.json", "--once", "go",
        ],
        dir.path(),
    );
    assert_eq!(out.status.code(), Some(1));
    let stdout = text(&out.stdout);
    assert!(
        stdout.lines().last().unwrap().starts_with("Stopped: "),
        "{stdout}"
    );
}

/// Whether pi's runtime is installed (`scripts/copy-adapters.sh`, `npm ci`
/// in `adapters/pi`); required with `YOAGENT_RUTIS_REQUIRE_RUNTIMES=1`.
fn pi_installed() -> bool {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("adapters/pi/node_modules");
    if dir.is_dir() {
        return true;
    }
    if std::env::var_os("YOAGENT_RUTIS_REQUIRE_RUNTIMES").is_some_and(|v| v == "1") {
        panic!("pi's runtime is required: scripts/copy-adapters.sh, then npm ci in adapters/pi");
    }
    eprintln!("SKIPPED: scripts/copy-adapters.sh, then npm ci in adapters/pi");
    false
}

// pi plugins run in the binary, in a folder of their own: the fixture
// extension writes `shutdown.txt` in its working folder when it stops.

#[test]
fn a_pi_extension_file_from_the_manifest_gives_its_tool() {
    if !pi_installed() {
        return;
    }
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("adapters/pi/fixture-extension.ts");
    let dir = project(
        &format!(
            "[[plugin]]\npi = \"{}\"\n[parts]\ngate = \"off\"\n",
            fixture.display()
        ),
        r#"[{ "tools": [{ "name": "pi_echo", "arguments": { "text": "hi" } }] }, { "text": "done" }]"#,
    );
    let out = yobuild(
        &[
            "run", "agent.toml", "--scripted", "script.json", "--once", "go",
        ],
        dir.path(),
    );
    let stderr = text(&out.stderr);
    assert!(
        out.status.success() && stderr.contains("  pi_echo ✓"),
        "{stderr}"
    );
    // pi works in the agent's folder.
    assert!(dir.path().join("shutdown.txt").exists(), "{stderr}");
}

#[test]
fn a_pinned_pi_package_is_found_beside_the_manifest() {
    if !pi_installed() {
        return;
    }
    let dir = project(
        "",
        r#"[{ "tools": [{ "name": "pi_echo", "arguments": { "text": "hi" } }] }, { "text": "done" }]"#,
    );
    let package = dir.path().join("node_modules/fake-pi");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("package.json"),
        r#"{ "name": "fake-pi", "version": "1.0.0", "type": "module", "pi": { "extensions": ["./ext.ts"] } }"#,
    )
    .unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("adapters/pi/fixture-extension.ts"),
        package.join("ext.ts"),
    )
    .unwrap();
    let manifest = |spec: &str| {
        std::fs::write(
            dir.path().join("agent.toml"),
            format!("[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n[[plugin]]\npi = \"{spec}\"\n[parts]\ngate = \"off\"\n"),
        )
        .unwrap()
    };
    let go = || {
        yobuild(
            &[
                "run", "agent.toml", "--scripted", "script.json", "--once", "go",
            ],
            dir.path(),
        )
    };
    manifest("fake-pi@1.0.0");
    let out = go();
    let stderr = text(&out.stderr);
    assert!(
        out.status.success() && stderr.contains("  pi_echo ✓"),
        "{stderr}"
    );
    manifest("fake-pi@1.0.1");
    let out = go();
    let stderr = text(&out.stderr);
    assert!(
        stderr.starts_with("yobuild: plugin fake-pi: fake-pi is installed at 1.0.0")
            && stderr.contains("not 1.0.1 (npm install fake-pi@1.0.1 in "),
        "{stderr}"
    );
}
