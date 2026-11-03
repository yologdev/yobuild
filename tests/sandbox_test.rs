//! The sandbox (design §17), through the binary: with `[sandbox]`, a pi
//! extension, a DSH plugin, a rutis plugin and a stdio MCP server each
//! write in the agent's folder and in a folder `write` names, and are
//! refused writing anywhere else and reading what `hide` names — in `run`
//! and in `check`, which probes no pi or DSH plugin alone. Linux
//! (bubblewrap) and macOS (sandbox-exec); skipped, said, where the tool is
//! missing, unless `YOBUILD_REQUIRE_SANDBOX=1` (CI).
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/echo");
/// The tests' cache of installed packages, as `cli_test`'s.
const CACHE: &str = concat!(env!("CARGO_TARGET_TMPDIR"), "/yobuild-home");

/// This system's sandbox tool is here; else skipped (said), or a failure
/// with `YOBUILD_REQUIRE_SANDBOX=1`.
fn sandbox_here() -> bool {
    let found = match cfg!(target_os = "macos") {
        true => Path::new("/usr/bin/sandbox-exec").is_file(),
        false => Command::new("sh")
            .args(["-c", "command -v bwrap"])
            .output()
            .is_ok_and(|o| o.status.success()),
    };
    if found {
        return true;
    }
    if std::env::var_os("YOBUILD_REQUIRE_SANDBOX").is_some_and(|v| v == "1") {
        panic!("YOBUILD_REQUIRE_SANDBOX=1: no sandbox tool here (bwrap on Linux)");
    }
    eprintln!("SKIPPED: no sandbox tool here (install bubblewrap on Linux)");
    false
}

/// Whether `dir` is there; else skipped, or a failure with
/// `YOAGENT_RUTIS_REQUIRE_RUNTIMES=1`.
fn required(dir: &Path, setup: &str) -> bool {
    if dir.is_dir() {
        return true;
    }
    if std::env::var_os("YOAGENT_RUTIS_REQUIRE_RUNTIMES").is_some_and(|v| v == "1") {
        panic!("{} is required: {setup}", dir.display());
    }
    eprintln!("SKIPPED: {setup}");
    false
}

/// What each plugin and the server found, one JSON line per load: writing
/// in the agent's folder, in the folder `write` names and outside both, and
/// reading the hidden file.
const SEEN: [&str; 4] = ["inside", "allowed", "outside", "secret"];

#[test]
fn plugins_and_servers_write_only_where_the_sandbox_lets_them() {
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rutis = checkout.join("frontend/plugins/node_modules");
    if !sandbox_here()
        || !required(
            &checkout.join("adapters/pi/node_modules"),
            "scripts/copy-adapters.sh, then npm ci in adapters/pi",
        )
        || !required(
            &checkout.join("adapters/dsh/node_modules"),
            "scripts/copy-adapters.sh, then npm ci in adapters/dsh",
        )
        || !required(
            &rutis.join("@arcships/rutis-runtime"),
            "npm ci in frontend/plugins",
        )
    {
        return;
    }
    // Not under the system's temp folder, which the sandbox lets them
    // write: yobuild is given a temp folder of its own beside the others.
    let base = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let base: PathBuf = std::fs::canonicalize(base.path()).unwrap();
    let [project, tmp, allowed, outside, secret] =
        ["project", "tmp", "allowed", "outside", "secret"].map(|f| base.join(f));
    for folder in [&project, &tmp, &allowed, &outside, &secret] {
        std::fs::create_dir(folder).unwrap();
    }
    std::fs::write(secret.join("key.txt"), "sk-secret\n").unwrap();
    let at = |p: &Path| p.display().to_string();
    // Each plugin's own lines, by its name: what writing and reading gave.
    let probe = |name: &str| {
        format!(
            "import {{ appendFileSync, writeFileSync, readFileSync }} from 'node:fs'\n\
             const write = (p) => {{ try {{ writeFileSync(p, 'x'); return 'written' }} catch (e) {{ return e.code }} }}\n\
             const read = (p) => {{ try {{ readFileSync(p, 'utf8'); return 'read' }} catch (e) {{ return e.code }} }}\n\
             const seen = () => appendFileSync({seen:?}, JSON.stringify([write({inside:?}), write({allowed:?}), write({outside:?}), read({secret:?})]) + '\\n')\n",
            seen = at(&project.join(format!("{name}.seen"))),
            inside = at(&project.join(format!("{name}.in"))),
            allowed = at(&allowed.join(name)),
            outside = at(&outside.join(name)),
            secret = at(&secret.join("key.txt")),
        )
    };
    std::fs::write(
        project.join("pi.ts"),
        format!("{}export default function () {{ seen() }}\n", probe("pi")),
    )
    .unwrap();
    std::fs::write(
        project.join("dsh.ts"),
        format!(
            "{}export const name = 'dsh'\nexport function apply() {{ seen() }}\n",
            probe("dsh")
        ),
    )
    .unwrap();
    let r = project.join("r");
    std::fs::create_dir(&r).unwrap();
    std::os::unix::fs::symlink(&rutis, r.join("node_modules")).unwrap();
    std::fs::write(r.join("package.json"), r#"{ "type": "module" }"#).unwrap();
    std::fs::write(
        r.join("plug.ts"),
        format!(
            "{}import {{ definePlugin }} from '@arcships/rutis'\n\
             export default definePlugin({{ apply() {{ seen() }} }})\n",
            probe("rutis")
        ),
    )
    .unwrap();
    let server = project.join("server.sh");
    std::fs::write(
        &server,
        format!(
            "#!/usr/bin/env bash\n\
             w() {{ (echo x > \"$1\") 2>/dev/null && echo written || echo refused; }}\n\
             r() {{ cat \"$1\" >/dev/null 2>&1 && echo read || echo refused; }}\n\
             printf '[\"%s\",\"%s\",\"%s\",\"%s\"]\\n' \"$(w {inside:?})\" \"$(w {allowed:?})\" \"$(w {outside:?})\" \"$(r {secret:?})\" >> {seen:?}\n\
             exec bash {FIXTURE}/server.sh\n",
            inside = at(&project.join("mcp.in")),
            allowed = at(&allowed.join("mcp")),
            outside = at(&outside.join("mcp")),
            secret = at(&secret.join("key.txt")),
            seen = at(&project.join("mcp.seen")),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&server, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    std::fs::write(
        project.join("agent.toml"),
        "[agent]\nname = \"a\"\nmodel = \"deepseek/deepseek-flash\"\n\
         [[plugin]]\npi = \"./pi.ts\"\n\
         [[plugin]]\ndsh = \"./dsh.ts\"\n\
         [[plugin]]\nname = \"plug\"\nrutis = \"./r/plug.ts\"\nruntime = \"node\"\n\
         [[mcp]]\nname = \"server\"\nnpm = \"./server.sh\"\n\
         [sandbox]\nwrite = [\"../allowed\"]\nhide = [\"../secret\"]\n",
    )
    .unwrap();
    std::fs::write(project.join("script.json"), r#"[{ "text": "done" }]"#).unwrap();
    let yobuild = |args: &[&str]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_yobuild"));
        for key in yobuild::manifest::KEY_VARIABLES {
            command.env_remove(key);
        }
        command
            .env("YOBUILD_HOME", CACHE)
            .env("TMPDIR", &tmp)
            .env_remove("RUST_LOG")
            .args(args)
            .current_dir(&project)
            .output()
            .unwrap()
    };
    // Each entry's lines, the file removed for the next command: written
    // inside and where `write` says, refused elsewhere (a read-only file
    // system, or the sandbox's refusal), the hidden file not read (not
    // there in bubblewrap's empty folder, refused by sandbox-exec).
    let lines = |name: &str, said: &str| -> usize {
        let file = project.join(format!("{name}.seen"));
        let read = std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{name}: {e}\n{said}"));
        std::fs::remove_file(&file).unwrap();
        for line in read.lines() {
            let seen: Vec<String> = serde_json::from_str(line).unwrap();
            let [inside, allowed, outside, secret] = <[String; 4]>::try_from(seen).unwrap();
            assert_eq!(
                (inside.as_str(), allowed.as_str()),
                ("written", "written"),
                "{name}: {line}\n{said}"
            );
            assert!(
                outside != "written" && secret != "read",
                "{name} {SEEN:?}: {line}\n{said}"
            );
        }
        read.lines().count()
    };
    let out = yobuild(&[
        "run", "agent.toml", "--scripted", "script.json", "--once", "go",
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{stderr}");
    for name in ["pi", "dsh", "rutis", "mcp"] {
        assert_eq!(lines(name, &stderr), 1, "{name}");
    }
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0, "{stderr}");
    let tool = match cfg!(target_os = "macos") {
        true => "sandbox-exec",
        false => "bwrap",
    };
    let note = format!("  Info: the sandbox ({tool}) confines every plugin runtime and stdio MCP server: they write only to ");
    assert_eq!(stderr.matches(&note).count(), 1, "{stderr}");

    // `check`: each entry confined, the pi and DSH plugins checked together
    // only (their probes alone would run outside the sandbox).
    let out = yobuild(&["check", "agent.toml", "--json"]);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let report: serde_json::Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {stderr}"));
    assert_eq!(report["ok"], true, "{report}");
    assert_eq!(report["agent"]["sandbox"]["tool"], tool, "{report}");
    let write = report["agent"]["sandbox"]["write"].as_array().unwrap();
    assert!(write.contains(&serde_json::json!(at(&project))), "{report}");
    assert!(write.contains(&serde_json::json!(at(&allowed))), "{report}");
    assert!(
        !write.contains(&serde_json::json!(at(&outside))),
        "{report}"
    );
    assert!(report["agent"]["sandbox"]["hide"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!(at(&secret))));
    for entry in report["entries"].as_array().unwrap() {
        assert_eq!(entry["confined"], true, "{entry}");
        let alone = !matches!(entry["ecosystem"].as_str(), Some("pi" | "dsh"));
        assert_eq!(entry["checkedAlone"], alone, "{entry}");
    }
    // Loaded together; the rutis plugin alone too.
    for name in ["pi", "dsh", "mcp"] {
        assert!(lines(name, &stderr) >= 1, "{name}");
    }
    assert_eq!(lines("rutis", &stderr), 2);
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);

    // What would undo it is refused before anything starts.
    std::fs::write(
        project.join("agent.toml"),
        "[agent]\nname = \"a\"\nmodel = \"deepseek/deepseek-flash\"\n[sandbox]\nhide = [\"./\"]\n",
    )
    .unwrap();
    let out = yobuild(&[
        "run", "agent.toml", "--scripted", "script.json", "--once", "go",
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains(&format!(
            "[sandbox] hide {}: it holds the agent's folder: hide what is inside it instead",
            at(&project)
        )),
        "{stderr}"
    );
}
