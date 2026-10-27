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
use yobuild::frontend::host::{PluginHost, Row};

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

/// A runtime added while the host runs (design §14 "Stage 2"): it loads a
/// file another runtime loads too, starts with the environment every
/// runtime gets (nothing kept for it), reaches only the services it was
/// given, and is removed whole — its plugins and its process — leaving the
/// other runtime's.
#[tokio::test(flavor = "multi_thread")]
async fn a_runtime_added_while_running_loads_its_own_rows_and_is_removed_whole() {
    if !runtime_installed() {
        return;
    }
    const NAME: &str = "CARGO_MANIFEST_DIR";
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), r#"{ "type": "module" }"#).unwrap();
    // Registers a handler named after its process, and writes what it got
    // to `started-<pid>.json`.
    let file = dir.path().join("pid.mjs");
    let rutis = plugins().join("node_modules/@arcships/rutis/src/index.mjs");
    std::fs::write(
        &file,
        format!(
            r#"import {{ writeFileSync }} from 'node:fs'
import {{ definePlugin }} from 'file://{rutis}'
export default definePlugin({{
  inject: ['yoagent'],
  apply(ctx) {{
    ctx.effect(ctx.use('yoagent').register('h-' + process.pid, {{
      async tools() {{ return [] }},
      async call_tool() {{ return {{ text: '' }} }},
    }}))
    writeFileSync({out:?} + '/started-' + process.pid + '.json',
      JSON.stringify({{ pid: process.pid, env: process.env.{NAME} ?? null }}))
  }},
}})
"#,
            rutis = rutis.display(),
            out = dir.path().display().to_string(),
        ),
    )
    .unwrap();
    let started = || -> Vec<serde_json::Value> {
        std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| {
                let path = e.unwrap().path();
                let name = path.file_name()?.to_str()?.to_owned();
                name.starts_with("started-")
                    .then(|| serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok())?
            })
            .collect()
    };

    let root = Ctx::root().unwrap();
    let bridge = RutisBridge::install(&root).unwrap();
    let mut host = PluginHost::builder()
        .node("ui", plugins())
        .share("yoagent")
        .withhold(NAME)
        .keep("ui", NAME)
        .start(&root)
        .await
        .unwrap();
    host.load([Row::new("first", file.display()).runtime("ui")])
        .await
        .unwrap();

    let refused = |r: Result<(), _>| {
        r.map_err(|e: Box<dyn std::error::Error + Send + Sync>| e.to_string())
            .unwrap_err()
    };
    assert_eq!(
        refused(host.add_runtime("ui", plugins(), &["yoagent"])),
        "a runtime `ui` runs already"
    );
    assert!(refused(host.add_runtime("Loaded A", plugins(), &[])).contains("lowercase"));
    host.add_runtime("loaded-a", plugins(), &["yoagent"])
        .unwrap();
    assert_eq!(
        refused(host.add_runtime("loaded-a", plugins(), &[])),
        "a runtime `loaded-a` runs already"
    );
    // The same file, loaded again in the runtime added.
    host.load([Row::new("second", file.display()).runtime("loaded-a")])
        .await
        .unwrap();
    let two = tokio::time::timeout(WAIT, async {
        loop {
            let found = started();
            if found.len() == 2 {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("both started");
    let pid = |kept: bool| {
        two.iter()
            .find(|s| s["env"].is_string() == kept)
            .unwrap_or_else(|| panic!("{two:?}"))["pid"]
            .as_u64()
            .unwrap()
    };
    // `ui` keeps the withheld name; the added runtime keeps nothing.
    let (first, second) = (pid(true), pid(false));
    assert_ne!(first, second);
    let handlers = || -> Vec<String> {
        let mut names: Vec<String> = bridge
            .registry()
            .handlers()
            .iter()
            .map(|h| h.name().to_owned())
            .filter(|n| n.starts_with("h-"))
            .collect();
        names.sort();
        names
    };
    let mut both = vec![format!("h-{first}"), format!("h-{second}")];
    both.sort();
    tokio::time::timeout(WAIT, async {
        while handlers() != both {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("both registered");

    // Given no service, the same plugin never reaches `yoagent`.
    host.add_runtime("bare", plugins(), &[]).unwrap();
    let bare = tokio::time::timeout(
        Duration::from_secs(10),
        host.load([Row::new("third", file.display()).runtime("bare")]),
    )
    .await;
    // The host takes the row for running (`load` is `Ok`): it waits in
    // the runtime's own Cordis, for a `yoagent` nobody provides there. A
    // caller learns that it started from what it registers.
    assert!(matches!(bare, Ok(Ok(()))), "{bare:?}");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(started().len(), 2);
    assert_eq!(handlers(), both);

    // Removed: its plugin and its process gone, `ui`'s still there.
    host.remove_runtime("loaded-a").await.unwrap();
    tokio::time::timeout(WAIT, async {
        while handlers() != [format!("h-{first}")] {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("its handler went");
    #[cfg(unix)]
    tokio::time::timeout(WAIT, async {
        let alive = |pid: u64| {
            std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success()
        };
        while alive(second) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(alive(first));
    })
    .await
    .expect("its process ended");
    assert!(host.status().iter().all(|(id, _)| id != "second"));
    assert_eq!(
        refused(host.remove_runtime("loaded-a").await),
        "no runtime `loaded-a` was added"
    );
    assert!(host.is_running("first"));
    host.remove_runtime("bare").await.unwrap();
    root.shutdown().await.unwrap();
}
