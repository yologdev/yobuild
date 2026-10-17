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
    assert!(
        stderr.contains("  server__echo ✓"),
        "each tool call: {stderr}"
    );
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

/// The web page's session, as a browser holds it: `yobuild run` in `dir`,
/// its URL read from stdout, connected over its WebSocket.
mod web {
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, Stdio};
    use std::time::Duration;

    use futures::StreamExt;
    use serde_json::Value as Json;
    use tokio_tungstenite::tungstenite::Message;

    pub struct Page {
        pub child: Child,
        socket: tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    }

    /// A test that fails before `interrupt` leaves no `yobuild` running.
    impl Drop for Page {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    impl Page {
        /// Starts it with `task`, connects once it serves.
        pub async fn open(dir: &std::path::Path, task: &str) -> Page {
            Self::open_after(dir, task, Duration::ZERO).await
        }

        /// The same, connecting `late` after it serves.
        pub async fn open_after(dir: &std::path::Path, task: &str, late: Duration) -> Page {
            let mut child = Command::new(env!("CARGO_BIN_EXE_yobuild"))
                .args(["run", "agent.toml", "--scripted", "script.json"])
                .args(["--web", "127.0.0.1:0", task])
                .current_dir(dir)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let mut out = BufReader::new(child.stdout.take().unwrap());
            let mut line = String::new();
            out.read_line(&mut line).unwrap();
            // Keep reading whatever else it prints to stdout.
            std::thread::spawn(move || std::io::copy(&mut out, &mut std::io::sink()));
            // "a: http://127.0.0.1:PORT/?t=TOKEN  (Ctrl+C to stop)"
            let url = line.split_whitespace().nth(1).expect("the URL");
            let ws = url.replace("http://", "ws://").replace("/?t=", "/ws?t=");
            tokio::time::sleep(late).await;
            let socket = match tokio_tungstenite::connect_async(ws).await {
                Ok((socket, _)) => socket,
                // Why it is not there: what it said before it went.
                Err(e) => {
                    use std::io::Read;
                    let _ = child.kill();
                    let _ = child.wait();
                    let mut said = String::new();
                    if let Some(mut err) = child.stderr.take() {
                        let _ = err.read_to_string(&mut said);
                    }
                    panic!("cannot connect to the page ({e}); yobuild said:\n{said}");
                }
            };
            Page { child, socket }
        }

        /// The next message of type `kind`.
        pub async fn until(&mut self, kind: &str) -> Json {
            let wait = async {
                while let Some(Ok(message)) = self.socket.next().await {
                    if let Message::Text(text) = message {
                        let message: Json = serde_json::from_str(&text).unwrap();
                        if message["type"] == kind {
                            return message;
                        }
                    }
                }
                panic!("the page closed before a {kind}")
            };
            tokio::time::timeout(Duration::from_secs(60), wait)
                .await
                .unwrap_or_else(|_| panic!("no {kind} in 60 s"))
        }

        /// Ctrl+C, then its exit code and stderr.
        pub fn interrupt(mut self) -> (Option<i32>, String) {
            use std::io::Read;
            // Read while it ends: a full pipe would keep it from ending.
            let mut err = self.child.stderr.take().expect("piped");
            let reading = std::thread::spawn(move || {
                let mut text = String::new();
                let _ = err.read_to_string(&mut text);
                text
            });
            let pid = self.child.id().to_string();
            Command::new("kill").args(["-INT", &pid]).status().unwrap();
            let status = self.child.wait().unwrap();
            (status.code(), reading.join().unwrap_or_default())
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_web_page_exits_by_how_the_last_run_ended() {
    // The last run completed: 0.
    let dir = project("[parts]\ngate = \"off\"\n", r#"[{ "text": "done" }]"#);
    let mut page = web::Page::open(dir.path(), "go").await;
    assert_eq!(page.until("runEnd").await["outcome"], "completed");
    let (code, stderr) = page.interrupt();
    assert_eq!(code, Some(0), "{stderr}");

    // Stopped by a limit: 1, and said.
    let echo =
        r#"{ "tools": [{ "name": "write_file", "arguments": { "path": "a", "content": "a" } }] }"#;
    let dir = project(
        "[limits]\nmax_turns = 1\n[parts]\ngate = \"off\"\n",
        &format!("[{echo}, {echo}, {{ \"text\": \"done\" }}]"),
    );
    let mut page = web::Page::open(dir.path(), "go").await;
    assert_eq!(page.until("runEnd").await["outcome"], "stopped");
    let (code, stderr) = page.interrupt();
    assert_eq!(code, Some(1), "{stderr}");
    assert!(
        stderr.contains("yobuild: the last run ended Stopped: "),
        "{stderr}"
    );

    // Ctrl+C during a run: aborted, 1.
    let dir = project(
        "[parts]\ngate = \"off\"\n",
        r#"[{ "tools": [{ "name": "bash", "arguments": { "command": "sleep 30" } }] }, { "text": "done" }]"#,
    );
    let mut page = web::Page::open(dir.path(), "go").await;
    page.until("runStart").await;
    let (code, stderr) = page.interrupt();
    assert_eq!(code, Some(1), "{stderr}");
    assert!(
        stderr.contains("yobuild: the last run ended Aborted: stopped with Ctrl+C"),
        "{stderr}"
    );
}

#[test]
fn a_plugin_tool_named_ask_owner_is_not_free() {
    if !pi_installed() {
        return;
    }
    // With the part off, a plugin's `ask_owner` is any other tool: the gate
    // asks, and with nobody there it does not run.
    let fixture = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("adapters/pi/fixture-extension.ts"),
    )
    .unwrap();
    let dir = project(
        "[[plugin]]\npi = \"./node_modules/fake/ext.ts\"\n",
        r#"[{ "tools": [{ "name": "ask_owner", "arguments": { "text": "hi" } }] }, { "text": "done" }]"#,
    );
    let ext = dir.path().join("node_modules/fake");
    std::fs::create_dir_all(&ext).unwrap();
    std::fs::write(
        ext.join("package.json"),
        r#"{ "name": "fake", "version": "1.0.0", "type": "module" }"#,
    )
    .unwrap();
    std::fs::write(
        ext.join("ext.ts"),
        fixture.replace("'pi_echo'", "'ask_owner'"),
    )
    .unwrap();
    let out = yobuild(
        &[
            "run", "agent.toml", "--scripted", "script.json", "--once", "go",
        ],
        dir.path(),
    );
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("  ask_owner ✗ Tool call denied: not allowed"),
        "{stderr}"
    );
}

#[test]
fn ctrl_c_stops_a_once_run_as_aborted() {
    let dir = project(
        "[parts]\ngate = \"off\"\n",
        r#"[{ "tools": [{ "name": "bash", "arguments": { "command": "sleep 30" } }] }, { "text": "done" }]"#,
    );
    let child = Command::new(env!("CARGO_BIN_EXE_yobuild"))
        .args([
            "run", "agent.toml", "--scripted", "script.json", "--once", "go",
        ])
        .current_dir(dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Ready, then the run under way.
    std::thread::sleep(std::time::Duration::from_secs(3));
    Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    let started = std::time::Instant::now();
    let out = child.wait_with_output().unwrap();
    assert!(started.elapsed().as_secs() < 20, "it stopped promptly");
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout).lines().last(),
        Some("Aborted: stopped with Ctrl+C")
    );
}

#[test]
fn facts_count_the_questions_left_open() {
    let dir = project(
        "[parts]\nask_owner = true\nfacts = true\n",
        r#"[{ "tools": [{ "name": "ask_owner", "arguments": { "question": "Which?" } }] }, { "text": "done" }]"#,
    );
    let out = yobuild(
        &[
            "run", "agent.toml", "--scripted", "script.json", "--once", "go",
        ],
        dir.path(),
    );
    let stderr = text(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    // Free (yobuild's own), asked, and counted as waiting.
    assert!(stderr.contains("  ask_owner ✓"), "{stderr}");
    assert!(
        stderr.contains("; 1 question(s) waiting on you."),
        "{stderr}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_task_waits_for_the_page_so_its_question_reaches_the_owner() {
    // Its first call asks (a file outside the project). Started before the
    // page opened, nobody would be there and it would be refused unseen.
    let outside = tempfile::tempdir().unwrap();
    let dir = project(
        "",
        &format!(
            r#"[{{ "tools": [{{ "name": "write_file", "arguments": {{ "path": "{}", "content": "x" }} }}] }}, {{ "text": "done" }}]"#,
            outside.path().join("x.md").display()
        ),
    );
    let mut page =
        web::Page::open_after(dir.path(), "go", std::time::Duration::from_millis(1500)).await;
    let asked = page.until("uiRequest").await;
    assert!(
        asked["request"]["title"]
            .as_str()
            .is_some_and(|t| t.contains("outside the project")),
        "{asked}"
    );
    let (code, stderr) = page.interrupt();
    assert_eq!(code, Some(1), "{stderr}");
}
