//! The `yobuild` binary: its arguments, what `--once` prints, its exit
//! codes, and what needs a process of its own — the working folder (the
//! agent's, read by the file tools, the gate and facts) and the environment
//! (a missing key). Unix (the fixture's MCP server is bash).
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/echo");
/// The tests' cache of installed packages (`YOBUILD_HOME` for every
/// `yobuild` they start): each set installed from npm once, kept across
/// runs of the suite.
const CACHE: &str = concat!(env!("CARGO_TARGET_TMPDIR"), "/yobuild-home");

fn yobuild(args: &[&str], cwd: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_yobuild"))
        .env("YOBUILD_HOME", CACHE)
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
        .env("YOBUILD_HOME", CACHE)
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
        "[builtins]\ngate = \"off\"\nfacts = true\n",
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
        "[limits]\nmax_turns = 1\n[builtins]\ngate = \"off\"\n",
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
            "[[plugin]]\npi = \"{}\"\n[builtins]\ngate = \"off\"\n",
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
fn a_pinned_pi_package_runs_from_the_cache() {
    if !pi_installed() {
        return;
    }
    let dir = project(
        "",
        r#"[{ "tools": [{ "name": "pi_echo", "arguments": { "text": "hi" } }] }, { "text": "done" }]"#,
    );
    // Not on npm: installed by hand into the cache, with its lock.
    let package = dir.path().join("fake-pi");
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
            format!("[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n[[plugin]]\npi = \"{spec}\"\n[builtins]\ngate = \"off\"\n"),
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
    yobuild::install::seed(
        Path::new(CACHE),
        &dir.path().join("agent.toml"),
        yobuild::install::Set::Pi,
        &[("fake-pi", "1.0.0", &package)],
    )
    .unwrap();
    let out = go();
    let stderr = text(&out.stderr);
    assert!(
        out.status.success() && stderr.contains("  pi_echo ✓"),
        "{stderr}"
    );
    // Another version: the lock is stale, which `--locked` refuses.
    manifest("fake-pi@1.0.1");
    let out = yobuild(
        &[
            "run", "agent.toml", "--scripted", "script.json", "--locked", "--once", "go",
        ],
        dir.path(),
    );
    let stderr = text(&out.stderr);
    assert!(
        stderr.starts_with("yobuild: /")
            && stderr.ends_with("/agent.lock: its `pi` set is not current: the manifest's pins or the DSH base changed (--locked: run without it to resolve the set again)\n"),
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
            let args = ["run", "agent.toml", "--scripted", "script.json"];
            Self::start(dir, &args, &["--web", "127.0.0.1:0", task], late).await
        }

        /// `yobuild <args> <web>` in `dir`, connected `late` after it serves.
        pub async fn start(
            dir: &std::path::Path,
            args: &[&str],
            web: &[&str],
            late: Duration,
        ) -> Page {
            let mut child = Command::new(env!("CARGO_BIN_EXE_yobuild"))
                .env("YOBUILD_HOME", super::CACHE)
                .args(args)
                .args(web)
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

        /// The next message, whatever it is.
        pub async fn next(&mut self) -> Json {
            let wait = async {
                while let Some(Ok(message)) = self.socket.next().await {
                    if let Message::Text(text) = message {
                        return serde_json::from_str(&text).unwrap();
                    }
                }
                panic!("the page closed")
            };
            tokio::time::timeout(Duration::from_secs(60), wait)
                .await
                .expect("a message in 60 s")
        }

        /// Sends `message` as the page would.
        pub async fn send(&mut self, message: Json) {
            use futures::SinkExt;
            self.socket
                .send(Message::Text(message.to_string().into()))
                .await
                .unwrap();
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
    let dir = project("[builtins]\ngate = \"off\"\n", r#"[{ "text": "done" }]"#);
    let mut page = web::Page::open(dir.path(), "go").await;
    assert_eq!(page.until("runEnd").await["outcome"], "completed");
    let (code, stderr) = page.interrupt();
    assert_eq!(code, Some(0), "{stderr}");

    // Stopped by a limit: 1, and said.
    let echo =
        r#"{ "tools": [{ "name": "write_file", "arguments": { "path": "a", "content": "a" } }] }"#;
    let dir = project(
        "[limits]\nmax_turns = 1\n[builtins]\ngate = \"off\"\n",
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
        "[builtins]\ngate = \"off\"\n",
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
    // With the built-in off, a plugin's `ask_owner` is any other tool: the gate
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
        "[builtins]\ngate = \"off\"\n",
        r#"[{ "tools": [{ "name": "bash", "arguments": { "command": "sleep 30" } }] }, { "text": "done" }]"#,
    );
    let child = Command::new(env!("CARGO_BIN_EXE_yobuild"))
        .env("YOBUILD_HOME", CACHE)
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
        "[builtins]\nask_owner = true\nfacts = true\n",
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

#[test]
fn a_plugin_allows_only_its_own_tools() {
    if !pi_installed() {
        return;
    }
    // Two pi extensions, each with one tool. With the gate on, a tool runs
    // without asking only when the plugin that offers it allows it.
    let fixture = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("adapters/pi/fixture-extension.ts"),
    )
    .unwrap();
    let dir = project(
        "",
        r#"[{ "tools": [{ "name": "beta_echo", "arguments": { "text": "hi" } }] }, { "text": "done" }]"#,
    );
    let ext = dir.path().join("node_modules/exts");
    std::fs::create_dir_all(&ext).unwrap();
    std::fs::write(
        ext.join("package.json"),
        r#"{ "name": "exts", "version": "1.0.0", "type": "module" }"#,
    )
    .unwrap();
    for name in ["alpha", "beta"] {
        std::fs::write(
            ext.join(format!("{name}.ts")),
            fixture.replace("'pi_echo'", &format!("'{name}_echo'")),
        )
        .unwrap();
    }
    let run = |allowing: &str| {
        std::fs::write(
            dir.path().join("agent.toml"),
            format!(
                "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n\
                 [[plugin]]\nname = \"alpha\"\npi = \"./node_modules/exts/alpha.ts\"\n{}\
                 [[plugin]]\nname = \"beta\"\npi = \"./node_modules/exts/beta.ts\"\n{}",
                if allowing == "alpha" {
                    "allow = [\"beta_echo\"]\n"
                } else {
                    ""
                },
                if allowing == "beta" {
                    "allow = [\"beta_echo\"]\n"
                } else {
                    ""
                },
            ),
        )
        .unwrap();
        let out = yobuild(
            &[
                "run", "agent.toml", "--scripted", "script.json", "--once", "go",
            ],
            dir.path(),
        );
        text(&out.stderr)
    };
    // Allowed by the plugin that offers it: it runs.
    let stderr = run("beta");
    assert!(stderr.contains("  beta_echo ✓"), "{stderr}");
    // Allowed by another plugin: it asks (and with nobody there, does not run).
    let stderr = run("alpha");
    assert!(
        stderr.contains("  beta_echo ✗ Tool call denied: not allowed"),
        "{stderr}"
    );
}

#[test]
fn a_stale_lock_is_refused_with_locked_and_resolved_again_without() {
    if !pi_installed() {
        return;
    }
    let dir = project(
        "[[plugin]]\npi = \"@juicesharp/rpiv-todo@2.12.0\"\n[builtins]\ngate = \"off\"\n",
        r#"[{ "text": "done" }]"#,
    );
    // A lock made for another version (its packages left out: never
    // installed).
    let lock = dir.path().join("agent.lock");
    std::fs::write(
        &lock,
        r#"{ "lockVersion": 1, "sets": { "pi": { "requested": { "@juicesharp/rpiv-todo": "2.11.0" }, "packages": { "": {} } } } }"#,
    )
    .unwrap();
    let go = |locked: bool| {
        let mut args = vec!["run", "agent.toml", "--scripted", "script.json"];
        if locked {
            args.push("--locked");
        }
        args.extend(["--once", "go"]);
        yobuild(&args, dir.path())
    };
    let out = go(true);
    let stderr = text(&out.stderr);
    assert!(
        !out.status.success() && stderr.contains("agent.lock: its `pi` set is not current"),
        "{stderr}"
    );
    let out = go(false);
    let stderr = text(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(
        stderr.contains("  Info: agent.lock: `pi` resolved again: +")
            && stderr.contains("+@juicesharp/rpiv-todo 2.12.0"),
        "{stderr}"
    );
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&lock).unwrap()).unwrap();
    assert_eq!(
        written["sets"]["pi"]["requested"],
        serde_json::json!({ "@juicesharp/rpiv-todo": "2.12.0" })
    );
    assert!(
        written["sets"]["pi"]["packages"]["node_modules/@juicesharp/rpiv-todo"]["integrity"]
            .as_str()
            .is_some_and(|i| i.starts_with("sha512-")),
        "{written}"
    );
    // The optional peer that is not pi's own is in the set; pi's own
    // packages (the adapter's) are not.
    let packages = written["sets"]["pi"]["packages"].as_object().unwrap();
    assert!(
        packages.contains_key("node_modules/@juicesharp/rpiv-i18n"),
        "{written}"
    );
    assert!(
        !packages
            .keys()
            .any(|k| k.starts_with("node_modules/@earendil-works/pi-")
                || k == "node_modules/typebox"),
        "{written}"
    );
    // Now current: --locked runs.
    let out = go(true);
    assert!(out.status.success(), "{}", text(&out.stderr));
}

// -- the example agents (examples/*.toml), as their demos ran ---------------

/// The examples folder, when the adapters they load are installed
/// (`scripts/try-coding-agent.sh --setup-only`; required in CI). Their
/// pinned packages are installed into the tests' cache from their checked-in
/// locks (`--locked`: a lock out of date fails the test), from npm the first
/// time.
fn examples() -> Option<std::path::PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let dsh = Path::new(env!("CARGO_MANIFEST_DIR")).join("adapters/dsh/node_modules");
    if pi_installed() && dsh.is_dir() {
        return Some(dir);
    }
    if std::env::var_os("YOAGENT_RUTIS_REQUIRE_RUNTIMES").is_some_and(|v| v == "1") {
        panic!(
            "the examples need the adapters: scripts/copy-adapters.sh, then npm ci in adapters/dsh"
        );
    }
    eprintln!("SKIPPED: scripts/copy-adapters.sh, then npm ci in adapters/dsh");
    None
}

#[test]
fn the_minimal_example_reads_and_completes() {
    let dir = tempfile::tempdir().unwrap();
    let ex = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let out = yobuild(
        &[
            "run",
            &ex.join("minimal.toml").display().to_string(),
            "--scripted",
            &ex.join("scripts/minimal.json").display().to_string(),
            "--once",
            "look",
        ],
        dir.path(),
    );
    let stderr = text(&out.stderr);
    assert!(
        out.status.success() && stderr.contains("  list_files ✓"),
        "{stderr}"
    );
}

#[test]
fn the_minimal_example_reads_only_inside_its_folder() {
    use serde_json::{json, Value as Json};
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("in.txt"), "inside").unwrap();
    std::fs::write(dir.path().join("out.txt"), "outside").unwrap();
    let ex = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    // Each call in its own turn, in this order.
    let calls = [
        ("read_file", json!({ "path": "in.txt" })),
        ("read_file", json!({ "path": "../out.txt" })),
        ("read_file", json!({ "path": dir.path().join("out.txt") })),
        ("list_files", json!({ "path": dir.path() })),
        (
            "search",
            json!({ "pattern": "outside", "path": dir.path() }),
        ),
        ("bash", json!({ "command": "cat ../out.txt" })),
    ];
    let mut script: Vec<Json> = calls
        .iter()
        .map(|(name, arguments)| json!({ "tools": [{ "name": name, "arguments": arguments }] }))
        .collect();
    script.push(json!({ "text": "done" }));
    let script_path = dir.path().join("script.json");
    std::fs::write(&script_path, Json::Array(script).to_string()).unwrap();
    let out = yobuild(
        &[
            "run",
            &ex.join("minimal.toml").display().to_string(),
            "--scripted",
            &script_path.display().to_string(),
            "--once",
            "look",
        ],
        &project,
    );
    let stderr = text(&out.stderr);
    let marks: Vec<&str> = stderr
        .lines()
        .filter(|l| l.starts_with("  ") && (l.contains(" ✓") || l.contains(" ✗")))
        .collect();
    assert_eq!(marks.len(), 6, "{stderr}");
    assert!(marks[0].starts_with("  read_file ✓"), "{marks:?}");
    for refused in &marks[1..] {
        assert!(refused.contains(" ✗"), "{marks:?}");
    }
}

#[test]
fn the_coding_examples_policy_blocks_a_deletion_with_nobody_there() {
    let Some(ex) = examples() else { return };
    let dir = tempfile::tempdir().unwrap();
    let out = yobuild(
        &[
            "run",
            &ex.join("coding.toml").display().to_string(),
            "--scripted",
            &ex.join("scripts/coding.json").display().to_string(),
            "--locked",
            "--once",
            "clean the build",
        ],
        dir.path(),
    );
    let stderr = text(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.contains("  list_files ✓"), "{stderr}");
    assert!(
        stderr.contains("  bash ✗ Tool call denied: the user blocked it"),
        "{stderr}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_handoff_example_asks_without_waiting_and_the_answer_starts_the_next_run() {
    let Some(ex) = examples() else { return };
    let dir = project("", "[]");
    let args = [
        "run".to_owned(),
        ex.join("handoff.toml").display().to_string(),
        "--scripted".to_owned(),
        ex.join("scripts/handoff.json").display().to_string(),
        "--measure".to_owned(),
        "--locked".to_owned(),
    ];
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let task = "Look at this folder and write notes.md about it.";
    let mut page = web::Page::start(
        dir.path(),
        &args,
        &["--web", "127.0.0.1:0", task],
        std::time::Duration::ZERO,
    )
    .await;
    let mut question = None;
    let mut facts = Vec::new();
    let mut ended = 0;
    let mut second_prompt = None;
    let mut ran = Vec::new();
    let mut ui_plugins = serde_json::Value::Null;
    while ended < 2 {
        let m = page.next().await;
        match m["type"].as_str() {
            Some("hello") => ui_plugins = m["uiPlugins"].clone(),
            Some("uiPlugins") => ui_plugins = m["uiPlugins"].clone(),
            Some("uiRequest") => {
                // The agent's question, the only one: a gate question here
                // would mean an allowed tool asked.
                assert!(question.is_none(), "a second question: {m}");
                let shown = m["request"].to_string();
                assert!(shown.contains("bullet list"), "{m}");
                question = Some(m["id"].clone());
            }
            Some("event") if m["event"]["type"] == "toolExecutionEnd" => {
                let tool = m["event"]["toolName"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                assert_eq!(m["event"]["isError"], false, "{tool} failed: {m}");
                ran.push(tool);
            }
            Some("notice") => facts.push(m["message"].as_str().unwrap_or_default().to_owned()),
            Some("runStart") if ended == 1 => {
                second_prompt = m["prompt"].as_str().map(str::to_owned)
            }
            Some("runEnd") => {
                assert_eq!(m["outcome"], "completed", "{m}");
                ended += 1;
                if ended == 1 {
                    // The owner comes back, and answers.
                    let id = question.clone().expect("the agent asked the owner");
                    page.send(serde_json::json!({ "type": "uiResponse", "id": id, "value": "Bullet list" }))
                        .await;
                }
            }
            _ => {}
        }
    }
    // Every scripted call ran: the plan (a pi tool the manifest allows),
    // the question, reading, writing.
    for tool in ["todo", "ask_owner", "list_files", "write_file"] {
        assert!(ran.iter().any(|t| t == tool), "{tool} did not run: {ran:?}");
    }
    // The todo-plan UI plugin (a rutis plugin of frontend/plugins) is up.
    assert!(ui_plugins.to_string().contains("todo-plan"), "{ui_plugins}");
    let second = second_prompt.unwrap_or_default();
    assert!(second.starts_with("[Owner's answer to #1]"), "{second}");
    assert!(dir.path().join("notes.md").exists(), "run 2 wrote notes.md");
    // Each run's plugin time and runtime memory (`--measure`).
    let measured = facts
        .iter()
        .filter(|f| f.starts_with("Plugin time this run: "))
        .count();
    assert_eq!(measured, 2, "{facts:?}");
    assert!(
        facts.iter().any(|f| f.contains("Runtime memory: pi ")),
        "{facts:?}"
    );
    let facts: Vec<&String> = facts.iter().filter(|f| f.starts_with("Facts:")).collect();
    assert_eq!(facts.len(), 2, "{facts:?}");
    assert!(facts[1].contains("new: notes.md"), "{facts:?}");
    let (code, stderr) = page.interrupt();
    assert_eq!(code, Some(0), "{stderr}");
}

#[test]
fn an_allowed_pi_tool_calls_only_its_own_extensions_tools() {
    // `relay` (allowed) calls `secret` (another extension's, not allowed)
    // through pi's `ctx.executeTool`: refused (`nestedCalls:
    // "own-extension"`), so allowing `relay` does not allow `secret`.
    if !pi_installed() {
        return;
    }
    let dir = project(
        "[[plugin]]\npi = \"./relay.ts\"\nallow = [\"relay\"]\n[[plugin]]\npi = \"./secret.ts\"\n",
        r#"[{ "tools": [{ "name": "relay", "arguments": {} }] }, { "text": "done" }]"#,
    );
    std::fs::write(
        dir.path().join("relay.ts"),
        "import { writeFileSync } from 'node:fs'\n\
         export default function (pi: any) {\n\
           pi.registerTool({\n\
             name: 'relay', label: 'Relay', description: 'Calls secret.',\n\
             parameters: { type: 'object', properties: {} },\n\
             async execute(_id: string, _p: unknown, _s: unknown, _u: unknown, ctx: any) {\n\
               const o = await ctx.executeTool('secret', {})\n\
               const text = o.result.content.map((b: any) => b.text ?? '').join('')\n\
               writeFileSync(`${ctx.cwd}/relay.txt`, `${o.isError ? 'error' : 'ok'}: ${text}`)\n\
               return { content: [{ type: 'text', text }], details: undefined }\n\
             },\n\
           })\n\
         }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("secret.ts"),
        "import { writeFileSync } from 'node:fs'\n\
         export default function (pi: any) {\n\
           pi.registerTool({\n\
             name: 'secret', label: 'Secret', description: 'Not allowed.',\n\
             parameters: { type: 'object', properties: {} },\n\
             async execute(_id: string, _p: unknown, _s: unknown, _u: unknown, ctx: any) {\n\
               writeFileSync(`${ctx.cwd}/secret-ran.txt`, 'ran')\n\
               return { content: [{ type: 'text', text: 'secret ran' }], details: undefined }\n\
             },\n\
           })\n\
         }\n",
    )
    .unwrap();
    let out = yobuild(
        &[
            "run", "agent.toml", "--scripted", "script.json", "--once", "go",
        ],
        dir.path(),
    );
    let stderr = text(&out.stderr);
    assert!(stderr.contains("  relay ✓"), "{stderr}");
    let relayed = std::fs::read_to_string(dir.path().join("relay.txt")).unwrap_or_default();
    assert!(
        relayed.starts_with("error: secret belongs to another extension"),
        "{relayed} {stderr}"
    );
    assert!(!dir.path().join("secret-ran.txt").exists(), "{stderr}");
}
