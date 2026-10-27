//! `PluginHost` with a real Node runtime: a plugin is reloaded after its
//! file changed (new code), kept when its new code does not load, and
//! unloaded (its tool gone); a runtime added while running, and removed.
//!
//! Needs `npm ci` in `frontend/plugins/`; skipped (passing, with `SKIPPED:`) without
//! it unless `YOAGENT_RUTIS_REQUIRE_RUNTIMES=1`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rutis::Ctx;
use serde_json::json;
use yoagent::provider::mock::{MockResponse, MockToolCall};
use yoagent::provider::{MockProvider, ModelConfig};
use yoagent::{Agent, AgentMessage, Content, Message};
use yoagent_rutis::RutisBridge;
use yobuild::frontend::host::{Inherit, PluginHost, Row};

const WAIT: Duration = Duration::from_secs(60);

fn plugins() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("frontend/plugins")
}

fn runtime_installed() -> bool {
    let installed = plugins()
        .join("node_modules/@arcships/rutis-runtime")
        .exists();
    if !installed {
        assert!(
            std::env::var("YOAGENT_RUTIS_REQUIRE_RUNTIMES").as_deref() != Ok("1"),
            "the plugins' packages are not installed (npm ci in {})",
            plugins().display()
        );
        println!("SKIPPED: npm ci in {} first", plugins().display());
    }
    installed
}

/// A plugin offering a `version` tool that answers `version`, and appending
/// a line to `starts` each time it starts. `@arcships/rutis` by file URL: the
/// plugin lives in a temp dir.
fn write_plugin(path: &Path, starts: &Path, version: &str) {
    write_named(path, starts, "reloadable", "version", version)
}

/// A plugin that imports fine but fails when it starts.
fn write_failing_plugin(path: &Path) {
    let rutis = plugins().join("node_modules/@arcships/rutis/src/index.mjs");
    std::fs::write(
        path,
        format!(
            r#"import {{ definePlugin }} from 'file://{}'
export default definePlugin({{
  inject: ['yoagent'],
  apply() {{
    throw new Error('this version breaks on start')
  }},
}})
"#,
            rutis.display()
        ),
    )
    .unwrap();
}

/// `write_plugin` with its own handler and tool names.
fn write_named(path: &Path, starts: &Path, handler: &str, tool: &str, version: &str) {
    let rutis = plugins().join("node_modules/@arcships/rutis/src/index.mjs");
    std::fs::write(
        path,
        format!(
            r#"import {{ appendFileSync }} from 'node:fs'
import {{ definePlugin }} from 'file://{rutis}'
export default definePlugin({{
  inject: ['yoagent'],
  apply(ctx) {{
    appendFileSync({starts:?}, '{version}\n')
    ctx.effect(ctx.use('yoagent').register('{handler}', {{
      async tools() {{
        return [{{ name: '{tool}', description: 'Which version runs.', parameters: {{ type: 'object', properties: {{}} }} }}]
      }},
      async call_tool() {{
        return {{ text: '{version}' }}
      }},
    }}))
  }},
}})
"#,
            rutis = rutis.display(),
            starts = starts.display().to_string(),
        ),
    )
    .unwrap();
}

/// What the `version` tool answers now (or the error a run gets).
async fn version(agent: &mut Agent) -> String {
    let before = agent.messages().len();
    let mut events = agent.prompt("which version?").await;
    tokio::time::timeout(WAIT, async { while events.recv().await.is_some() {} })
        .await
        .expect("the run ends");
    agent.finish().await;
    agent.messages()[before..]
        .iter()
        .find_map(|m| match m {
            AgentMessage::Llm(Message::ToolResult { content, .. }) => {
                content.iter().find_map(|c| match c {
                    Content::Text { text } => Some(text.clone()),
                    _ => None,
                })
            }
            _ => None,
        })
        .expect("the tool answered")
}

async fn registered(bridge: &RutisBridge, present: bool) {
    let registry = bridge.registry().clone();
    tokio::time::timeout(WAIT, async {
        while registry.handlers().iter().any(|h| h.name() == "reloadable") != present {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the handler (un)registered");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_reloads_with_its_edited_file_and_unloads() {
    if !runtime_installed() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // ESM, as plugin folders are: tsx loads a `.ts` file outside a
    // `"type": "module"` package as CommonJS, whose cache a reload cannot pass.
    std::fs::write(dir.path().join("package.json"), r#"{ "type": "module" }"#).unwrap();
    let file = dir.path().join("reloadable.ts");
    let starts = dir.path().join("starts.txt");
    write_plugin(&file, &starts, "version one");

    let root = Ctx::root().unwrap();
    let bridge = RutisBridge::install(&root).unwrap();
    let mut host = PluginHost::builder()
        .node("ui", plugins())
        .share("yoagent")
        .start(&root)
        .await
        .unwrap();
    host.load([Row::new("reloadable", file.display()).runtime("ui")])
        .await
        .unwrap();
    registered(&bridge, true).await;

    let call = || {
        MockResponse::ToolCalls(vec![MockToolCall {
            name: "version".into(),
            arguments: json!({}),
            provider_metadata: None,
        }])
    };
    let done = || MockResponse::Text("ok".into());
    let mut agent = Agent::from_provider(
        MockProvider::new(vec![
            call(),
            done(),
            call(),
            done(),
            call(),
            done(),
            call(),
            done(),
            call(),
            done(),
            call(),
            done(),
            call(),
            done(),
        ]),
        ModelConfig::mock(),
    )
    .with_extension(bridge.extension());
    assert_eq!(version(&mut agent).await, "version one");

    // Edited, then reloaded: the new code runs.
    tokio::time::sleep(Duration::from_millis(20)).await;
    write_plugin(&file, &starts, "version two");
    host.reload("reloadable").await.unwrap();
    registered(&bridge, true).await;
    assert_eq!(version(&mut agent).await, "version two");

    // Reloaded without a change.
    host.reload("reloadable").await.unwrap();
    registered(&bridge, true).await;
    assert_eq!(version(&mut agent).await, "version two");
    assert_eq!(
        std::fs::read_to_string(&starts).unwrap(),
        "version one\nversion two\nversion two\n",
        "each reload restarted it, the first with the new code"
    );

    // New code that does not load: an error, and the old code keeps running.
    tokio::time::sleep(Duration::from_millis(20)).await;
    std::fs::write(&file, "export default definePlugin({ this is not code").unwrap();
    let refused = host.reload("reloadable").await.unwrap_err().to_string();
    assert!(refused.contains("running version stays"), "{refused}");
    assert!(host.is_running("reloadable"));
    registered(&bridge, true).await;
    assert_eq!(version(&mut agent).await, "version two");

    // New code that imports but fails when it starts: the old code is gone
    // by then, so the plugin is stopped — and the error says so.
    tokio::time::sleep(Duration::from_millis(20)).await;
    write_failing_plugin(&file);
    let stopped = host.reload("reloadable").await.unwrap_err().to_string();
    assert!(stopped.contains("is stopped"), "{stopped}");
    assert!(!host.is_running("reloadable"));
    registered(&bridge, false).await;
    assert!(version(&mut agent).await.contains("not found"));
    // A stopped plugin does not block the others.
    let other = dir.path().join("other.ts");
    write_named(&other, &starts, "other", "other_version", "other one");
    host.load([Row::new("other", other.display()).runtime("ui")])
        .await
        .unwrap();
    // The next good save brings it back.
    tokio::time::sleep(Duration::from_millis(20)).await;
    write_plugin(&file, &starts, "version three");
    host.reload("reloadable").await.unwrap();
    registered(&bridge, true).await;
    assert_eq!(version(&mut agent).await, "version three");

    // Unloaded: the tool is gone.
    host.unload("reloadable").await.unwrap();
    registered(&bridge, false).await;
    let after = version(&mut agent).await;
    assert!(after.contains("not found"), "{after}");
    assert!(
        host.unload("reloadable").await.is_err(),
        "nothing left to unload"
    );
    assert!(host.reload("nope").await.is_err());
    root.shutdown().await.unwrap();
}

/// rutis 0.9: a Node plugin that disposes its own fiber is reported to the
/// host (rutis 0.8 left it reading as running, arcships/rutis#247), so
/// `is_running` turns false.
#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_that_disposes_itself_is_not_running() {
    if !runtime_installed() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), r#"{ "type": "module" }"#).unwrap();
    let file = dir.path().join("leaves.mjs");
    // A plain Cordis plugin: it starts, then disposes itself — late enough
    // that `load` (polling every 50 ms) has seen it running first.
    std::fs::write(
        &file,
        "export const name = 'leaves'\n\
         export function apply(ctx) { setTimeout(() => ctx.fiber.dispose(), 1500) }\n",
    )
    .unwrap();

    let root = Ctx::root().unwrap();
    let mut host = PluginHost::builder()
        .node("ui", plugins())
        .start(&root)
        .await
        .unwrap();
    host.load([Row::new("leaves", file.display()).runtime("ui")])
        .await
        .unwrap();
    tokio::time::timeout(WAIT, async {
        while host.is_running("leaves") {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the host sees the plugin end");
    // Ended, not failed: once it settles (it reads `Unloading` briefly), the
    // row is inactive (rutis-loader would disable a row of its own; this
    // one comes from the host's layer).
    let ended = |host: &PluginHost| {
        host.status()
            .iter()
            .any(|(id, state)| id == "leaves" && (state == "inactive" || state == "disabled"))
    };
    tokio::time::timeout(WAIT, async {
        while !ended(&host) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the row settles as ended: {:?}", host.status()));
    root.shutdown().await.unwrap();
}

/// `PluginHostBuilder::env` reaches its own runtime's process only, and a
/// runtime that is not started is an error, not silently ignored.
#[tokio::test(flavor = "multi_thread")]
async fn env_reaches_only_its_runtime_and_an_unknown_one_is_an_error() {
    if !runtime_installed() {
        return;
    }
    let root = Ctx::root().unwrap();
    let wrong = PluginHost::builder()
        .node("ui", plugins())
        .env("iu", "YOAGENT_ENV_TEST", "1")
        .start(&root)
        .await;
    assert!(
        matches!(&wrong, Err(e) if e.to_string().contains("`iu`")),
        "a typo must not pass"
    );

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), r#"{ "type": "module" }"#).unwrap();
    let writer = |name: &str| {
        let file = dir.path().join(format!("{name}.mjs"));
        let out = dir.path().join(format!("{name}.txt"));
        std::fs::write(
            &file,
            format!(
                "import {{ writeFileSync }} from 'node:fs'\n\
                 export const name = '{name}'\n\
                 export function apply() {{ writeFileSync({:?}, String(process.env.YOAGENT_ENV_TEST)) }}\n",
                out.display().to_string()
            ),
        )
        .unwrap();
        (file, out)
    };
    let (a, a_out) = writer("a");
    let (b, b_out) = writer("b");
    let mut host = PluginHost::builder()
        .node("one", plugins())
        .node("two", plugins())
        .env("one", "YOAGENT_ENV_TEST", "1")
        .start(&root)
        .await
        .unwrap();
    host.load([
        Row::new("a", a.display()).runtime("one"),
        Row::new("b", b.display()).runtime("two"),
    ])
    .await
    .unwrap();
    assert_eq!(written(&a_out).await, "1");
    assert_eq!(written(&b_out).await, "undefined");
    root.shutdown().await.unwrap();
}

/// A withheld variable is taken out of every runtime's environment (the
/// runtime started through `env -u`), but where `keep` or an `env` entry of
/// that name says otherwise; `keep` for a runtime that is not started is an
/// error. (The kept environment, which takes out everything else too:
/// `kept_env_test`.)
#[tokio::test(flavor = "multi_thread")]
async fn a_withheld_variable_is_gone_from_every_runtime_unless_kept_or_set() {
    if !runtime_installed() {
        return;
    }
    // Set by cargo for the tests: what a runtime would inherit.
    const NAME: &str = "CARGO_MANIFEST_DIR";
    let inherited = std::env::var(NAME).unwrap();
    assert!(!inherited.is_empty());
    let root = Ctx::root().unwrap();
    let wrong = PluginHost::builder()
        .node("ui", plugins())
        .withhold(NAME)
        .keep("iu", NAME)
        .start(&root)
        .await;
    assert!(
        matches!(&wrong, Err(e) if e.to_string().contains("`iu`")),
        "a typo must not pass"
    );
    // Through `env`, a `node` the runtime cannot find would only show as
    // `env` exiting 127: it is looked for first, on the runtime's `PATH`.
    #[cfg(unix)]
    {
        let empty = tempfile::tempdir().unwrap();
        let missing = PluginHost::builder()
            .node("ui", plugins())
            .withhold(NAME)
            .env("ui", "PATH", empty.path())
            .start(&root)
            .await;
        assert!(
            matches!(&missing, Err(e) if e.to_string() == "runtime `ui`: yobuild cannot start node: it is not on PATH"),
            "{:?}",
            missing.err().map(|e| e.to_string())
        );
    }

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), r#"{ "type": "module" }"#).unwrap();
    let writer = |name: &str| {
        let file = dir.path().join(format!("{name}.mjs"));
        let out = dir.path().join(format!("{name}.txt"));
        std::fs::write(
            &file,
            format!(
                "import {{ writeFileSync }} from 'node:fs'\n\
                 export const name = '{name}'\n\
                 export function apply() {{ writeFileSync({:?}, JSON.stringify([process.env.{NAME} ?? null, process.env.DYLD_YOBUILD_TEST ?? null])) }}\n",
                out.display().to_string()
            ),
        )
        .unwrap();
        (file, out)
    };
    let (a, a_out) = writer("a");
    let (b, b_out) = writer("b");
    let (c, c_out) = writer("c");
    let mut host = PluginHost::builder()
        .node("one", plugins())
        .node("two", plugins())
        .node("three", plugins())
        .withhold(NAME)
        .env("two", NAME, "given")
        .keep("three", NAME)
        // A runtime's own `DYLD_*` entry: dropped by the protected
        // `/usr/bin/env` on macOS, which every runtime starts through.
        .env("one", "DYLD_YOBUILD_TEST", "1")
        .env("three", "DYLD_YOBUILD_TEST", "1")
        .start(&root)
        .await
        .unwrap();
    host.load([
        Row::new("a", a.display()).runtime("one"),
        Row::new("b", b.display()).runtime("two"),
        Row::new("c", c.display()).runtime("three"),
    ])
    .await
    .unwrap();
    // Gone, not empty (set empty only where there is no `env` to exec).
    let gone = match cfg!(unix) {
        true => "null",
        false => r#""""#,
    };
    // Through `env` on macOS with System Integrity Protection on (the
    // default; GitHub's macOS runners have it off), `DYLD_*` is gone,
    // kept or not.
    let dropped = match cfg!(target_os = "macos") && sip_enabled() {
        true => "null",
        false => r#""1""#,
    };
    assert_eq!(written(&a_out).await, format!("[{gone},{dropped}]"));
    assert_eq!(written(&b_out).await, r#"["given",null]"#);
    assert_eq!(
        written(&c_out).await,
        format!("[{},{dropped}]", serde_json::to_string(&inherited).unwrap())
    );
    root.shutdown().await.unwrap();
}

/// macOS's System Integrity Protection is on (`csrutil status`), under
/// which starting the protected `/usr/bin/env` drops `DYLD_*` variables.
/// A "Custom Configuration" reads `status: unknown (…)`: taken as off.
fn sip_enabled() -> bool {
    std::process::Command::new("csrutil")
        .arg("status")
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains("status: enabled"))
}

/// The text of `path` once a plugin wrote it.
async fn written(path: &Path) -> String {
    tokio::time::timeout(WAIT, async {
        loop {
            if let Ok(text) = std::fs::read_to_string(path) {
                return text;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the plugin wrote")
}

/// A plugin `<stem>.mjs` in `dir` that registers a handler `<prefix>-<pid>`
/// (its `yoagent` injected) and writes `<stem>-<pid>.json`: its process,
/// `CARGO_MANIFEST_DIR` and whether it has `PATH`.
fn pid_plugin(dir: &Path, stem: &str, prefix: &str) -> PathBuf {
    let file = dir.join(format!("{stem}.mjs"));
    let rutis = plugins().join("node_modules/@arcships/rutis/src/index.mjs");
    std::fs::write(
        &file,
        format!(
            r#"import {{ writeFileSync }} from 'node:fs'
import {{ definePlugin }} from 'file://{rutis}'
export default definePlugin({{
  inject: ['yoagent'],
  apply(ctx) {{
    ctx.effect(ctx.use('yoagent').register('{prefix}-' + process.pid, {{
      async tools() {{ return [] }},
      async call_tool() {{ return {{ text: '' }} }},
    }}))
    writeFileSync({out:?} + '/{stem}-' + process.pid + '.json', JSON.stringify({{
      pid: process.pid,
      env: process.env.CARGO_MANIFEST_DIR ?? null,
      path: process.env.PATH !== undefined,
    }}))
  }},
}})
"#,
            rutis = rutis.display(),
            out = dir.display().to_string(),
        ),
    )
    .unwrap();
    file
}

/// A plain Cordis plugin `<stem>.mjs` in `dir`, injecting nothing, that
/// appends `text` to `<stem>-<pid>.txt` each time it starts.
fn plain_plugin(dir: &Path, stem: &str, text: &str) -> PathBuf {
    let file = dir.join(format!("{stem}.mjs"));
    std::fs::write(
        &file,
        format!(
            "import {{ appendFileSync }} from 'node:fs'\n\
             export const name = '{stem}'\n\
             export function apply() {{ appendFileSync({:?} + '/{stem}-' + process.pid + '.txt', '{text}\\n') }}\n",
            dir.display().to_string()
        ),
    )
    .unwrap();
    file
}

/// The files `<stem>-<pid>.<ext>` in `dir`: each process's, by pid.
fn written_by(dir: &Path, stem: &str) -> Vec<(u64, PathBuf)> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| {
            let path = e.unwrap().path();
            let name = path.file_stem()?.to_str()?.to_owned();
            let pid = name.strip_prefix(&format!("{stem}-"))?.parse().ok()?;
            Some((pid, path))
        })
        .collect()
}

/// What `<stem>-<pid>.json` says, once whole.
async fn report(dir: &Path, stem: &str, pid: u64) -> serde_json::Value {
    let path = dir.join(format!("{stem}-{pid}.json"));
    tokio::time::timeout(WAIT, async {
        loop {
            if let Some(json) = std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
            {
                return json;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the plugin wrote")
}

/// Waits until `count` processes wrote `<stem>-<pid>.*`, and gives them.
async fn processes_of(dir: &Path, stem: &str, count: usize) -> Vec<u64> {
    tokio::time::timeout(WAIT, async {
        loop {
            let mut pids: Vec<u64> = written_by(dir, stem).into_iter().map(|(p, _)| p).collect();
            if pids.len() >= count {
                pids.sort();
                return pids;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{count} processes of {stem}"))
}

/// The handlers registered now whose names begin with one of `prefixes`.
fn handlers_named(bridge: &RutisBridge, prefixes: &[&str]) -> Vec<String> {
    let mut names: Vec<String> = bridge
        .registry()
        .handlers()
        .iter()
        .map(|h| h.name().to_owned())
        .filter(|n| prefixes.iter().any(|p| n.starts_with(p)))
        .collect();
    names.sort();
    names
}

async fn handlers_become(bridge: &RutisBridge, prefixes: &[&str], mut want: Vec<String>) {
    want.sort();
    let found = tokio::time::timeout(WAIT, async {
        while handlers_named(bridge, prefixes) != want {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(
        found.is_ok(),
        "{:?}, not {want:?}",
        handlers_named(bridge, prefixes)
    );
}

#[cfg(unix)]
fn alive(pid: u64) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

fn refused(result: Result<(), Box<dyn std::error::Error + Send + Sync>>) -> String {
    result.unwrap_err().to_string()
}

/// A runtime added while the host runs (design §14 "Stage 2"): it loads a
/// file another runtime loads too, in a process of its own, with what every
/// runtime keeps (`inherit`) and nothing `keep` gave another; it reaches
/// only the services it was given, refuses a row that declares one it
/// provides, reloads and unloads its rows, never takes a row naming no
/// runtime, and is removed whole — its plugins and its process — leaving
/// the other runtimes' and their routes as they should be.
#[tokio::test(flavor = "multi_thread")]
async fn a_runtime_added_while_running_loads_its_own_rows_and_is_removed_whole() {
    if !runtime_installed() {
        return;
    }
    const NAME: &str = "CARGO_MANIFEST_DIR";
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path();
    std::fs::write(dir.join("package.json"), r#"{ "type": "module" }"#).unwrap();
    let pid = pid_plugin(dir, "pid", "h");

    let root = Ctx::root().unwrap();
    let bridge = RutisBridge::install(&root).unwrap();
    let mut host = PluginHost::builder()
        .node("ui", plugins())
        .node("two", plugins())
        .share("yoagent")
        .inherit(Inherit::only(["PATH"], Vec::<&str>::new()))
        .keep("ui", NAME)
        .start(&root)
        .await
        .unwrap();
    host.load([Row::new("first", pid.display()).runtime("ui")])
        .await
        .unwrap();

    for (name, shared, error) in [
        ("ui", vec!["yoagent"], "a runtime `ui` runs already"),
        ("Loaded A", vec![], "lowercase"),
        ("", vec![], "lowercase"),
        ("a::b", vec![], "lowercase"),
        (
            "loaded-b",
            vec!["nope"],
            "runtime `loaded-b`: `nope` is not a service this host shares (yoagent)",
        ),
    ] {
        let said = refused(host.add_runtime(name, plugins(), &shared).await);
        assert!(said.contains(error), "{name}: {said}");
    }
    host.add_runtime("loaded-a", plugins(), &["yoagent"])
        .await
        .unwrap();
    assert_eq!(
        refused(host.add_runtime("loaded-a", plugins(), &[]).await),
        "a runtime `loaded-a` runs already"
    );
    // A name that would read as an added runtime's row is no row's.
    assert_eq!(
        refused(
            host.load([Row::new("odd", "loaded-a::x.mjs").runtime("ui")])
                .await
        ),
        "`loaded-a::x.mjs`: a plugin's name holds no `::`"
    );

    // The same file, loaded again in the runtime added: two processes.
    host.load([Row::new("second", pid.display()).runtime("loaded-a")])
        .await
        .unwrap();
    let pids = processes_of(dir, "pid", 2).await;
    let (mut first, mut second) = (0, 0);
    for p in pids {
        let said = report(dir, "pid", p).await;
        // Both keep what every runtime keeps; only `ui` was given more.
        assert_eq!(said["path"], json!(true), "{said}");
        match said["env"].is_string() {
            true => first = p,
            false => second = p,
        }
    }
    assert!(first != 0 && second != 0 && first != second);
    handlers_become(
        &bridge,
        &["h-"],
        vec![format!("h-{first}"), format!("h-{second}")],
    )
    .await;

    // A row naming no runtime goes to the host's own runtimes, never to an
    // added one.
    let other = pid_plugin(dir, "other", "g");
    host.load([Row::new("unrouted", other.display())])
        .await
        .unwrap();
    let unrouted = processes_of(dir, "other", 1).await;
    assert_eq!(unrouted, [first], "the first runtime that has it: `ui`");
    host.unload("unrouted").await.unwrap();

    // Reloaded in the added runtime: its new code runs there.
    let edited = plain_plugin(dir, "edited", "one");
    host.load([Row::new("fifth", edited.display()).runtime("loaded-a")])
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    plain_plugin(dir, "edited", "two");
    host.reload("fifth").await.unwrap();
    let path = dir.join(format!("edited-{second}.txt"));
    let lines = tokio::time::timeout(WAIT, async {
        loop {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            if text.lines().count() >= 2 {
                return text;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("reloaded");
    assert_eq!(lines, "one\ntwo\n");
    host.unload("fifth").await.unwrap();

    // `first` unloaded: the loader keeps that file's resolution, `ui`'s,
    // so another of the host's runtimes may not load it; `second`, in the
    // added runtime, goes on.
    host.unload("first").await.unwrap();
    handlers_become(&bridge, &["h-"], vec![format!("h-{second}")]).await;
    let said = refused(
        host.load([Row::new("again", pid.display()).runtime("two")])
            .await,
    );
    assert!(
        said.ends_with(
            "was loaded by runtime `ui` this session, where the loader keeps it: not `two`"
        ),
        "{said}"
    );
    host.load([Row::new("again", pid.display()).runtime("ui")])
        .await
        .unwrap();
    let third = processes_of(dir, "pid", 2).await;
    let third = *third.iter().find(|p| **p != second).unwrap();
    assert_eq!(third, first, "back in `ui`'s process");
    handlers_become(
        &bridge,
        &["h-"],
        vec![format!("h-{second}"), format!("h-{third}")],
    )
    .await;

    // Given no service, a plugin never reaches `yoagent`, though the host
    // takes its row for running (`load` is `Ok`: it waits in the runtime's
    // own Cordis, where nobody provides it). The control, which injects
    // nothing, shows the runtime runs plugins.
    host.add_runtime("bare", plugins(), &[]).await.unwrap();
    let control = plain_plugin(dir, "control", "ran");
    host.load([
        Row::new("control", control.display()).runtime("bare"),
        Row::new("third", pid.display()).runtime("bare"),
    ])
    .await
    .unwrap();
    let bare = processes_of(dir, "control", 1).await[0];
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        !written_by(dir, "pid").iter().any(|(p, _)| *p == bare),
        "it reached `yoagent`"
    );

    // A row that declares a service it provides is refused.
    let provider = dir.join("provider.mjs");
    std::fs::write(
        &provider,
        format!(
            "import {{ definePlugin }} from 'file://{}'\n\
             export default definePlugin({{ provides: {{ thing: {{}} }}, apply(ctx) {{ ctx.provide('thing', {{ hi() {{ return 1 }} }}) }} }})\n",
            plugins().join("node_modules/@arcships/rutis/src/index.mjs").display()
        ),
    )
    .unwrap();
    let said = refused(
        host.load([Row::new("provider", provider.display()).runtime("loaded-a")])
            .await,
    );
    assert!(
        said.contains("it provides thing: a runtime added while running may provide no service"),
        "{said}"
    );

    // Removed: its plugin and its process gone, the others' still there.
    host.remove_runtime("loaded-a").await.unwrap();
    handlers_become(&bridge, &["h-"], vec![format!("h-{third}")]).await;
    #[cfg(unix)]
    {
        assert!(!alive(second), "removed, its process has ended");
        assert!(alive(third));
    }
    assert!(host.status().iter().all(|(id, _)| id != "second"));
    assert!(host.is_running("again"));
    assert_eq!(
        refused(host.remove_runtime("loaded-a").await),
        "no runtime `loaded-a` was added"
    );
    assert_eq!(
        refused(host.add_runtime("loaded-a", plugins(), &["yoagent"]).await),
        "a runtime `loaded-a` ran already: give another name"
    );
    assert_eq!(
        refused(host.remove_runtime("ui").await),
        "no runtime `ui` was added"
    );
    host.remove_runtime("bare").await.unwrap();
    #[cfg(unix)]
    assert!(!alive(bare));
    assert!(host
        .status()
        .iter()
        .all(|(id, _)| id != "third" && id != "control"));
    root.shutdown().await.unwrap();
}

/// A runtime that cannot start (no rutis runtime in its folder) is said
/// at `add_runtime`, with why, and is gone again.
#[tokio::test(flavor = "multi_thread")]
async fn an_added_runtime_that_cannot_start_is_said_and_gone() {
    if !runtime_installed() {
        return;
    }
    let empty = tempfile::tempdir().unwrap();
    std::fs::write(empty.path().join("package.json"), "{}").unwrap();
    let root = Ctx::root().unwrap();
    let mut host = PluginHost::builder()
        .node("ui", plugins())
        .share("yoagent")
        .start(&root)
        .await
        .unwrap();
    let started = tokio::time::Instant::now();
    let said = refused(host.add_runtime("broken", empty.path(), &[]).await);
    assert!(
        said.starts_with("runtime `broken` did not start: "),
        "{said}"
    );
    assert!(started.elapsed() < Duration::from_secs(25), "said at once");
    assert_eq!(
        refused(host.remove_runtime("broken").await),
        "no runtime `broken` was added"
    );
    root.shutdown().await.unwrap();
}
