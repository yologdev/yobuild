//! `install::install_one` per set (design §14 "Stage 2", point 7), against
//! the stub npm registry: a pi package with its peers that are not pi's
//! own, a DSH package from the DSH base's lock — whose locked packages
//! stay as locked under `--before`, a peer on one of them too, whose
//! install scripts are not counted as the pin's, and whose other peers are
//! added unless optional — the base's own package at its version, and a
//! pin the base holds at another version (or a base entry that is not a
//! version) refused before npm runs. Real `npm install --before` and
//! `npm ci`; no network. Unix, as `load_plugin_test`.
#![cfg(unix)]

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};

use serde_json::{json, Value};
use yobuild::install::{install_one, NpmRun, Set};

use common::registry::{ago, pack, stub, Packages, Stub};

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
    let script = json!({ "scripts": { "postinstall": POSTINSTALL } });
    Arc::new(BTreeMap::from([
        lib("peer-lib", json!({})),
        lib("dep-lib", script.clone()),
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
                    // pi's own: the stub has none, and npm is never asked.
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
                // A peer on the base's own, one the base lacks, and an
                // optional one (the stub has none: npm is never asked).
                json!({ "keywords": ["dsh-plugin"],
                        "dependencies": { "dep-lib": "^1.0.0" },
                        "peerDependencies": { "base-lib": "1.0.0", "peer-lib": "^1.0.0",
                                              "opt-lib": "*" },
                        "peerDependenciesMeta": { "opt-lib": { "optional": true } } }),
            )],
        ),
        // The DSH base's own, published yesterday, with an install script:
        // under `--before` it could not be resolved anew, only kept as
        // locked; its script is the base's, not a DSH plugin's.
        (
            "base-lib".to_owned(),
            vec![pack("base-lib", "1.0.0", 1, &index(), script)],
        ),
        // Another base's, only for the refusal: no other test asks for it.
        (
            "only-base".to_owned(),
            vec![pack("only-base", "1.0.0", 30, &index(), json!({}))],
        ),
    ]))
}

/// The stub, with npm pointed at it and given a home of its own: set once
/// a process, before any test runs npm.
struct Setup {
    stub: Stub,
    npm_home: PathBuf,
}

fn setup() -> &'static Setup {
    static SETUP: OnceLock<Setup> = OnceLock::new();
    SETUP.get_or_init(|| {
        let stub = stub(packages(), &[]);
        let npm_home = tempfile::tempdir().unwrap().keep();
        std::env::set_var(
            "npm_config_registry",
            format!("http://127.0.0.1:{}/", stub.port),
        );
        std::env::set_var("HOME", &npm_home);
        Setup { stub, npm_home }
    })
}

/// The version of `name` installed at the top of `dir`.
fn version_in(dir: &Path, name: &str) -> Option<String> {
    let text =
        std::fs::read_to_string(dir.join("node_modules").join(name).join("package.json")).ok()?;
    let manifest: Value = serde_json::from_str(&text).unwrap();
    manifest["version"].as_str().map(str::to_owned)
}

/// How often the stub was asked about `name`, either way.
fn asked_about(setup: &Setup, name: &str) -> usize {
    let tarballs = setup.stub.tarballs.lock().unwrap();
    let packuments = setup.stub.packuments.lock().unwrap();
    tarballs
        .iter()
        .chain(packuments.iter())
        .filter(|n| *n == name)
        .count()
}

/// An adapters folder whose DSH base has `dependencies`, the adapter's
/// files beside; with `lock`, its lock resolved against the stub (npm
/// with only the stub's registry and home: none of this shell's npm
/// settings).
fn adapters(dependencies: Value, lock: bool) -> tempfile::TempDir {
    let setup = setup();
    let dir = tempfile::tempdir().unwrap();
    let dsh = dir.path().join("dsh");
    std::fs::create_dir(&dsh).unwrap();
    let package = json!({ "name": "dsh-base", "private": true, "type": "module",
                          "dependencies": dependencies });
    std::fs::write(dsh.join("package.json"), package.to_string()).unwrap();
    for name in DSH_FILES {
        std::fs::write(dsh.join(name), format!("// {name}\n")).unwrap();
    }
    if lock {
        let out = Command::new("npm")
            .args(["install", "--package-lock-only", "--ignore-scripts"])
            .args(["--no-audit", "--no-fund"])
            .current_dir(&dsh)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", &setup.npm_home)
            .env(
                "npm_config_registry",
                format!("http://127.0.0.1:{}/", setup.stub.port),
            )
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "the base's lock: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    dir
}

#[test]
fn a_pi_package_brings_its_peers_not_pis_own() {
    let setup = setup();
    let home = tempfile::tempdir().unwrap();
    let (dir, scripts) = install_one(
        Some(home.path()),
        Set::Pi,
        Path::new(""),
        "pi-ext",
        "1.0.0",
        Some(&ago(7)),
        &NpmRun::default(),
    )
    .unwrap();
    assert!(dir.starts_with(home.path().join("plugins/pi")), "{dir:?}");
    assert_eq!(version_in(&dir, "pi-ext").as_deref(), Some("1.0.0"));
    // Its peer that is not pi's own, at the version old enough, as is its
    // dependency.
    assert_eq!(version_in(&dir, "peer-lib").as_deref(), Some("1.0.0"));
    assert_eq!(version_in(&dir, "dep-lib").as_deref(), Some("1.0.0"));
    // pi's own left to the adapter: npm never asked about it.
    assert!(!dir.join("node_modules/@earendil-works").exists());
    assert_eq!(asked_about(setup, "@earendil-works/pi-coding-agent"), 0);
    assert_eq!(scripts, ["dep-lib"]);
    assert!(!setup.npm_home.join("postinstall-ran").exists());
}

#[test]
fn a_dsh_package_installs_on_the_bases_lock() {
    let setup = setup();
    let home = tempfile::tempdir().unwrap();
    let adapters = adapters(json!({ "base-lib": "1.0.0" }), true);
    let before = ago(7);
    let (dir, scripts) = install_one(
        Some(home.path()),
        Set::Dsh,
        adapters.path(),
        "dsh-plug",
        "1.0.0",
        Some(&before),
        &NpmRun::default(),
    )
    .unwrap();
    assert!(dir.starts_with(home.path().join("plugins/dsh")), "{dir:?}");
    assert_eq!(version_in(&dir, "dsh-plug").as_deref(), Some("1.0.0"));
    // The pin's tree by the age rule; the base's own kept as locked though
    // too new for `--before`.
    assert_eq!(version_in(&dir, "dep-lib").as_deref(), Some("1.0.0"));
    assert_eq!(version_in(&dir, "base-lib").as_deref(), Some("1.0.0"));
    // Its peer the base lacks added, by the age rule; the optional one not.
    assert_eq!(version_in(&dir, "peer-lib").as_deref(), Some("1.0.0"));
    assert_eq!(asked_about(setup, "opt-lib"), 0);
    for name in DSH_FILES {
        assert_eq!(
            std::fs::read_to_string(dir.join(name)).unwrap(),
            format!("// {name}\n")
        );
    }
    let package: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("package.json")).unwrap()).unwrap();
    assert_eq!(package["type"], "module");
    // The pin peers on `base-lib`, whose script is the base's: not its.
    assert_eq!(scripts, ["dep-lib"]);
    assert!(!setup.npm_home.join("postinstall-ran").exists());

    // The base's own package at its version: taken, its script still the
    // base's.
    let (dir, scripts) = install_one(
        Some(home.path()),
        Set::Dsh,
        adapters.path(),
        "base-lib",
        "1.0.0",
        Some(&before),
        &NpmRun::default(),
    )
    .unwrap();
    assert_eq!(version_in(&dir, "base-lib").as_deref(), Some("1.0.0"));
    assert!(scripts.is_empty(), "{scripts:?}");
}

#[test]
fn a_pin_the_base_holds_otherwise_is_refused_before_npm_runs() {
    let setup = setup();
    let home = tempfile::tempdir().unwrap();
    let base = adapters(json!({ "only-base": "1.0.0" }), true);
    let asked = asked_about(setup, "only-base");
    let refused = install_one(
        Some(home.path()),
        Set::Dsh,
        base.path(),
        "only-base",
        "2.0.0",
        Some(&ago(7)),
        &NpmRun::default(),
    )
    .unwrap_err();
    assert_eq!(
        refused.to_string(),
        "only-base@2.0.0 is pinned by the DSH base at 1.0.0"
    );
    assert_eq!(asked_about(setup, "only-base"), asked);

    // A base entry that is not a version: an error, not "not pinned".
    let odd = adapters(json!({ "odd": 1 }), false);
    let refused = install_one(
        Some(home.path()),
        Set::Dsh,
        odd.path(),
        "odd",
        "1.0.0",
        None,
        &NpmRun::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(
        refused.ends_with("dsh/package.json: its dependency odd is 1, not a version"),
        "{refused}"
    );
    assert_eq!(asked_about(setup, "odd"), 0);
}
