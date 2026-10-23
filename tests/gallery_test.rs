//! The gallery (`gallery/<name>/`, design §11): each agent `check`ed
//! `--locked`, and each of its reply files (`scripts/<x>.json`, with its task
//! in `scripts/<x>.task`) run scripted under `--once --locked`, in a temp
//! folder, with a temp `HOME` (so a plugin's cache or config there is the
//! test's own). What holds an agent:
//! - `check`: `ok`, no note, each name an entry's `allow` lists `allowed`,
//!   and no tool that writes with `tools = "read-only"`;
//! - each reply file (its folder holding the files the agent's test puts
//!   there, unchanged afterwards): `Completed` with its last `text` as the answer, each call it
//!   scripts made once, no `Error:` notice, every call to a tool that asks
//!   denied (nobody answers), every other call `✓` —
//!   but a plugin's or server's own `✗` (a third party's day: a search
//!   engine down), never one of the bridge's (its plugin unloaded or
//!   stopped answering), and never a denial;
//!   A reply file meant to show refusals: every call `✗`;
//! - every tool `check` says asks, called with nobody there: denied.
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

/// The agent's gallery folder, when the adapters are installed (required
/// in CI).
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
/// first (path, contents), each the same afterwards; the reply files meant
/// to show refusals (every call `✗`, whatever the reason); the tools that
/// ask (each call denied: nobody answers); the plugins' and servers' tools,
/// whose own `✗` is tolerated; a `HOME` shared by every run, when the
/// agent's plugin keeps a large cache there (else a temporary one each).
struct Rules<'a> {
    files: &'a [(&'a str, &'a str)],
    home: Option<&'a Path>,
    refusing: &'a [&'a str],
    asks: HashSet<String>,
    theirs: HashSet<String>,
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
            assert!(
                n == GRANTED_DSH || n.starts_with(KEPT) || here(n) || skills,
                "{report:#}"
            );
            if n.starts_with(KEPT) {
                assert_eq!(n.contains("probed alone"), probed, "{n}");
            }
        }
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
    for file in &files {
        let pair = |x| file.with_extension(x).is_file();
        assert!(
            pair("json") && pair("task"),
            "{}: a reply file and its task, in pairs",
            file.display()
        );
        if file.extension().is_some_and(|x| x == "json") {
            let task = std::fs::read_to_string(file.with_extension("task")).unwrap();
            scripted(&manifest, file, task.trim(), &rules);
            if file.file_stem().is_some_and(|s| s == "demo") {
                demo = Some(file.clone());
            }
        }
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
