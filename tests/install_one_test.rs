//! `install::install_one` per set (design §14 "Stage 2", point 7), against
//! the stub npm registry: a pi package with its peers that are not pi's
//! own, a DSH package from the DSH base's lock — whose locked packages
//! stay as locked under `--before` — and a pin the base holds at another
//! version refused. Real `npm install --before` and `npm ci`; no network.
//! Unix, as `load_plugin_test`.
#![cfg(unix)]

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use serde_json::{json, Value};
use yobuild::install::{install_one, NpmRun, Set};

use common::registry::{ago, pack, stub, Packages};

/// A `postinstall` that would leave `postinstall-ran` in the home: the
/// install must never run it.
const POSTINSTALL: &str =
    "node -e \"require('fs').writeFileSync(require('os').homedir()+'/postinstall-ran','')\"";

const DSH_FILES: [&str; 6] = [
    "dsh-tools-adapter.ts", "host-dialogs.ts", "probe-services.ts", "dsh-host.ts", "dsh-host.json",
    "host-services.json",
];

fn index() -> Vec<(&'static str, String)> {
    vec![("index.js", "export default 1;\n".to_owned())]
}

/// The version of `name` installed at the top of `dir`.
fn version_in(dir: &Path, name: &str) -> Option<String> {
    let text =
        std::fs::read_to_string(dir.join("node_modules").join(name).join("package.json")).ok()?;
    let manifest: Value = serde_json::from_str(&text).unwrap();
    manifest["version"].as_str().map(str::to_owned)
}

/// The packages: two that each have a version too new for the age rule.
fn packages() -> Packages {
    let lib = |name: &str, extra: Value| {
        (
            name.to_owned(),
            vec![
                pack(name, "1.0.0", 30, &index(), extra),
                pack(name, "1.1.0", 1, &index(), json!({})),
            ],
        )
    };
    Arc::new(BTreeMap::from([
        lib("peer-lib", json!({})),
        lib(
            "dep-lib",
            json!({ "scripts": { "postinstall": POSTINSTALL } }),
        ),
        (
            "pi-ext".to_owned(),
            vec![pack(
                "pi-ext",
                "1.0.0",
                60,
                &[("ext.js", "export default function () {}\n".to_owned())],
                json!({
                    "keywords": ["pi-package"],
                    "pi": { "extensions": ["./ext.js"] },
                    "dependencies": { "dep-lib": "^1.0.0" },
                    // pi's own is never fetched (the stub has none).
                    "peerDependencies": {
                        "@earendil-works/pi-coding-agent": "*",
                        "peer-lib": "^1.0.0"
                    }
                }),
            )],
        ),
        (
            "dsh-plug".to_owned(),
            vec![pack(
                "dsh-plug",
                "1.0.0",
                60,
                &index(),
                json!({ "keywords": ["dsh-plugin"], "dependencies": { "dep-lib": "^1.0.0" } }),
            )],
        ),
        // The DSH base's own, published yesterday: under `--before` it
        // could not be resolved anew, only kept as locked.
        (
            "base-lib".to_owned(),
            vec![pack("base-lib", "1.0.0", 1, &index(), json!({}))],
        ),
    ]))
}

/// An adapters folder whose DSH base pins `base-lib` 1.0.0, its lock
/// resolved against the stub.
fn adapters() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let dsh = dir.path().join("dsh");
    std::fs::create_dir(&dsh).unwrap();
    std::fs::write(
        dsh.join("package.json"),
        r#"{ "name": "dsh-base", "private": true, "type": "module", "dependencies": { "base-lib": "1.0.0" } }"#,
    )
    .unwrap();
    for name in DSH_FILES {
        std::fs::write(dsh.join(name), format!("// {name}\n")).unwrap();
    }
    let out = Command::new("npm")
        .args([
            "install", "--package-lock-only", "--ignore-scripts", "--no-audit", "--no-fund",
        ])
        .current_dir(&dsh)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "the base's lock: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    dir
}

#[test]
fn a_pi_and_a_dsh_package_install_as_their_sets_say() {
    let (port, fetched) = stub(packages(), &[]);
    // One test in this process: the registry and npm's home set once.
    let npm_home = tempfile::tempdir().unwrap();
    std::env::set_var("npm_config_registry", format!("http://127.0.0.1:{port}/"));
    std::env::set_var("HOME", npm_home.path());
    let home = tempfile::tempdir().unwrap();
    let before = ago(7);
    let how = NpmRun::default();

    // pi: its peer that is not pi's own installed, at the version old
    // enough, as is its dependency; pi's own left to the adapter.
    let (dir, scripts) = install_one(
        Some(home.path()),
        Set::Pi,
        Path::new(""),
        "pi-ext",
        "1.0.0",
        Some(&before),
        &how,
    )
    .unwrap();
    assert!(dir.starts_with(home.path().join("plugins/pi")), "{dir:?}");
    assert_eq!(version_in(&dir, "pi-ext").as_deref(), Some("1.0.0"));
    assert_eq!(version_in(&dir, "peer-lib").as_deref(), Some("1.0.0"));
    assert_eq!(version_in(&dir, "dep-lib").as_deref(), Some("1.0.0"));
    assert!(!dir.join("node_modules/@earendil-works").exists());
    assert_eq!(scripts, ["dep-lib"]);
    assert!(!npm_home.path().join("postinstall-ran").exists());
    assert!(!fetched
        .lock()
        .unwrap()
        .iter()
        .any(|name| name.starts_with("@earendil-works")));

    // dsh: the base's lock the start, its package kept as locked though
    // too new for `--before`; the pin's tree resolved by the age rule; the
    // adapter's files copied in.
    let adapters = adapters();
    let (dir, scripts) = install_one(
        Some(home.path()),
        Set::Dsh,
        adapters.path(),
        "dsh-plug",
        "1.0.0",
        Some(&before),
        &how,
    )
    .unwrap();
    assert!(dir.starts_with(home.path().join("plugins/dsh")), "{dir:?}");
    assert_eq!(version_in(&dir, "dsh-plug").as_deref(), Some("1.0.0"));
    assert_eq!(version_in(&dir, "dep-lib").as_deref(), Some("1.0.0"));
    assert_eq!(version_in(&dir, "base-lib").as_deref(), Some("1.0.0"));
    for name in DSH_FILES {
        assert!(dir.join(name).is_file(), "{name}");
    }
    let package: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("package.json")).unwrap()).unwrap();
    assert_eq!(package["type"], "module");
    assert_eq!(scripts, ["dep-lib"]);

    // A pin the base holds at another version: refused before npm.
    let asked = fetched.lock().unwrap().len();
    let refused = install_one(
        Some(home.path()),
        Set::Dsh,
        adapters.path(),
        "base-lib",
        "2.0.0",
        Some(&before),
        &how,
    )
    .unwrap_err();
    assert_eq!(
        refused.to_string(),
        "base-lib@2.0.0 is pinned by the DSH base at 1.0.0"
    );
    assert_eq!(fetched.lock().unwrap().len(), asked);
}
