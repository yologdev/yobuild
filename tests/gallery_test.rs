//! The gallery (`gallery/<name>/`, design §11): each agent `check`ed
//! `--locked`, and each of its reply files (`scripts/<x>.json`, with its task
//! in `scripts/<x>.task`) run scripted under `--once --locked`, in a temp
//! folder, with a temp `HOME` (so a plugin's cache or config there is the
//! test's own). What holds an agent:
//! - `check`: `ok`, no note (but `[extend]`'s, once, with it), each name
//!   an entry's `allow` lists `allowed`, and no tool that writes with
//!   `tools = "read-only"`;
//! - each reply file (its folder holding the files the agent's test puts
//!   there, unchanged afterwards): `Completed` with its last `text` as the answer, each call it
//!   scripts made once, no `Error:` notice, every call to a tool that asks
//!   denied (nobody answers), every other call `✓` —
//!   but a plugin's or server's own `✗` (a third party's day: a search
//!   engine down), never one of the bridge's (its plugin unloaded or
//!   stopped answering), and never a denial;
//!   A reply file meant to show refusals: every call `✗`;
//! - every tool `check` says asks, called with nobody there: denied;
//! - a reply file that loads a server or package (`[extend]`, design §14), refused
//!   under `--once`, runs on the page instead (killed after 5 min in all):
//!   its questions answered over the WebSocket as the test says, every
//!   answer used and no other question; `completed` with its last `text` as
//!   the answer, Ctrl+C then exiting 0; no `Error` notice, even just after
//!   the run; each call it scripts made once, every one `✓` — but, when the
//!   owner did not allow both questions, `load_plugin` refused for the
//!   reason the test names (and said in a Warning), and every
//!   `<name>__<tool>` call `✗`; the `[[mcp]]` (or `[[plugin]]`) block on
//!   stderr exactly when the server or package was kept, as the test says
//!   it, never marked stopped; the second question offering the tools the
//!   test names; the files the test names left in the run's `HOME`.
//!
//! A scripted run cannot tell whether the task fits the replies (the model
//! is the script): the task is the one a reader would type. Unix (as
//! `cli_test`).
#![cfg(unix)]

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value as Json;
use yobuild::manifest::{Manifest, PluginKind, Tools};

/// The tests' cache of installed packages, shared with `cli_test`.
const CACHE: &str = concat!(env!("CARGO_TARGET_TMPDIR"), "/yobuild-home");
/// One `yobuild` call at most: a third party that stalls fails the test
/// instead of hanging it.
const DEADLINE: Duration = Duration::from_secs(300);
/// What the bridge says when a plugin, not the tool's work, failed a call
/// (`hints.rs`' `Call` rows: the run goes on, so only the call shows it).
const BRIDGE: [&str; 4] = [
    "is no longer available",
    "its plugin unloaded",
    "did not answer",
    "a required plugin handler failed",
];
const DENIED: &str = "Tool call denied: ";

/// A reply file's `expect` missed (`run/expect.rs`): the result was not
/// what the reply file was written against — never a plugin's own `✗`.
const MISSED: &str = "the reply file expected `";
const NOBODY: &str = "Tool call denied: not allowed: the owner declined, or nobody answered";

struct Ran {
    ok: bool,
    stdout: String,
    stderr: String,
}

fn yobuild(args: &[&str], cwd: &Path, home: &Path) -> Ran {
    let mut command = Command::new(env!("CARGO_BIN_EXE_yobuild"));
    // None of the credentials yoagent reads a developer may hold: `check` would note them.
    for key in yobuild::manifest::KEY_VARIABLES {
        command.env_remove(key);
    }
    let mut child = command
        .env("YOBUILD_HOME", CACHE)
        .env("HOME", home)
        .env_remove("RUST_LOG")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let read = |mut pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut text = String::new();
            pipe.read_to_string(&mut text).unwrap();
            text
        })
    };
    let stdout = read(Box::new(child.stdout.take().unwrap()));
    let stderr = read(Box::new(child.stderr.take().unwrap()));
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if start.elapsed() > DEADLINE {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let ran = Ran {
        ok: status.is_some_and(|s| s.success()),
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
    };
    assert!(
        status.is_some(),
        "yobuild {args:?} still running after {DEADLINE:?}: {}",
        ran.stderr
    );
    ran
}

/// The agent's gallery folder, when Node and npm are there (required in
/// CI): the binary installs its own runtime files (design §13), so the
/// checkout's `adapters/` is not needed.
fn agent(name: &str) -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let runs = |program: &str| {
        Command::new(program)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    if runs("node") && runs("npm") {
        return Some(root.join("gallery").join(name));
    }
    if std::env::var_os("YOAGENT_RUTIS_REQUIRE_RUNTIMES").is_some_and(|v| v == "1") {
        panic!("the gallery needs Node 22.19+ and npm 11.19+");
    }
    eprintln!("SKIPPED: the gallery needs Node 22.19+ and npm 11.19+");
    None
}

/// The tool calls a run printed under `--once`, in order: (name, mark).
fn calls(stderr: &str) -> Vec<(&str, &str)> {
    stderr
        .lines()
        .filter_map(|line| line.strip_prefix("  ")?.split_once(' '))
        .filter(|(name, mark)| {
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                && (*mark == "✓" || mark.starts_with("✗ "))
        })
        .collect()
}

/// A value `schema` (JSON Schema, as `check --json` gives a tool's
/// `parameters`) accepts: its required properties, each the first of its
/// `enum`, else a plain value of its type; a list holds one item. An
/// object with nothing required is `{}`.
fn accepted(schema: &Json) -> Json {
    if let Some(first) = schema["enum"].as_array().and_then(|e| e.first()) {
        return first.clone();
    }
    match schema["type"].as_str() {
        Some("string") => Json::from("x"),
        Some("number") | Some("integer") => Json::from(1),
        Some("boolean") => Json::from(false),
        Some("array") => Json::Array(vec![accepted(&schema["items"])]),
        _ => {
            let required = schema["required"].as_array().cloned().unwrap_or_default();
            let object = required
                .iter()
                .filter_map(|name| name.as_str())
                .map(|name| (name.to_owned(), accepted(&schema["properties"][name])))
                .collect();
            Json::Object(object)
        }
    }
}

/// The `tools` replies' calls of a reply file, in order, and its last `text`.
fn script(replies: &Json) -> (Vec<String>, String) {
    let replies = replies.as_array().expect("a reply file is a list");
    let called = replies
        .iter()
        .filter_map(|r| r["tools"].as_array())
        .flatten()
        .map(|c| c["name"].as_str().unwrap().to_owned())
        .collect();
    let last = replies.last().unwrap()["text"]
        .as_str()
        .expect("a reply file ends with a text")
        .to_owned();
    (called, last)
}

/// What a run starts with and is held to: the files put in its folder
/// first (path, contents), each the same afterwards; the reply files run on
/// the page instead of under `--once` ([`Page`]); the reply files meant to
/// show refusals (every call `✗`, whatever the reason); the tools that ask
/// (each call denied: nobody answers); the plugins' and servers' tools,
/// whose own `✗` is tolerated; a `HOME` shared by every run, when the
/// agent's plugin keeps a large cache there (else a temporary one each).
struct Rules<'a> {
    files: &'a [(&'a str, &'a str)],
    page: &'a [Page<'a>],
    home: Option<&'a Path>,
    refusing: &'a [&'a str],
    asks: HashSet<String>,
    theirs: HashSet<String>,
}

/// A reply file (`file`, its name) run on the page: each question's answer
/// in order (the title's start, `Allow` or `Block`), and, when the owner
/// does not allow both of `load_plugin`'s questions, what its refusal must
/// say (the reason, as `load_plugin` words it). When kept: what stderr
/// says of it at the end (`block`, each text in it) and the tools the second
/// question names (`offers`). After the run: files under its `HOME` and a
/// text each holds (`home`).
struct Page<'a> {
    file: &'a str,
    answers: &'a [(&'a str, &'a str)],
    refused: Option<&'a str>,
    block: &'a [&'a str],
    offers: &'a [&'a str],
    home: &'a [(&'a str, &'a str)],
}

/// What a page showed: the questions' titles, the notices (`level: text`),
/// the tool calls (name, `✓` or `✗ text`), the last answer, and how the
/// run ended (outcome, error).
#[derive(Debug, Default)]
struct Shown {
    asked: Vec<String>,
    notices: Vec<String>,
    made: Vec<(String, String)>,
    answer: String,
    ended: Option<(String, String)>,
}

/// How long the page is read after the run ended: notices the session's
/// tick sends (a loaded server stopped) can come just after it.
const AFTER: Duration = Duration::from_millis(1500);

/// Reads the page at `ws` until the run ended and [`AFTER`] passed,
/// answering each question with the next of `answers`. A question not
/// expected, the page closing or failing first: the reason.
async fn read_page(ws: &str, answers: &[(&str, &str)], shown: &mut Shown) -> Result<(), String> {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let opened = tokio::time::timeout(
        Duration::from_secs(30),
        tokio_tungstenite::connect_async(ws),
    );
    let (mut socket, _) = opened
        .await
        .map_err(|_| "the page did not open within 30 s".to_owned())?
        .map_err(|e| format!("the page did not open: {e}"))?;
    let mut left = answers.iter();
    loop {
        let ended = shown.ended.is_some();
        let wait = if ended { AFTER } else { DEADLINE };
        let message = match tokio::time::timeout(wait, socket.next()).await {
            Err(_) | Ok(None) if ended => return Ok(()),
            Err(_) => return Err(format!("nothing came from the page for {wait:?}")),
            Ok(None) => return Err("the page closed before the run ended".to_owned()),
            Ok(Some(Err(e))) => return Err(format!("the page's socket failed: {e}")),
            Ok(Some(Ok(message))) => message,
        };
        let text = match message {
            Message::Text(text) => text,
            Message::Close(_) if ended => return Ok(()),
            Message::Close(frame) => return Err(format!("the page closed: {frame:?}")),
            _ => continue,
        };
        let m: Json =
            serde_json::from_str(&text).map_err(|e| format!("not the protocol ({e}): {text}"))?;
        let said = |v: &Json| v.as_str().unwrap_or_default().to_owned();
        match m["type"].as_str() {
            Some("uiRequest") => {
                let title = said(&m["request"]["title"]);
                let Some((start, value)) = left.next() else {
                    return Err(format!("a question not expected: {title}"));
                };
                if !title.starts_with(start) {
                    return Err(format!("{title:?} is not {start:?}"));
                }
                shown.asked.push(title);
                let answer =
                    serde_json::json!({ "type": "uiResponse", "id": m["id"], "value": value });
                socket
                    .send(Message::Text(answer.to_string().into()))
                    .await
                    .map_err(|e| format!("the answer could not be sent: {e}"))?;
            }
            Some("notice") => {
                shown
                    .notices
                    .push(format!("{}: {}", said(&m["level"]), said(&m["message"])))
            }
            Some("event") => {
                let event = &m["event"];
                let texts = |content: &Json| {
                    content
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|c| c["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                match event["type"].as_str() {
                    Some("toolExecutionEnd") => {
                        let mark = match event["isError"] == true {
                            true => format!("✗ {}", texts(&event["result"]["content"])),
                            false => "✓".to_owned(),
                        };
                        shown.made.push((said(&event["toolName"]), mark));
                    }
                    Some("messageEnd") if event["message"]["role"] == "assistant" => {
                        let text = texts(&event["message"]["content"]);
                        if !text.is_empty() {
                            shown.answer = text;
                        }
                    }
                    _ => {}
                }
            }
            Some("runEnd") => shown.ended = Some((said(&m["outcome"]), said(&m["error"]))),
            _ => {}
        }
    }
}

/// One reply file's run on the page (`--web`), its questions answered as
/// `page` says, then Ctrl+C: held to the rules above, the run killed after
/// [`DEADLINE`] in all.
fn on_page(manifest: &str, file: &Path, task: &str, page: &Page, rules: &Rules) {
    use std::io::{BufRead, BufReader};
    use std::sync::mpsc;

    let name = file.file_name().unwrap().to_string_lossy();
    let replies: Json = serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
    let (called, last) = script(&replies);
    let (work, own) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let home = rules.home.unwrap_or(own.path());
    for (path, contents) in rules.files {
        let path = work.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    let mut command = Command::new(env!("CARGO_BIN_EXE_yobuild"));
    for key in yobuild::manifest::KEY_VARIABLES {
        command.env_remove(key);
    }
    let mut child = command
        .env("YOBUILD_HOME", CACHE)
        .env("HOME", home)
        .env_remove("RUST_LOG")
        .args(["run", manifest, "--scripted"])
        .arg(file)
        .args(["--web", "127.0.0.1:0", "--locked", task])
        .current_dir(work.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    /// Killed however the test ends.
    struct Kill(std::process::Child);
    impl Drop for Kill {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let pid = child.id().to_string();
    // Its stderr, sent once it closes.
    let (err_tx, err_rx) = mpsc::channel();
    let mut err = child.stderr.take().unwrap();
    std::thread::spawn(move || {
        let mut text = String::new();
        if let Err(e) = err.read_to_string(&mut text) {
            text.push_str(&format!("\n(stderr could not be read on: {e})"));
        }
        let _ = err_tx.send(text);
    });
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut child = Kill(child);
    // The whole run's limit: killed then, whatever it waits for (its URL,
    // the page, its end after Ctrl+C). Dropping `done` stands it down.
    let (done, watched) = mpsc::channel::<()>();
    let killer = pid.clone();
    let watchdog = std::thread::spawn(move || {
        let fired = watched.recv_timeout(DEADLINE) == Err(mpsc::RecvTimeoutError::Timeout);
        if fired {
            let _ = Command::new("kill").args(["-KILL", &killer]).status();
        }
        fired
    });

    let mut line = String::new();
    let read = out.read_line(&mut line);
    std::thread::spawn(move || std::io::copy(&mut out, &mut std::io::sink()));
    let mut shown = Shown::default();
    let result = match (read, line.split_whitespace().nth(1)) {
        (Err(e), _) => Err(format!("its URL could not be read: {e}")),
        (Ok(_), None) => Err(format!("no URL: {line:?}")),
        (Ok(_), Some(url)) => {
            let ws = url.replace("http://", "ws://").replace("/?t=", "/ws?t=");
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(read_page(&ws, page.answers, &mut shown))
        }
    };
    // The block a kept server would be kept by comes once the runtimes stop.
    if matches!(child.0.try_wait(), Ok(None)) {
        let sent = Command::new("kill").args(["-INT", &pid]).status();
        assert!(sent.is_ok_and(|s| s.success()), "{name}: Ctrl+C not sent");
    }
    let status = child.0.wait().unwrap();
    drop(done);
    let killed = watchdog.join().unwrap();
    // A process it left holding stderr open is not waited for.
    let stderr = err_rx
        .recv_timeout(Duration::from_secs(10))
        .unwrap_or_else(|_| "(stderr still open 10 s after it exited)".to_owned());
    let all = format!("{name} on the page:\n{shown:#?}\n{stderr}");
    assert!(!killed, "killed after {DEADLINE:?}: {all}");
    if let Err(why) = result {
        panic!("{name}: {why}\n{all}");
    }
    let (outcome, error) = shown.ended.clone().unwrap_or_default();
    assert_eq!(outcome, "completed", "{error}: {all}");
    assert_eq!(shown.answer, last, "{all}");
    assert_eq!(status.code(), Some(0), "{all}");
    assert_eq!(
        shown.asked.len(),
        page.answers.len(),
        "answers left unused: {all}"
    );
    assert!(
        !shown.notices.iter().any(|n| n.starts_with("error: ")),
        "{all}"
    );
    assert!(
        !shown
            .notices
            .iter()
            .any(|n| n.contains("expectation for `")),
        "{all}"
    );
    let mut names: Vec<&str> = shown.made.iter().map(|(n, _)| n.as_str()).collect();
    let mut called: Vec<&str> = called.iter().map(String::as_str).collect();
    names.sort_unstable();
    called.sort_unstable();
    assert_eq!(names, called, "{all}");
    for (path, contents) in rules.files {
        let now = std::fs::read_to_string(work.path().join(path)).unwrap_or_default();
        assert_eq!(&now, contents, "{path} changed: {all}");
    }
    // Loaded only when allowed to start and then kept (the questions came
    // as `answers` says: checked above, in order).
    let kept = ["Start the ", "Keep the "].iter().all(|q| {
        page.answers
            .iter()
            .any(|(t, v)| t.starts_with(q) && *v == "Allow")
    });
    let refused = page.refused.filter(|_| !kept);
    assert_eq!(
        refused.is_some(),
        !kept,
        "{name}: a refusal to expect, only when not kept"
    );
    for (tool, mark) in &shown.made {
        match refused {
            Some(why) if tool == "load_plugin" => {
                assert!(
                    mark.starts_with("✗ load_plugin refused it: ") && mark.contains(why),
                    "{tool}: not `{why}`: {all}"
                );
                assert!(
                    shown
                        .notices
                        .iter()
                        .any(|n| n.starts_with("warning: The agent's plugin `") && n.contains(why)),
                    "no Warning naming the refusal: {all}"
                );
            }
            // Not loaded: its tools are denied (they ask), or not found.
            Some(_) if tool.contains("__") => {
                assert!(mark.starts_with("✗ Tool "), "{tool} ran unloaded: {all}")
            }
            _ => assert_eq!(mark, "✓", "{tool}: {all}"),
        }
    }
    assert_eq!(
        stderr.contains("To keep it, add to "),
        kept,
        "the block said: {all}"
    );
    assert!(
        !stderr.contains("stopped this session"),
        "a kept server stopped: {all}"
    );
    if kept {
        for said in page.block {
            assert!(stderr.contains(said), "{said:?} not in the block: {all}");
        }
        let second = shown.asked.iter().find(|t| t.starts_with("Keep the "));
        for tool in page.offers {
            assert!(
                second.is_some_and(|t| t
                    .split(" It offers: ")
                    .nth(1)
                    .is_some_and(|l| l.split(", ").any(|t| t == *tool))),
                "the second question does not offer `{tool}`: {all}"
            );
        }
    }
    for (path, text) in page.home {
        let held = std::fs::read_to_string(home.join(path)).unwrap_or_default();
        assert!(
            held.contains(text),
            "{path} in its HOME holds no {text:?}: {held:?}\n{all}"
        );
    }
}

/// One reply file's run, held to the rules above.
fn scripted(manifest: &str, file: &Path, task: &str, rules: &Rules) {
    let name = file.file_name().unwrap().to_string_lossy();
    let replies: Json = serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
    let (called, last) = script(&replies);
    let (work, own) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let home = rules.home.unwrap_or(own.path());
    for (path, contents) in rules.files {
        let path = work.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    let ran = yobuild(
        &[
            "run",
            manifest,
            "--scripted",
            &file.display().to_string(),
            "--once",
            "--locked",
            task,
        ],
        work.path(),
        home,
    );
    let all = format!("{name}:\n{}{}", ran.stdout, ran.stderr);
    assert!(ran.ok, "{all}");
    assert_eq!(ran.stdout, format!("{last}\nCompleted\n"), "{all}");
    assert!(!ran.stderr.contains("\n  Error: "), "{all}");
    // Every `expect` was checked: its call ran.
    assert!(!ran.stderr.contains("expectation for `"), "{all}");
    // Each scripted call made, once (a reply's several calls may end in any
    // order).
    let made = calls(&ran.stderr);
    let mut names: Vec<&str> = made.iter().map(|(n, _)| *n).collect();
    let mut called: Vec<&str> = called.iter().map(String::as_str).collect();
    names.sort_unstable();
    called.sort_unstable();
    assert_eq!(names, called, "{all}");
    for (path, contents) in rules.files {
        let now = std::fs::read_to_string(work.path().join(path)).unwrap_or_default();
        assert_eq!(&now, contents, "{path} changed: {all}");
    }
    let refusing = rules.refusing.contains(&name.as_ref());
    for (tool, mark) in made {
        if refusing {
            assert!(mark.starts_with("✗ "), "{tool} ran: {all}");
        } else if rules.asks.contains(tool) {
            assert_eq!(mark, format!("✗ {NOBODY}"), "{tool}: {all}");
        } else if let Some(why) = mark.strip_prefix("✗ ") {
            assert!(
                rules.theirs.contains(tool)
                    && !why.starts_with(DENIED)
                    && !why.starts_with(MISSED)
                    && !BRIDGE.iter().any(|b| why.contains(b)),
                "{tool} ✗ {why}: {all}"
            );
        }
    }
}

/// `check` and every reply file of a gallery agent, each run starting with
/// `files` in its folder; in the reply files `refusing` names, every call
/// refused.
fn holds(dir: &Path, files: &[(&str, &str)], refusing: &[&str], home: Option<&Path>) {
    holds_with(dir, files, refusing, &[], home)
}

/// As [`holds`], the reply files `page` names run on the page.
fn holds_with(
    dir: &Path,
    files: &[(&str, &str)],
    refusing: &[&str],
    page: &[Page],
    home: Option<&Path>,
) {
    let path = dir.join("agent.toml");
    let manifest = path.display().to_string();
    let parsed = Manifest::load(&path).unwrap();
    let (work, own) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let ran = yobuild(
        &["check", &manifest, "--locked", "--json"],
        work.path(),
        home.unwrap_or(own.path()),
    );
    let report: Json = serde_json::from_str(&ran.stdout)
        .unwrap_or_else(|e| panic!("check's JSON ({e}): {}", ran.stderr));
    assert_eq!(report["ok"], true, "{report:#}");
    let entries = report["entries"].as_array().unwrap();
    // No note, but the one a DSH plugin with grants always gets (alone it
    // is probed without the DSH host; the scripted runs exercise it).
    const GRANTED_DSH: &str = "probed without the DSH host: its grants are not exercised alone";
    // And the kept environment's: this process's own variables are left
    // out of the plugins and servers (design §12 "Step 5").
    const KEPT: &str = "plugins and stdio MCP servers start with the kept environment (";
    // `[extend]`'s, on the agent (`check`'s own words: its sources and
    // kinds).
    let extend_note = parsed.extend.as_ref().map(|e| {
        let kinds: Vec<String> = e.kinds.iter().map(|k| format!("{}s", k.label())).collect();
        let sources = e.sources();
        let kinds = match kinds.split_last() {
            Some((last, [])) => last.clone(),
            Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
            None => String::new(),
        };
        format!("[extend]: the agent may search {sources} for {kinds} (")
    });
    let is_extend = |n: &str| {
        extend_note
            .as_ref()
            .is_some_and(|e| n.starts_with(e.as_str()))
    };
    // What depends on the environment the tests run in: a variable a
    // plugin's code reads set here (`DSH_HOME`, `NODE_PATH`), a proxy
    // holding a password.
    let here = |n: &str| {
        (n.starts_with("its code reads `") && n.contains("`, which is set here"))
            || n.contains(" hold(s) a password, ")
    };
    // `check`'s probe alone is said only with a pi or DSH plugin.
    let probed = parsed
        .plugins
        .iter()
        .any(|p| matches!(p.kind, PluginKind::Pi(_) | PluginKind::Dsh(_)));
    for notes in entries
        .iter()
        .chain([&report["agent"]])
        .map(|e| &e["notes"])
    {
        let notes = notes.as_array().map(Vec::as_slice).unwrap_or_default();
        for note in notes {
            let n = note.as_str().unwrap();
            // A pi package's skills, which yobuild does not load (pi-lens
            // ships four).
            let skills = n.starts_with("plugin ") && n.contains(" skills are not loaded (");
            // What `[extend]` lets the agent do, said whenever it is there
            // (counted below: once).
            let extend = is_extend(n);
            assert!(
                n == GRANTED_DSH || n.starts_with(KEPT) || here(n) || skills || extend,
                "{report:#}"
            );
            if n.starts_with(KEPT) {
                assert_eq!(n.contains("probed alone"), probed, "{n}");
            }
        }
    }
    if parsed.extend.is_some() {
        let said = report["agent"]["notes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|n| n.as_str().is_some_and(is_extend))
            .count();
        assert_eq!(said, 1, "[extend]'s agent note, once: {report:#}");
    }
    let kinds: HashMap<&str, &str> = report["agent"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| (t["name"].as_str().unwrap(), t["kind"].as_str().unwrap()))
        .collect();
    let allowed = parsed.plugins.iter().flat_map(|p| p.allow.clone()).chain(
        parsed
            .mcp
            .iter()
            .flat_map(|m| m.allow.iter().map(|t| format!("{}__{t}", m.name))),
    );
    for tool in allowed {
        assert_eq!(
            kinds.get(tool.as_str()),
            Some(&"allowed"),
            "{tool}: {kinds:?}"
        );
    }
    if parsed.builtins.tools == Tools::ReadOnly {
        for tool in ["bash", "write_file", "edit_file"] {
            assert!(
                !kinds.contains_key(tool),
                "read-only, yet {tool}: {kinds:?}"
            );
        }
    }
    let theirs: HashSet<String> = entries
        .iter()
        .flat_map(|e| e["tools"].as_array().unwrap())
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    let asks: HashSet<String> = kinds
        .iter()
        .filter(|(_, kind)| **kind == "asks")
        .map(|(name, _)| (*name).to_owned())
        .collect();

    let rules = Rules {
        files,
        page,
        home,
        refusing,
        asks,
        theirs,
    };

    // The reply files and their tasks, in pairs.
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir.join("scripts"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    let mut demo = None;
    let mut on_the_page = vec![];
    for file in &files {
        let pair = |x| file.with_extension(x).is_file();
        assert!(
            pair("json") && pair("task"),
            "{}: a reply file and its task, in pairs",
            file.display()
        );
        if file.extension().is_some_and(|x| x == "json") {
            let task = std::fs::read_to_string(file.with_extension("task")).unwrap();
            let file_name = file.file_name().unwrap().to_string_lossy();
            match rules.page.iter().find(|p| p.file == file_name) {
                Some(page) => {
                    on_page(&manifest, file, task.trim(), page, &rules);
                    on_the_page.push(page.file);
                }
                None => scripted(&manifest, file, task.trim(), &rules),
            }
            if file.file_stem().is_some_and(|s| s == "demo") {
                demo = Some(file.clone());
            }
        }
    }

    // A page entry naming no reply file would be passed over unsaid.
    for page in rules.page {
        assert!(
            on_the_page.contains(&page.file),
            "{}: no reply file {}",
            dir.display(),
            page.file
        );
    }

    // Each tool that asks, called with nobody there, ending as the demo does.
    let demo = demo.expect("scripts/demo.json");
    if !rules.asks.is_empty() {
        let (_, last) =
            script(&serde_json::from_str(&std::fs::read_to_string(&demo).unwrap()).unwrap());
        let calls: Vec<Json> = rules
            .asks
            .iter()
            .map(|t| {
                // Arguments its schema accepts, so the gate is what stops
                // it (a plugin may check them first: pi-lens does).
                let schema = report["agent"]["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|tool| tool["name"] == t.as_str())
                    .map(|tool| tool["parameters"].clone())
                    .unwrap_or(Json::Null);
                serde_json::json!({ "name": t, "arguments": accepted(&schema) })
            })
            .collect();
        let replies = serde_json::json!([{ "tools": calls }, { "text": last }]);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("asks.json");
        std::fs::write(&file, replies.to_string()).unwrap();
        scripted(&manifest, &file, "call each tool that asks", &rules);
    }
}

#[test]
fn web_research() {
    let Some(dir) = agent("web-research") else {
        return;
    };
    holds(&dir, &[], &[], None);
}

#[test]
fn notes_keeper() {
    let Some(dir) = agent("notes-keeper") else {
        return;
    };
    // The notes its replies read: `projects/garden.md` only here, so a read
    // that succeeds shows the server's "." is the run's folder.
    holds(
        &dir,
        &[
            (
                "groceries.md",
                "# Groceries\n\n- oat milk\n- coffee beans\n-  eggs\n",
            ),
            (
                "projects/garden.md",
                "# garden plan\nPlant tomatoes after the last frost (mid May).\nBasil goes next to the tomatoes.\n",
            ),
        ],
        &[],
        None,
    );
}

#[test]
fn code_reviewer() {
    let Some(dir) = agent("code-reviewer") else {
        return;
    };
    // A tiny TypeScript project: ESLint's config covers `.js` only, so
    // `src/util.js` has its findings and the `.ts` files are read.
    holds(
        &dir,
        &[
            ("eslint.config.js", "export default [\n  {\n    files: [\"**/*.js\"],\n    rules: {\n      \"no-unused-vars\": \"error\",\n      \"no-eval\": \"error\",\n      \"eqeqeq\": \"error\"\n    }\n  }\n];\n"),
            ("package.json", "{\n  \"name\": \"tiny-ts\",\n  \"version\": \"1.0.0\",\n  \"private\": true,\n  \"type\": \"module\",\n  \"scripts\": {\n    \"lint\": \"eslint .\",\n    \"typecheck\": \"tsc --noEmit\"\n  },\n  \"devDependencies\": {\n    \"eslint\": \"9.39.1\",\n    \"typescript\": \"5.9.3\"\n  }\n}\n"),
            ("src/index.ts", "import { add, total } from \"./math\";\n\nconsole.log(add(2, 3), total);\n"),
            ("src/math.ts", "export function add(a: number, b: number): number {\n  return a + b;\n}\n\nexport const total: number = add(1, \"2\");\n"),
            ("src/util.js", "export function check(value) {\n  const unused = 42;\n  if (value == null) {\n    return eval(\"'empty'\");\n  }\n  return value;\n}\n"),
            ("tsconfig.json", "{\n  \"compilerOptions\": {\n    \"target\": \"ES2022\",\n    \"module\": \"ESNext\",\n    \"moduleResolution\": \"Bundler\",\n    \"strict\": true,\n    \"noEmit\": true\n  },\n  \"include\": [\"src\"]\n}\n"),
        ],
        &["no-writes.json"],
        None,
    );
}

#[test]
fn self_extending_reviewer() {
    let Some(dir) = agent("self-extending-reviewer") else {
        return;
    };
    // A tiny project whose ESLint config covers `.js` only: ESLint's
    // findings in `src/util.js`, `src/math.ts` ignored. Each reply file on
    // the page (`load_plugin` is refused under `--once`), against the
    // live sources (design §14 "The demo"): npm's registry down fails it.
    // `not-offered.json`'s package, `acme-lint-mcp`, must stay unpublished
    // (were it found, its load would ask, a question not expected here).
    let files: &[(&str, &str)] = &[
        ("eslint.config.js", "export default [\n  {\n    files: [\"**/*.js\"],\n    rules: {\n      \"no-unused-vars\": \"error\",\n      \"no-eval\": \"error\",\n      \"eqeqeq\": \"error\"\n    }\n  }\n];\n"),
        ("package.json", "{\n  \"name\": \"tiny-ts\",\n  \"version\": \"1.0.0\",\n  \"private\": true,\n  \"type\": \"module\"\n}\n"),
        ("src/util.js", "export function check(value) {\n  const unused = 42;\n  if (value == null) {\n    return eval(\"'empty'\");\n  }\n  return value;\n}\n"),
        ("src/math.ts", "export function add(a: number, b: number): number {\n  return a + b;\n}\n\nexport const total: number = add(1, \"2\");\n"),
    ];
    let start = "Start the MCP server @eslint/mcp@";
    holds_with(
        &dir,
        files,
        &[],
        &[
            Page {
                file: "demo.json",
                answers: &[
                    (start, "Allow"),
                    ("Keep the MCP server eslint? It offers: lint-files", "Allow"),
                    (
                        "Run `eslint__lint-files`, a tool the agent loaded this session?",
                        "Allow",
                    ),
                ],
                refused: None,
                block: &["[[mcp]]\nname = \"eslint\"\nnpm  = \"@eslint/mcp@"],
                offers: &["lint-files"],
                home: &[],
            },
            Page {
                file: "blocked.json",
                answers: &[(start, "Block")],
                refused: Some("the owner blocked it (start it)"),
                block: &[],
                offers: &[],
                home: &[],
            },
            Page {
                file: "not-offered.json",
                answers: &[],
                refused: Some("acme-lint-mcp is not among this session's candidates"),
                block: &[],
                offers: &[],
                home: &[],
            },
        ],
        None,
    );

    // Under `--once` the demo stops at `load_plugin`, as the skill says,
    // in the words it quotes.
    const ONCE: &str =
        "load_plugin refused it: not under --once: nobody is there to allow it (nothing was counted)";
    let (work, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    for (path, contents) in files {
        let path = work.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    let task = std::fs::read_to_string(dir.join("scripts/demo.task")).unwrap();
    let ran = yobuild(
        &[
            "run",
            &dir.join("agent.toml").display().to_string(),
            "--scripted",
            &dir.join("scripts/demo.json").display().to_string(),
            "--once",
            "--locked",
            task.trim(),
        ],
        work.path(),
        home.path(),
    );
    let all = format!("demo.json --once:\n{}{}", ran.stdout, ran.stderr);
    assert!(ran.ok && ran.stdout.ends_with("\nCompleted\n"), "{all}");
    for said in [
        format!("\n  load_plugin ✗ {ONCE}\n"),
        format!("\n  eslint__lint-files ✗ {NOBODY}\n"),
        "the reply file's expectation for `eslint__lint-files` was not checked: the call did not run".to_owned(),
    ] {
        assert!(ran.stderr.contains(&said), "{said:?} not in: {all}");
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("skill/yobuild/reference");
    for quoted in ["scripted.md", "failures.md"] {
        let text = std::fs::read_to_string(root.join(quoted)).unwrap();
        let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(flat.contains(ONCE), "{quoted} does not quote `{ONCE}`");
    }
}

#[test]
fn project_notebook() {
    let Some(dir) = agent("project-notebook") else {
        return;
    };
    // The sample project its reply files read. Each reply file on the page
    // (`load_plugin` is refused under `--once`), against the live npm
    // (design §14 "Stage 2", PR 7): pi-memory installed, started in a
    // runtime of its own and its tools called — the write in this test's
    // own `HOME` (`~/.pi/agent/memory`, a temp folder each run).
    // `not-offered.json`'s package, `brainjar-pi-notes`, must stay
    // unpublished (were it found, its load would ask, a question not
    // expected here).
    let files: &[(&str, &str)] = &[
        ("README.md", "# tiny-shop\n\nA small order service: create orders, list them, look one up.\n\n    npm start        # listens on PORT (default 3000)\n\nOrders are kept in memory (src/store.js); they are lost on restart.\n"),
        ("package.json", "{\n  \"name\": \"tiny-shop\",\n  \"version\": \"0.1.0\",\n  \"private\": true,\n  \"type\": \"module\",\n  \"scripts\": { \"start\": \"node src/server.js\" }\n}\n"),
        ("src/config.js", "export const config = {\n  port: Number(process.env.PORT ?? 3000),\n  maxItemsPerOrder: 20,\n  currency: 'EUR',\n};\n"),
        ("src/server.js", "import http from 'node:http';\nimport { config } from './config.js';\nimport { createOrder, getOrder, listOrders } from './store.js';\n\nconst server = http.createServer(async (req, res) => {\n  const url = new URL(req.url, 'http://localhost');\n  if (req.method === 'GET' && url.pathname === '/orders') {\n    return send(res, 200, listOrders());\n  }\n  const match = url.pathname.match(/^\\/orders\\/(\\d+)$/);\n  if (req.method === 'GET' && match) {\n    const order = getOrder(Number(match[1]));\n    return order ? send(res, 200, order) : send(res, 404, { error: 'not found' });\n  }\n  if (req.method === 'POST' && url.pathname === '/orders') {\n    let body = '';\n    for await (const chunk of req) body += chunk;\n    const { items } = JSON.parse(body || '{}');\n    if (!Array.isArray(items) || items.length > config.maxItemsPerOrder) {\n      return send(res, 400, { error: 'bad items' });\n    }\n    return send(res, 201, createOrder(items));\n  }\n  send(res, 404, { error: 'not found' });\n});\n\nfunction send(res, status, data) {\n  res.writeHead(status, { 'content-type': 'application/json' });\n  res.end(JSON.stringify(data));\n}\n\nserver.listen(config.port, () => console.log(`tiny-shop on ${config.port}`));\n"),
        ("src/store.js", "const orders = new Map();\nlet nextId = 1;\n\nexport function createOrder(items) {\n  const order = { id: nextId++, items, createdAt: new Date().toISOString() };\n  orders.set(order.id, order);\n  return order;\n}\n\nexport function getOrder(id) {\n  return orders.get(id) ?? null;\n}\n\nexport function listOrders() {\n  return [...orders.values()];\n}\n"),
    ];
    let start = "Start the pi package pi-memory@";
    let read = "Run `pi-memory__memory_read`, a tool the agent loaded this session?";
    holds_with(
        &dir,
        files,
        &[],
        &[
            Page {
                file: "demo.json",
                answers: &[
                    (start, "Allow"),
                    ("Keep the pi package pi-memory? It offers: ", "Allow"),
                    (read, "Allow"),
                    (
                        "Run `pi-memory__memory_write`, a tool the agent loaded this session?",
                        "Allow",
                    ),
                    (read, "Allow"),
                ],
                refused: None,
                block: &[
                    "[[plugin]]\nname = \"pi-memory\"\npi   = \"pi-memory@",
                    "In agent.toml a pi package runs with the other pi plugins",
                ],
                offers: &["memory_read", "memory_write"],
                home: &[(
                    ".pi/agent/memory/MEMORY.md",
                    "#tiny-shop #decision 2026-10-28",
                )],
            },
            Page {
                file: "blocked.json",
                answers: &[(start, "Block")],
                refused: Some("the owner blocked it (start it)"),
                block: &[],
                offers: &[],
                home: &[],
            },
            Page {
                file: "not-offered.json",
                answers: &[],
                refused: Some("brainjar-pi-notes is not among this session's candidates"),
                block: &[],
                offers: &[],
                home: &[],
            },
        ],
        None,
    );
}

#[test]
fn spreadsheet_clerk() {
    let Some(dir) = agent("spreadsheet-clerk") else {
        return;
    };
    // Two months' bank exports, and last month's spreadsheet standing in
    // (text, seen only by `list_files`) for the "exists" reply file.
    holds(
        &dir,
        &[
            ("expenses-2026-08.csv", "date,description,category,amount\n2026-08-01,Rent August,Housing,1200.00\n2026-08-04,FreshMart,Groceries,71.10\n2026-08-18,Water bill,Utilities,30.25\n"),
            ("expenses-2026-08.xlsx", "placeholder: stands for last month's spreadsheet the owner already made\n"),
            ("expenses-2026-09.csv", "date,description,category,amount\n2026-09-01,Rent September,Housing,1200.00\n2026-09-02,FreshMart,Groceries,64.20\n2026-09-05,City Transit pass,Transport,45.00\n2026-09-09,FreshMart,Groceries,38.75\n2026-09-12,Power & Light,Utilities,72.40\n2026-09-15,Cinema,Entertainment,24.00\n2026-09-20,Corner Bakery,Groceries,9.80\n2026-09-27,Fuel stop,Transport,51.30\n"),
        ],
        &[],
        None,
    );
}

#[test]
fn code_health() {
    let Some(dir) = agent("code-health") else {
        return;
    };
    // Its results are checked, not only its `✓` (design §11 "pi-lens in
    // the gallery"): the demo's last `lens_diagnostics` must expect the
    // type error, so a reply file without one cannot pass by checking
    // nothing.
    let demo: Json =
        serde_json::from_str(&std::fs::read_to_string(dir.join("scripts/demo.json")).unwrap())
            .unwrap();
    let last = demo
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["tools"].as_array())
        .flatten()
        .rfind(|c| c["name"] == "lens_diagnostics")
        .expect("the demo runs lens_diagnostics");
    let expects = |e: &Json| match e {
        Json::String(t) => t.contains("2345"),
        Json::Array(ts) => ts
            .iter()
            .any(|t| t.as_str().is_some_and(|t| t.contains("2345"))),
        _ => false,
    };
    assert!(expects(&last["expect"]), "{last}");
    // pi-lens's language servers (about 87 MB) are fetched once into a
    // `HOME` every run shares (kept between local runs; CI removes it).
    let home = Path::new(env!("CARGO_TARGET_TMPDIR")).join("pi-lens-home");
    std::fs::create_dir_all(&home).unwrap();
    // The sample project the agent was built against: `"2"` where a
    // `number` goes.
    holds(
        &dir,
        &[
            ("tsconfig.json", "{ \"compilerOptions\": { \"strict\": true, \"noEmit\": true }, \"include\": [\"src\"] }\n"),
            ("src/sum.ts", "export function sum(a: number, b: number): number {\n  return a + b\n}\nexport const total: number = sum(1, \"2\")\n"),
        ],
        &[],
        Some(&home),
    );
}
