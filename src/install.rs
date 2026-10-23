//! A manifest's pinned packages, installed (design §4): pi plugins, DSH
//! plugins and stdio MCP servers, each ecosystem's an npm project of its
//! own (a *set*), resolved into the lock beside the manifest (`agent.toml`
//! → `agent.lock`) and installed with `npm ci` into a cache folder named
//! after the set's hash, `<home>/plugins/<set>/<hash>/` (`home`:
//! `YOBUILD_HOME`, else `~/.yobuild`), shared by every agent whose lock
//! holds the same set. Never a package script; npm's settings that decide
//! a result passed on its command line. Unix only.
//!
//! - `pi`: the pi packages and their peers that are not pi's own
//!   (`legacy-peer-deps`: the pi adapter supplies `@earendil-works/pi-*`
//!   and `typebox`).
//! - `dsh`: the DSH adapter's base (`adapters/dsh`'s `package.json` and
//!   lock) plus the DSH packages, so they resolve the base's Cordis; the
//!   `dsh` runtime starts there, with the adapter's files it loads copied
//!   in.
//! - `mcp`: the stdio servers.
//!
//! What is not installed: local files (rutis plugins, pi and DSH files),
//! the adapters, HTTP servers.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value as Json};
use sha2::{Digest, Sha256};

use crate::manifest::{Manifest, PluginKind, Source, Transport};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The file an installed set's folder holds, last: its hash.
const STAMP: &str = ".yobuild-installed";
/// The adapter files the `dsh` set holds at its root, where they resolve
/// the set's Cordis: the runtime loads them from there. The DSH host's
/// (`dsh-host.ts`) and the row lists `dsh_host_rows` reads beside it
/// (`dsh-host.json`, `host-services.json`) among them.
const DSH_FILES: [&str; 6] = [
    "dsh-tools-adapter.ts", "host-dialogs.ts", "probe-services.ts", "dsh-host.ts", "dsh-host.json",
    "host-services.json",
];
/// How long one npm call may take.
const NPM_TIME: Duration = Duration::from_secs(600);
/// A `.partial-*` folder older than this was left by an install that died.
const PARTIAL_AGE: Duration = Duration::from_secs(3600);

/// A manifest file's lock: beside it, named after it (`agent.toml` →
/// `agent.lock`, `a.local.toml` → `a.local.lock`), so manifests sharing a
/// folder keep their own.
fn lock_beside(manifest_file: &Path) -> PathBuf {
    manifest_file.with_extension("lock")
}

/// A manifest's lock, in its (absolute) folder.
fn lock_path(manifest: &Manifest) -> PathBuf {
    let name = manifest.path.file_name().unwrap_or_default();
    lock_beside(&manifest.dir.join(name))
}

/// The cache's home: `YOBUILD_HOME`, else `~/.yobuild`.
fn home() -> Result<PathBuf, BoxError> {
    match std::env::var_os("YOBUILD_HOME").filter(|v| !v.is_empty()) {
        Some(home) => Ok(PathBuf::from(home)),
        None => match std::env::var_os("HOME").filter(|v| !v.is_empty()) {
            Some(home) => Ok(Path::new(&home).join(".yobuild")),
            None => Err("neither YOBUILD_HOME nor HOME is set: where would packages go?".into()),
        },
    }
}

/// One ecosystem's npm project. Ordered as a lock lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Set {
    Dsh,
    Mcp,
    Pi,
}

impl Set {
    pub fn name(self) -> &'static str {
        match self {
            Set::Pi => "pi",
            Set::Dsh => "dsh",
            Set::Mcp => "mcp",
        }
    }
}

/// A lock: per set, what the manifest pinned and npm's own lock.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Lock {
    lock_version: u32,
    sets: BTreeMap<Set, SetLock>,
}

/// One set in a lock.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetLock {
    /// For `dsh`: the checksum of the DSH adapter's `package.json` and
    /// lock, the base the set was resolved over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base: Option<String>,
    /// The manifest's pins in this set, name → version.
    requested: BTreeMap<String, String>,
    /// npm's lock (`package-lock.json` v3's `packages`), as npm wrote it.
    packages: Map<String, Json>,
}

impl Lock {
    const VERSION: u32 = 1;

    fn empty() -> Lock {
        Lock {
            lock_version: Lock::VERSION,
            sets: BTreeMap::new(),
        }
    }

    /// The lock at `path`, if there is one. Its version is read first (a
    /// later version may have another shape), then the rest strictly.
    fn read(path: &Path) -> Result<Option<Lock>, BoxError> {
        let failed = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(failed(&e).into()),
        };
        let value: Json = serde_json::from_str(&text).map_err(|e| failed(&e))?;
        match value["lockVersion"].as_u64() {
            Some(v) if v == u64::from(Lock::VERSION) => {}
            Some(v) => {
                return Err(failed(&format!(
                    "lockVersion {v}, which this yobuild does not read (it writes {})",
                    Lock::VERSION
                ))
                .into())
            }
            None => return Err(failed(&"no lockVersion: not a yobuild lock").into()),
        }
        Ok(Some(serde_json::from_value(value).map_err(|e| failed(&e))?))
    }

    /// Written whole: to a temp file beside it, then renamed into place, so
    /// two writers leave one version, not a mix. No sets: the file goes.
    fn write(&self, path: &Path) -> Result<(), BoxError> {
        let failed = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
        if self.sets.is_empty() {
            return match std::fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(failed(&e).into()),
                _ => Ok(()),
            };
        }
        let dir = path.parent().unwrap_or(Path::new("."));
        let mut text = serde_json::to_string_pretty(self)?;
        text.push('\n');
        let temp = tempfile::NamedTempFile::new_in(dir).map_err(|e| failed(&e))?;
        std::fs::write(temp.path(), text).map_err(|e| failed(&e))?;
        temp.persist(path).map_err(|e| failed(&e.error))?;
        Ok(())
    }
}

/// Where each set is installed, and what was said about the lock.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct Installed {
    dirs: BTreeMap<Set, PathBuf>,
    said: Vec<String>,
}

impl Installed {
    /// The folder `set` is installed in, if the manifest pins any of it.
    pub fn get(&self, set: Set) -> Option<&Path> {
        self.dirs.get(&set).map(PathBuf::as_path)
    }

    /// What changed in the lock (an Info notice each).
    pub fn said(&self) -> &[String] {
        &self.said
    }

    /// For tests: `set` said to be installed in `dir`.
    #[cfg(test)]
    pub(crate) fn with(set: Set, dir: &Path) -> Installed {
        Installed {
            dirs: [(set, dir.to_path_buf())].into(),
            said: Vec::new(),
        }
    }
}

/// The manifest's pins, by set; a refusal names the entry.
fn pins(
    manifest: &Manifest,
    adapters: &Path,
) -> Result<BTreeMap<Set, BTreeMap<String, String>>, BoxError> {
    let mut pins: BTreeMap<Set, BTreeMap<String, String>> = BTreeMap::new();
    let mut dsh_file: Option<&str> = None;
    let mut dsh_package: Option<&str> = None;
    let mut base: Option<Map<String, Json>> = None;
    for plugin in &manifest.plugins {
        let (set, source) = match &plugin.kind {
            PluginKind::Pi(source) => (Set::Pi, source),
            PluginKind::Dsh(source) => (Set::Dsh, source),
            PluginKind::Rutis { .. } => continue,
        };
        let package = match source {
            Source::Package(package) => package,
            Source::Path(_) => {
                if set == Set::Dsh {
                    dsh_file.get_or_insert(&plugin.name);
                }
                continue;
            }
            // yobuild's own: nothing to install.
            Source::Builtin(_) => continue,
        };
        if set == Set::Dsh {
            dsh_package.get_or_insert(&plugin.name);
            let base = match &mut base {
                Some(base) => base,
                None => base.insert(base_dependencies(adapters)?),
            };
            if let Some(at) = base.get(package.name()).and_then(Json::as_str) {
                if at != package.version() {
                    return Err(format!(
                        "plugin {}: {} is pinned by the DSH base at {at}",
                        plugin.name,
                        package.spec()
                    )
                    .into());
                }
            }
        }
        pins.entry(set)
            .or_default()
            .insert(package.name().to_owned(), package.version().to_owned());
    }
    if let (Some(file), Some(package)) = (dsh_file, dsh_package) {
        return Err(format!(
            "DSH files and DSH packages cannot run together yet (plugin {file}, a file; plugin {package}, a package): the packages run in the cache, the files in adapters/dsh"
        )
        .into());
    }
    for server in &manifest.mcp {
        if let Transport::Stdio {
            source: Source::Package(package),
            ..
        } = &server.transport
        {
            pins.entry(Set::Mcp)
                .or_default()
                .insert(package.name().to_owned(), package.version().to_owned());
        }
    }
    Ok(pins)
}

/// The DSH adapter's `package.json`.
fn dsh_package_json(adapters: &Path) -> Result<Map<String, Json>, BoxError> {
    let path = adapters.join("dsh/package.json");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    match serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))? {
        Json::Object(fields) => Ok(fields),
        _ => Err(format!("{}: not an object", path.display()).into()),
    }
}

/// What the DSH base pins: its `package.json`'s dependencies.
fn base_dependencies(adapters: &Path) -> Result<Map<String, Json>, BoxError> {
    match dsh_package_json(adapters)?.remove("dependencies") {
        Some(Json::Object(deps)) => Ok(deps),
        None => Ok(Map::new()),
        Some(_) => Err(format!(
            "{}: dependencies is not an object",
            adapters.join("dsh/package.json").display()
        )
        .into()),
    }
}

fn sha256_hex(parts: &[&[u8]]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        // Each part's length first: no two lists of parts hash alike.
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn read(path: &Path) -> Result<Vec<u8>, BoxError> {
    std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()).into())
}

fn write(path: &Path, contents: impl AsRef<[u8]>) -> Result<(), BoxError> {
    std::fs::write(path, contents).map_err(|e| format!("{}: {e}", path.display()).into())
}

/// The DSH base's checksum: its `package.json` and lock.
fn base_checksum(adapters: &Path) -> Result<String, BoxError> {
    let dsh = adapters.join("dsh");
    Ok(sha256_hex(&[
        &read(&dsh.join("package.json"))?,
        &read(&dsh.join("package-lock.json"))?,
    ]))
}

/// One canonical text of a JSON value: compact, object keys sorted by code
/// point at every level (this crate's `serde_json` keeps insertion order,
/// so sorting is explicit).
fn canonical(value: &Json) -> String {
    match value {
        Json::Object(fields) => {
            let mut keys: Vec<&String> = fields.keys().collect();
            keys.sort();
            let inner: Vec<String> = keys
                .into_iter()
                .map(|k| format!("{}:{}", Json::String(k.clone()), canonical(&fields[k])))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        Json::Array(items) => {
            let inner: Vec<String> = items.iter().map(canonical).collect();
            format!("[{}]", inner.join(","))
        }
        other => other.to_string(),
    }
}

/// A set's folder name: the first 16 hex digits of a sha256 over the set's
/// name, its lock entry's `base` and `packages` (canonical) and, for `dsh`,
/// the adapter files it copies.
fn hash(set: Set, entry: &SetLock, adapters: &Path) -> Result<String, BoxError> {
    // What decides the folder's content: npm's packages and the base, not
    // which of them the manifest asked for (two agents pinning the DSH
    // base's own packages share one folder).
    let entry = canonical(&json!({ "base": entry.base, "packages": entry.packages }));
    let files = match set {
        Set::Dsh => {
            let dsh = adapters.join("dsh");
            let mut parts = Vec::new();
            for name in DSH_FILES {
                parts.push(name.as_bytes().to_vec());
                parts.push(read(&dsh.join(name))?);
            }
            let parts: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
            sha256_hex(&parts)
        }
        _ => String::new(),
    };
    let mut hex = sha256_hex(&[set.name().as_bytes(), entry.as_bytes(), files.as_bytes()]);
    hex.truncate(16);
    Ok(hex)
}

/// The settings that decide npm's result, on its command line (they win
/// over every config file and variable), and its output's. Not `--omit=`:
/// npm warns that it is invalid; `include` wins over an `omit` set
/// anywhere.
fn npm_flags(set: Set) -> Vec<String> {
    let legacy = set == Set::Pi;
    vec![
        "--ignore-scripts".into(),
        format!("--legacy-peer-deps={legacy}"),
        "--package-lock=true".into(),
        "--install-strategy=hoisted".into(),
        "--strict-peer-deps=false".into(),
        "--install-links=false".into(),
        "--include=dev".into(),
        "--include=optional".into(),
        "--include=peer".into(),
        "--no-audit".into(),
        "--no-fund".into(),
        "--loglevel=error".into(),
    ]
}

/// The deciding settings again, for anyone running npm in the folder by
/// hand.
fn npmrc(set: Set) -> String {
    format!(
        "# Written by yobuild: the settings it installs this set with.\n\
         ignore-scripts=true\n\
         legacy-peer-deps={}\n\
         package-lock=true\n\
         install-strategy=hoisted\n\
         strict-peer-deps=false\n\
         install-links=false\n\
         include[]=dev\n\
         include[]=optional\n\
         include[]=peer\n",
        set == Set::Pi
    )
}

/// An npm variable yobuild keeps: the registry and its login. Every other
/// `npm_config_*` (npm reads them without regard to case) is dropped, so it
/// cannot change a result.
fn npm_variable_kept(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    match lower.strip_prefix("npm_config_") {
        None => true,
        Some(rest) => rest == "registry" || rest.ends_with(":registry") || rest.starts_with("//"),
    }
}

/// Runs npm in `dir`; its stdout. A failure carries npm's message
/// (`npm_error`).
fn npm(dir: &Path, args: &[String]) -> Result<String, BoxError> {
    let failed = |e: &dyn std::fmt::Display| format!("npm {} in {}: {e}", args[0], dir.display());
    let mut command = std::process::Command::new("npm");
    command
        .args(args)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (key, _) in std::env::vars_os() {
        if !npm_variable_kept(&key.to_string_lossy()) {
            command.env_remove(&key);
        }
    }
    let mut child = command.spawn().map_err(|e| failed(&e))?;
    let drain = |mut from: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            from.read_to_end(&mut bytes).map(|_| bytes)
        })
    };
    let stdout = drain(Box::new(child.stdout.take().expect("piped")));
    let stderr = drain(Box::new(child.stderr.take().expect("piped")));
    let deadline = Instant::now() + NPM_TIME;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| failed(&e))? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(failed(&format!("not done in {} s", NPM_TIME.as_secs())).into());
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let output = |thread: std::thread::JoinHandle<std::io::Result<Vec<u8>>>, which: &str| {
        match thread.join() {
            Ok(Ok(bytes)) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
            Ok(Err(e)) => Err(failed(&format!("reading its {which}: {e}"))),
            Err(_) => Err(failed(&format!("reading its {which} failed"))),
        }
    };
    let stdout = output(stdout, "output")?;
    let stderr = output(stderr, "errors")?;
    if !status.success() {
        return Err(failed(&npm_error(&stderr)).into());
    }
    if !stderr.trim().is_empty() {
        tracing::debug!("npm {} in {}: {}", args[0], dir.display(), stderr.trim());
    }
    Ok(stdout)
}

/// What npm said went wrong: its message up to the usage text it prints
/// after a misuse (`npm error Usage:` and the option list), at most 20
/// lines, then the line naming npm's own log, which holds the rest.
fn npm_error(stderr: &str) -> String {
    let lines: Vec<&str> = stderr.trim().lines().collect();
    let usage = lines
        .iter()
        .position(|l| l.trim_start_matches("npm error").trim() == "Usage:")
        .unwrap_or(lines.len());
    let mut said: Vec<&str> = lines[..usage].iter().take(20).copied().collect();
    if let Some(log) = lines.iter().rev().find(|l| l.contains("complete log")) {
        if !said.contains(log) {
            said.push(log);
        }
    }
    said.join("\n")
}

/// pi's own packages, which the adapter supplies: never installed in the
/// `pi` set.
fn pis_own(name: &str) -> bool {
    name.starts_with("@earendil-works/pi-") || name == "typebox"
}

/// Two semver ranges intersected: each pair of their `||` alternatives
/// joined by a space (a space binds tighter than `||`).
fn intersect(a: &str, b: &str) -> String {
    let alternatives =
        |r: &str| -> Vec<String> { r.split("||").map(|alt| alt.trim().to_owned()).collect() };
    let mut out = Vec::new();
    for x in alternatives(a) {
        for y in alternatives(b) {
            let joined = format!("{x} {y}");
            let joined = joined.trim().to_owned();
            if !out.contains(&joined) {
                out.push(joined);
            }
        }
    }
    out.join(" || ")
}

/// The `pi` set's dependencies: the pins, and each pin's peers that are not
/// pi's own (a pinned peer keeps its pin; two ranges intersected).
fn pi_dependencies(
    dir: &Path,
    pins: &BTreeMap<String, String>,
) -> Result<Map<String, Json>, BoxError> {
    let mut peers: BTreeMap<String, String> = BTreeMap::new();
    for (name, version) in pins {
        let spec = format!("{name}@{version}");
        let view = npm(dir, &["view".into(), spec.clone(), "--json".into()])?;
        // npm says nothing, and succeeds, for a version it does not have.
        if view.trim().is_empty() {
            return Err(
                format!("{spec} is not on the registry (npm view found no such version)").into(),
            );
        }
        let view: Json =
            serde_json::from_str(&view).map_err(|e| format!("npm view {spec}: {e}"))?;
        peers_of(&spec, &view, pins, &mut peers)?;
    }
    let mut deps = Map::new();
    for (name, version) in pins.iter().chain(peers.iter()) {
        deps.insert(name.clone(), json!(version));
    }
    Ok(deps)
}

/// Adds to `peers` the peers `view` (`npm view <spec> --json`) declares
/// that are neither pi's own nor pinned, a range already there intersected.
fn peers_of(
    spec: &str,
    view: &Json,
    pins: &BTreeMap<String, String>,
    peers: &mut BTreeMap<String, String>,
) -> Result<(), BoxError> {
    let declared = match &view["peerDependencies"] {
        Json::Null => return Ok(()),
        Json::Object(declared) => declared,
        other => return Err(format!("npm view {spec}: peerDependencies is {other}").into()),
    };
    for (peer, range) in declared {
        let range = range
            .as_str()
            .ok_or_else(|| format!("npm view {spec}: peer {peer}'s range is {range}"))?;
        if pis_own(peer) || pins.contains_key(peer) {
            continue;
        }
        let joined = match peers.get(peer) {
            Some(other) => intersect(other, range),
            None => range.to_owned(),
        };
        peers.insert(peer.clone(), joined);
    }
    Ok(())
}

/// A set's `package.json`: fixed fields (for `dsh`, the adapter's: its
/// `"type": "module"` among them) and `dependencies`.
fn package_json(
    set: Set,
    adapters: &Path,
    dependencies: Map<String, Json>,
) -> Result<Json, BoxError> {
    let mut fields = match set {
        Set::Dsh => dsh_package_json(adapters)?,
        _ => {
            let mut fields = Map::new();
            fields.insert("name".into(), json!(format!("yobuild-{}", set.name())));
            fields.insert("private".into(), json!(true));
            fields.insert("type".into(), json!("module"));
            fields
        }
    };
    fields.insert("dependencies".into(), Json::Object(dependencies));
    Ok(Json::Object(fields))
}

fn write_json(path: &Path, value: &Json) -> Result<(), BoxError> {
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    write(path, text)
}

/// Resolves a set anew in a temp folder: its lock entry (nothing unpacked).
fn resolve(
    set: Set,
    pins: &BTreeMap<String, String>,
    adapters: &Path,
) -> Result<SetLock, BoxError> {
    let dir = tempfile::tempdir().map_err(|e| format!("a temp folder to resolve in: {e}"))?;
    write(&dir.path().join(".npmrc"), npmrc(set))?;
    let (dependencies, base) = match set {
        Set::Pi => (pi_dependencies(dir.path(), pins)?, None),
        Set::Mcp => (
            pins.iter().map(|(n, v)| (n.clone(), json!(v))).collect(),
            None,
        ),
        Set::Dsh => {
            // The base's lock as the start: DSH's packages stay as pinned.
            let lock = adapters.join("dsh/package-lock.json");
            std::fs::copy(&lock, dir.path().join("package-lock.json"))
                .map_err(|e| format!("{}: {e}", lock.display()))?;
            let mut deps = base_dependencies(adapters)?;
            for (name, version) in pins {
                deps.insert(name.clone(), json!(version));
            }
            (deps, Some(base_checksum(adapters)?))
        }
    };
    write_json(
        &dir.path().join("package.json"),
        &package_json(set, adapters, dependencies)?,
    )?;
    let mut args = vec!["install".to_owned(), "--package-lock-only".to_owned()];
    args.extend(npm_flags(set));
    npm(dir.path(), &args)?;
    let path = dir.path().join("package-lock.json");
    let lock: Json = serde_json::from_slice(&read(&path)?)
        .map_err(|e| format!("npm's lock for {}: {e}", set.name()))?;
    let packages = match &lock["packages"] {
        Json::Object(packages) => packages.clone(),
        _ => return Err(format!("npm's lock for {} has no packages", set.name()).into()),
    };
    let missing = missing(&packages);
    if !missing.is_empty() {
        let version = match npm(dir.path(), &["--version".to_owned()]) {
            Ok(version) => version.trim().to_owned(),
            Err(e) => format!("(version unknown: {e})"),
        };
        return Err(format!(
            "npm {version} wrote an incomplete lock for the `{}` set (another npm would refuse to install it): {}; upgrade npm (11.19 resolves it whole)",
            set.name(),
            missing.join(", ")
        )
        .into());
    }
    Ok(SetLock {
        base,
        requested: pins.clone(),
        packages,
    })
}

/// What npm's `packages` lacks: each entry's `dependencies` and
/// `optionalDependencies` must resolve, as Node resolves them, from the
/// entry's own `node_modules` up to the root's (a v3 lock lists every
/// platform's optional packages). npm 11.5 drops a peer-only package's
/// optional dependencies when it resolves a lock, and a newer `npm ci`
/// refuses that lock: "dep of package" each.
fn missing(packages: &Map<String, Json>) -> Vec<String> {
    let mut out = Vec::new();
    for (path, entry) in packages {
        for key in ["dependencies", "optionalDependencies"] {
            let Some(deps) = entry[key].as_object() else {
                continue;
            };
            for dep in deps.keys() {
                if resolve_from(packages, path, dep).is_none() {
                    let of = path
                        .rsplit_once("node_modules/")
                        .map_or("the root", |(_, n)| n);
                    out.push(format!("{dep} of {of}"));
                }
            }
        }
    }
    out
}

/// Where `dep` resolves from the package at `path` (a key of `packages`),
/// as Node resolves it: `a/node_modules/b` looks in
/// `a/node_modules/b/node_modules`, then `a/node_modules`, then the root's.
fn resolve_from(packages: &Map<String, Json>, path: &str, dep: &str) -> Option<String> {
    let mut at = path;
    loop {
        let candidate = match at.is_empty() {
            true => format!("node_modules/{dep}"),
            false => format!("{at}/node_modules/{dep}"),
        };
        if packages.contains_key(&candidate) {
            return Some(candidate);
        }
        if at.is_empty() {
            return None;
        }
        at = match at.rfind("/node_modules/") {
            Some(i) => &at[..i],
            None => "",
        };
    }
}

/// The paths in `packages` a package at `path` brings: itself, and what its
/// dependencies, optional dependencies and peers resolve to, transitively.
fn brought(packages: &Map<String, Json>, path: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut todo = vec![path.to_owned()];
    while let Some(at) = todo.pop() {
        if !seen.insert(at.clone()) {
            continue;
        }
        let Some(entry) = packages.get(&at) else {
            continue;
        };
        for key in ["dependencies", "optionalDependencies", "peerDependencies"] {
            for dep in entry[key].as_object().into_iter().flat_map(|d| d.keys()) {
                if let Some(found) = resolve_from(packages, &at, dep) {
                    todo.push(found);
                }
            }
        }
    }
    seen
}

/// The installed packages that have install scripts (npm's
/// `hasInstallScript`), which were not run: by pin, each package a pin
/// brings in its set; in the `dsh` set the base's own apart, said once
/// (what every DSH plugin gets).
#[derive(Debug, Default)]
pub(crate) struct InstallScripts {
    by_pin: BTreeMap<(Set, String), Vec<String>>,
    pub base: Vec<String>,
}

impl InstallScripts {
    /// The packages with install scripts `pin` brings in `set`.
    pub fn of(&self, set: Set, pin: &str) -> Vec<String> {
        self.by_pin
            .get(&(set, pin.to_owned()))
            .cloned()
            .unwrap_or_default()
    }
}

/// What the manifest's lock says about install scripts (nothing when it has
/// no lock). With a `dsh` set it reads the DSH adapter's own lock too, to
/// tell the base's packages apart.
pub(crate) fn install_scripts(
    manifest: &Manifest,
    adapters: &Path,
) -> Result<InstallScripts, BoxError> {
    let mut out = InstallScripts::default();
    let Some(lock) = Lock::read(&lock_path(manifest))? else {
        return Ok(out);
    };
    let name_of = |path: &str| path.rsplit_once("node_modules/").map(|(_, n)| n.to_owned());
    let scripted = |packages: &Map<String, Json>, path: &str| {
        packages
            .get(path)
            .is_some_and(|p| p["hasInstallScript"] == Json::Bool(true))
    };
    // The base's packages: those the adapter's own lock has, at the same
    // place and version.
    let mut base = BTreeSet::new();
    if let Some(set) = lock.sets.get(&Set::Dsh) {
        let path = adapters.join("dsh/package-lock.json");
        let adapter: Json = serde_json::from_slice(&read(&path)?)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        for (at, entry) in adapter["packages"].as_object().into_iter().flatten() {
            if !at.is_empty()
                && set.packages.get(at).map(|p| &p["version"]) == Some(&entry["version"])
            {
                base.insert(at.clone());
            }
        }
        out.base = base
            .iter()
            .filter(|at| scripted(&set.packages, at))
            .filter_map(|at| name_of(at))
            .collect();
    }
    for (set, entry) in &lock.sets {
        for pin in entry.requested.keys() {
            let mut names: Vec<String> = brought(&entry.packages, &format!("node_modules/{pin}"))
                .into_iter()
                // The base is the `dsh` set's: each set is its own project.
                .filter(|at| !(*set == Set::Dsh && base.contains(at)))
                .filter(|at| scripted(&entry.packages, at))
                .filter_map(|at| name_of(&at))
                .collect();
            names.sort();
            names.dedup();
            out.by_pin.insert((*set, pin.clone()), names);
        }
    }
    Ok(out)
}

/// What is wrong with a lock entry read from a file, if anything: each pin
/// must be a root dependency at its version and installed at the top at it,
/// and nothing may be missing (a lock edited by hand, or merged, can
/// disagree with itself; deleting the cache would not help).
fn unsound(entry: &SetLock) -> Option<String> {
    let root = entry.packages.get("").map(|r| &r["dependencies"]);
    for (name, version) in &entry.requested {
        let at_root = root.and_then(|deps| deps[name].as_str());
        let installed = entry
            .packages
            .get(&format!("node_modules/{name}"))
            .and_then(|p| p["version"].as_str());
        if at_root != Some(version.as_str()) || installed != Some(version.as_str()) {
            return Some(format!("it does not hold {name}@{version}"));
        }
    }
    let missing = missing(&entry.packages);
    (!missing.is_empty()).then(|| format!("it lacks {}", missing.join(", ")))
}

/// Why a set in the lock cannot be used for `wanted`, if it cannot.
fn stale(
    entry: Option<&SetLock>,
    wanted: &BTreeMap<String, String>,
    base: Option<&String>,
) -> Option<String> {
    let Some(entry) = entry else {
        return Some("is missing".into());
    };
    if entry.requested != *wanted || entry.base.as_ref() != base {
        return Some("is not current: the manifest's pins or the DSH base changed".into());
    }
    unsound(entry).map(|why| format!("does not agree with itself: {why}"))
}

/// The package name and version of each entry in npm's `packages`.
fn versions(packages: &Map<String, Json>) -> BTreeSet<(String, String)> {
    packages
        .iter()
        .filter_map(|(path, entry)| {
            let name = path.rsplit_once("node_modules/")?.1;
            let version = entry["version"].as_str()?;
            Some((name.to_owned(), version.to_owned()))
        })
        .collect()
}

/// How many changes a notice names before "and N more".
const NAMED: usize = 12;

/// What a resolve made: for a new set, its pins and how many packages; for
/// one resolved again, the packages added and removed ("+a 1.0.0, −b
/// 0.3.1", the first [`NAMED`], then "and N more"; or "nothing changed").
fn changes(old: Option<&SetLock>, new: &SetLock) -> String {
    let after = versions(&new.packages);
    let Some(old) = old else {
        let pins: Vec<String> = new
            .requested
            .iter()
            .map(|(n, v)| format!("{n} {v}"))
            .collect();
        return format!("{} with {} package(s) in all", pins.join(", "), after.len());
    };
    let before = versions(&old.packages);
    let mut said: Vec<String> = after
        .difference(&before)
        .map(|(n, v)| format!("+{n} {v}"))
        .collect();
    said.extend(before.difference(&after).map(|(n, v)| format!("−{n} {v}")));
    if said.is_empty() {
        return "nothing changed".into();
    }
    let more = said.len().saturating_sub(NAMED);
    said.truncate(NAMED);
    let mut text = said.join(", ");
    if more > 0 {
        text.push_str(&format!(", and {more} more"));
    }
    text
}

/// Whether `dir` holds a set installed whole: its stamp names `hash`. No
/// stamp (or no folder) is `false`; a stamp that cannot be read is an error.
fn stamped(dir: &Path, hash: &str) -> Result<bool, BoxError> {
    let path = dir.join(STAMP);
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(text.trim() == hash),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("{}: {e}", path.display()).into()),
    }
}

/// Removes a folder, logging (not failing on) what it cannot.
fn remove_quietly(dir: &Path, why: &str) {
    if let Err(e) = std::fs::remove_dir_all(dir) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("removing {} ({why}): {e}", dir.display());
        }
    }
}

/// A set's folder, installed from its lock entry if it is not yet.
fn install_set(
    home: &Path,
    set: Set,
    entry: &SetLock,
    adapters: &Path,
) -> Result<PathBuf, BoxError> {
    let hash = hash(set, entry, adapters)?;
    let parent = home.join("plugins").join(set.name());
    let dir = parent.join(&hash);
    if stamped(&dir, &hash)? {
        return Ok(dir);
    }
    std::fs::create_dir_all(&parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    remove_old_partials(&parent);
    let partial = |what: &str| {
        parent.join(format!(
            "{hash}.partial-{what}{}",
            uuid::Uuid::new_v4().simple()
        ))
    };
    // A folder there without its stamp (or with another): changed by hand.
    // Looked at again, since another yobuild may have just made it; then
    // moved aside whole (a rename), so nobody ever runs from half of it.
    if dir.exists() && !stamped(&dir, &hash)? {
        tracing::warn!(
            "{}: no stamp or another hash; installing it again",
            dir.display()
        );
        let aside = partial("old-");
        match std::fs::rename(&dir, &aside) {
            Ok(()) => remove_quietly(&aside, "a cache folder changed by hand"),
            // Another yobuild moved it first.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", dir.display()).into()),
        }
    }
    if stamped(&dir, &hash)? {
        return Ok(dir);
    }
    let fresh = partial("");
    if let Err(e) = make(&fresh, set, entry, adapters, &hash) {
        remove_quietly(&fresh, "an install that failed");
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&fresh, &dir) {
        remove_quietly(&fresh, "an install another yobuild finished first");
        // Another yobuild made it first: the same lock, the same content.
        if !stamped(&dir, &hash)? {
            return Err(format!("{}: {e}", dir.display()).into());
        }
        tracing::debug!("{}: made by another yobuild meanwhile", dir.display());
    }
    Ok(dir)
}

/// Fills a fresh folder with a set: its files, `npm ci`, then the stamp.
fn make(
    dir: &Path,
    set: Set,
    entry: &SetLock,
    adapters: &Path,
    hash: &str,
) -> Result<(), BoxError> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let dependencies = match entry.packages.get("").map(|root| &root["dependencies"]) {
        Some(Json::Object(deps)) => deps.clone(),
        Some(Json::Null) | None => Map::new(),
        Some(_) => {
            return Err(format!(
                "the lock's `{}` set: its root dependencies are not an object",
                set.name()
            )
            .into())
        }
    };
    let package = package_json(set, adapters, dependencies)?;
    write_json(&dir.join("package.json"), &package)?;
    write_json(
        &dir.join("package-lock.json"),
        &json!({
            "name": package["name"],
            "lockfileVersion": 3,
            "requires": true,
            "packages": entry.packages,
        }),
    )?;
    write(&dir.join(".npmrc"), npmrc(set))?;
    if set == Set::Dsh {
        for name in DSH_FILES {
            let from = adapters.join("dsh").join(name);
            std::fs::copy(&from, dir.join(name)).map_err(|e| format!("{}: {e}", from.display()))?;
        }
    }
    let mut args = vec!["ci".to_owned(), "--prefer-offline".to_owned()];
    args.extend(npm_flags(set));
    npm(dir, &args)?;
    write(&dir.join(STAMP), format!("{hash}\n"))
}

/// Removes `.partial-*` folders an install that died left (an hour old);
/// what it cannot read or remove is logged.
fn remove_old_partials(parent: &Path) {
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!("{}: {e}", parent.display());
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                tracing::warn!("{}: {e}", parent.display());
                continue;
            }
        };
        if !entry.file_name().to_string_lossy().contains(".partial-") {
            continue;
        }
        // A time in the future (a clock set back) counts as new.
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|t| t.elapsed().is_ok_and(|age| age > PARTIAL_AGE));
        match old {
            Ok(true) => remove_quietly(&entry.path(), "an install that died"),
            Ok(false) => {}
            Err(e) => tracing::warn!("{}: {e}", entry.path().display()),
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Installs the manifest's pinned packages: the lock beside it read; a set
/// it no longer needs dropped, and a missing or stale one (or one that
/// disagrees with itself) resolved again — each refused instead with
/// `locked`; the lock written if that changed it; then each set installed
/// into the cache under `home` (`None`: `YOBUILD_HOME`, else `~/.yobuild`,
/// looked up only when there is something to install).
pub fn install(
    manifest: &Manifest,
    adapters: &Path,
    home: Option<&Path>,
    locked: bool,
) -> Result<Installed, BoxError> {
    let pins = pins(manifest, adapters)?;
    let home = match (home, pins.is_empty()) {
        (Some(home), _) => home.to_path_buf(),
        (None, false) => self::home()?,
        (None, true) => PathBuf::new(),
    };
    let path = lock_path(manifest);
    let old = Lock::read(&path)?;
    let mut lock = old.clone().unwrap_or_else(Lock::empty);
    let mut installed = Installed::default();
    let base = match pins.contains_key(&Set::Dsh) {
        true => Some(base_checksum(adapters)?),
        false => None,
    };
    let unneeded: Vec<Set> = lock
        .sets
        .keys()
        .filter(|set| !pins.contains_key(set))
        .copied()
        .collect();
    for set in unneeded {
        if locked {
            return Err(format!(
                "{}: its `{}` set is no longer needed (--locked: run without it to update the lock)",
                path.display(),
                set.name()
            )
            .into());
        }
        lock.sets.remove(&set);
        installed.said.push(format!(
            "{}: `{}` removed (no pins left)",
            file_name(&path),
            set.name()
        ));
    }
    for (set, wanted) in &pins {
        let previous = lock.sets.get(set);
        let base = (*set == Set::Dsh).then_some(base.as_ref()).flatten();
        let Some(why) = stale(previous, wanted, base) else {
            continue;
        };
        if locked {
            return Err(format!(
                "{}: its `{}` set {why} (--locked: run without it to resolve the set again)",
                path.display(),
                set.name()
            )
            .into());
        }
        let entry = resolve(*set, wanted, adapters)
            .map_err(|e| format!("resolving the `{}` set: {e}", set.name()))?;
        let verb = match previous {
            Some(_) => "resolved again",
            None => "resolved",
        };
        installed.said.push(format!(
            "{}: `{}` {verb}: {}",
            file_name(&path),
            set.name(),
            changes(previous, &entry)
        ));
        lock.sets.insert(*set, entry);
    }
    if old.as_ref() != Some(&lock) && !(old.is_none() && lock.sets.is_empty()) {
        lock.write(&path)?;
    }
    for set in pins.keys() {
        let dir = install_set(&home, *set, &lock.sets[set], adapters)
            .map_err(|e| format!("installing the `{}` set: {e}", set.name()))?;
        installed.dirs.insert(*set, dir);
    }
    Ok(installed)
}

/// For tests: a set installed by hand, with a lock beside `manifest_file`
/// that holds it (through the same hash, so a run finds it current and
/// installed, and never calls npm). `packages`: each package's name,
/// version and the folder copied into `node_modules/`. Not for `dsh`.
#[doc(hidden)]
pub fn seed(
    home: &Path,
    manifest_file: &Path,
    set: Set,
    packages: &[(&str, &str, &Path)],
) -> Result<PathBuf, BoxError> {
    assert!(set != Set::Dsh, "a seeded dsh set would need the base");
    let requested: BTreeMap<String, String> = packages
        .iter()
        .map(|(n, v, _)| (n.to_string(), v.to_string()))
        .collect();
    let mut lock_packages = Map::new();
    lock_packages.insert(
        String::new(),
        json!({ "name": format!("yobuild-{}", set.name()), "dependencies": requested }),
    );
    for (name, version, _) in packages {
        lock_packages.insert(
            format!("node_modules/{name}"),
            json!({ "version": version }),
        );
    }
    let entry = SetLock {
        base: None,
        requested,
        packages: lock_packages,
    };
    let hash = hash(set, &entry, Path::new(""))?;
    let dir = home.join("plugins").join(set.name()).join(&hash);
    for (name, _, from) in packages {
        copy_tree(from, &dir.join("node_modules").join(name))?;
    }
    write(&dir.join(STAMP), format!("{hash}\n"))?;
    let lock_path = lock_beside(manifest_file);
    let mut lock = Lock::read(&lock_path)?.unwrap_or_else(Lock::empty);
    lock.sets.insert(set, entry);
    lock.write(&lock_path)?;
    Ok(dir)
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), BoxError> {
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    for entry in std::fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)
                .map_err(|e| format!("{}: {e}", entry.path().display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_sorts_keys_at_every_level() {
        let value: Json =
            serde_json::from_str(r#"{"b":1,"a":{"d":[{"z":1,"y":"é"}],"c":null}}"#).unwrap();
        assert_eq!(
            canonical(&value),
            r#"{"a":{"c":null,"d":[{"y":"é","z":1}]},"b":1}"#
        );
    }

    #[test]
    fn a_hash_is_fixed_and_independent_of_key_order() {
        let entry = |packages: &str| SetLock {
            base: None,
            requested: [("x".to_owned(), "1.0.0".to_owned())].into(),
            packages: serde_json::from_str(packages).unwrap(),
        };
        let a = entry(
            r#"{"":{"dependencies":{"x":"1.0.0"}},"node_modules/x":{"version":"1.0.0","integrity":"sha512-a"}}"#,
        );
        let b = entry(
            r#"{"node_modules/x":{"integrity":"sha512-a","version":"1.0.0"},"":{"dependencies":{"x":"1.0.0"}}}"#,
        );
        let adapters = Path::new("/nowhere");
        let hash_a = hash(Set::Pi, &a, adapters).unwrap();
        assert_eq!(hash_a, hash(Set::Pi, &b, adapters).unwrap());
        assert_eq!(hash_a.len(), 16);
        // The fixed vector: a change to the canonical form or the hash
        // moves every cache folder.
        assert_eq!(hash_a, "51342142c18f25fb");
        assert_ne!(hash_a, hash(Set::Mcp, &a, adapters).unwrap());
    }

    #[test]
    fn ranges_intersect_per_alternative() {
        assert_eq!(intersect("*", "^2.1.0"), "* ^2.1.0");
        assert_eq!(
            intersect("1.x || 2.x", "^2.1.0"),
            "1.x ^2.1.0 || 2.x ^2.1.0"
        );
        assert_eq!(intersect("^1 || ^2", "^1 || ^2").split(" || ").count(), 4);
    }

    #[test]
    fn only_the_registry_and_its_login_reach_npm() {
        assert!(npm_variable_kept("PATH"));
        assert!(npm_variable_kept("npm_config_registry"));
        assert!(npm_variable_kept("NPM_CONFIG_REGISTRY"));
        assert!(npm_variable_kept("npm_config_@acme:registry"));
        assert!(npm_variable_kept("npm_config_//npm.acme.dev/:_authToken"));
        assert!(!npm_variable_kept("npm_config_legacy_peer_deps"));
        assert!(!npm_variable_kept("NPM_CONFIG_BEFORE"));
        assert!(!npm_variable_kept("npm_config_omit"));
    }

    #[test]
    fn npm_errors_keep_their_message_not_the_usage() {
        let stderr = "npm error code EUSAGE\nnpm error\nnpm error `npm ci` can only install when in sync.\nnpm error\nnpm error Usage:\nnpm error npm ci\nnpm error   --install-links\nnpm error A complete log of this run can be found in: /x.log\n";
        assert_eq!(
            npm_error(stderr),
            "npm error code EUSAGE\nnpm error\nnpm error `npm ci` can only install when in sync.\nnpm error\nnpm error A complete log of this run can be found in: /x.log"
        );
    }

    #[test]
    fn a_lock_missing_a_dependency_is_found() {
        let packages: Map<String, Json> = serde_json::from_str(
            r#"{
                "": { "dependencies": { "a": "1" } },
                "node_modules/a": { "version": "1.0.0", "dependencies": { "b": "1" }, "optionalDependencies": { "a-linux": "1", "a-darwin": "1" } },
                "node_modules/a/node_modules/b": { "version": "1.0.0", "dependencies": { "c": "1" } },
                "node_modules/c": { "version": "1.0.0" },
                "node_modules/a-darwin": { "version": "1.0.0" }
            }"#,
        )
        .unwrap();
        // b is nested under a; c is found from b up at the root; a-linux is
        // nowhere.
        assert_eq!(missing(&packages), ["a-linux of a"]);
        // The adapter's own lock is whole.
        let adapter = Path::new(env!("CARGO_MANIFEST_DIR")).join("adapters/dsh/package-lock.json");
        if let Ok(text) = std::fs::read_to_string(adapter) {
            let lock: Json = serde_json::from_str(&text).unwrap();
            assert_eq!(
                missing(lock["packages"].as_object().unwrap()),
                Vec::<String>::new()
            );
        }
    }

    #[test]
    fn a_pin_brings_its_dependencies_and_peers() {
        let packages: Map<String, Json> = serde_json::from_str(
            r#"{
                "": { "dependencies": { "a": "1", "p": "1" } },
                "node_modules/a": { "version": "1.0.0", "dependencies": { "b": "1" }, "peerDependencies": { "p": "*" } },
                "node_modules/a/node_modules/b": { "version": "1.0.0", "hasInstallScript": true },
                "node_modules/p": { "version": "1.0.0" },
                "node_modules/other": { "version": "1.0.0" }
            }"#,
        )
        .unwrap();
        assert_eq!(
            brought(&packages, "node_modules/a")
                .into_iter()
                .collect::<Vec<_>>(),
            ["node_modules/a", "node_modules/a/node_modules/b", "node_modules/p"]
        );
    }

    #[test]
    fn install_scripts_belong_to_the_pin_that_brings_them_and_the_base_apart() {
        let dir = tempfile::tempdir().unwrap();
        let adapters = tempfile::tempdir().unwrap();
        std::fs::create_dir(adapters.path().join("dsh")).unwrap();
        // The base: esbuild 1, which has a script.
        std::fs::write(
            adapters.path().join("dsh/package-lock.json"),
            r#"{ "packages": { "": {}, "node_modules/esbuild": { "version": "1.0.0" } } }"#,
        )
        .unwrap();
        let set = |pins: &str, packages: &str| {
            format!(r#"{{ "requested": {pins}, "packages": {packages} }}"#)
        };
        let dsh = set(
            r#"{ "d": "1.0.0" }"#,
            r#"{ "": {}, "node_modules/d": { "version": "1.0.0", "dependencies": { "esbuild": "1", "k": "1" } },
                 "node_modules/esbuild": { "version": "1.0.0", "hasInstallScript": true },
                 "node_modules/k": { "version": "1.0.0", "hasInstallScript": true } }"#,
        );
        // The pi set brings its own esbuild: its own, whatever the base has.
        let pi = set(
            r#"{ "p": "1.0.0" }"#,
            r#"{ "": {}, "node_modules/p": { "version": "1.0.0", "dependencies": { "esbuild": "1" } },
                 "node_modules/esbuild": { "version": "1.0.0", "hasInstallScript": true } }"#,
        );
        std::fs::write(
            dir.path().join("agent.lock"),
            format!(r#"{{ "lockVersion": 1, "sets": {{ "dsh": {dsh}, "pi": {pi} }} }}"#),
        )
        .unwrap();
        let m = manifest(dir.path(), "");
        let scripts = install_scripts(&m, adapters.path()).unwrap();
        assert_eq!(scripts.base, ["esbuild"]);
        assert_eq!(scripts.of(Set::Dsh, "d"), ["k"]);
        assert_eq!(scripts.of(Set::Pi, "p"), ["esbuild"]);
        assert!(scripts.of(Set::Mcp, "p").is_empty());
    }

    #[test]
    fn pis_own_packages_are_a_fixed_pattern() {
        assert!(pis_own("@earendil-works/pi-coding-agent"));
        assert!(pis_own("typebox"));
        assert!(!pis_own("@juicesharp/rpiv-i18n"));
        assert!(!pis_own("jiti"));
    }

    #[test]
    fn changes_name_what_was_added_and_removed() {
        let entry = |packages: &str| SetLock {
            base: None,
            requested: BTreeMap::new(),
            packages: serde_json::from_str(packages).unwrap(),
        };
        let old = entry(
            r#"{"":{},"node_modules/a":{"version":"1.0.0"},"node_modules/b/node_modules/z":{"version":"0.3.1"}}"#,
        );
        let new = entry(r#"{"":{},"node_modules/a":{"version":"1.1.0"}}"#);
        assert_eq!(changes(Some(&old), &new), "+a 1.1.0, −a 1.0.0, −z 0.3.1");
        assert_eq!(changes(Some(&new), &new), "nothing changed");
        let mut first = new.clone();
        first.requested = [("a".to_owned(), "1.1.0".to_owned())].into();
        assert_eq!(changes(None, &first), "a 1.1.0 with 1 package(s) in all");
        let many = entry(&format!(
            "{{{}}}",
            (0..15)
                .map(|i| format!(r#""node_modules/p{i:02}":{{"version":"1.0.0"}}"#))
                .collect::<Vec<_>>()
                .join(",")
        ));
        assert!(changes(Some(&new), &many).ends_with("+p11 1.0.0, and 4 more"));
    }

    #[test]
    fn a_lock_is_read_strictly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.lock");
        assert!(Lock::read(&path).unwrap().is_none());
        std::fs::write(&path, r#"{"lockVersion":2,"sets":{}}"#).unwrap();
        assert!(Lock::read(&path)
            .unwrap_err()
            .to_string()
            .contains("lockVersion 2"));
        // A later version is said as such, whatever its shape.
        std::fs::write(&path, r#"{"lockVersion":2,"sets":{},"signed":true}"#).unwrap();
        assert!(Lock::read(&path)
            .unwrap_err()
            .to_string()
            .ends_with("lockVersion 2, which this yobuild does not read (it writes 1)"));
        std::fs::write(&path, r#"{"sets":{}}"#).unwrap();
        assert!(Lock::read(&path)
            .unwrap_err()
            .to_string()
            .ends_with("no lockVersion: not a yobuild lock"));
        std::fs::write(
            &path,
            r#"{"lockVersion":1,"sets":{"go":{"requested":{},"packages":{}}}}"#,
        )
        .unwrap();
        assert!(Lock::read(&path)
            .unwrap_err()
            .to_string()
            .contains("unknown variant `go`"));
        std::fs::write(&path, r#"{"lockVersion":1,"sets":{},"extra":1}"#).unwrap();
        assert!(Lock::read(&path).is_err());
    }

    #[test]
    fn a_lock_lists_its_sets_in_a_fixed_order() {
        let entry = SetLock {
            base: None,
            requested: BTreeMap::new(),
            packages: Map::new(),
        };
        let mut lock = Lock::empty();
        for set in [Set::Pi, Set::Mcp, Set::Dsh] {
            lock.sets.insert(set, entry.clone());
        }
        let text = serde_json::to_string(&lock).unwrap();
        let at = |name: &str| text.find(&format!("\"{name}\":")).unwrap();
        assert!(at("dsh") < at("mcp") && at("mcp") < at("pi"), "{text}");
    }

    #[test]
    fn a_lock_is_named_after_its_manifest() {
        assert_eq!(
            lock_beside(Path::new("/a/agent.toml")),
            Path::new("/a/agent.lock")
        );
        assert_eq!(
            lock_beside(Path::new("/a/handoff.local.toml")),
            Path::new("/a/handoff.local.lock")
        );
        let dir = tempfile::tempdir().unwrap();
        let m = Manifest::parse(
            "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n",
            Path::new("handoff.local.toml"),
            dir.path(),
        )
        .unwrap();
        assert_eq!(lock_path(&m), dir.path().join("handoff.local.lock"));
    }

    fn manifest(dir: &Path, plugins: &str) -> Manifest {
        Manifest::parse(
            &format!("[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n{plugins}"),
            &dir.join("agent.toml"),
            dir,
        )
        .unwrap()
    }

    /// An adapters folder whose DSH base pins `dsh-free-search` 0.8.1.
    fn adapters() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("dsh")).unwrap();
        std::fs::write(
            dir.path().join("dsh/package.json"),
            r#"{ "name": "base", "type": "module", "dependencies": { "dsh-free-search": "0.8.1" } }"#,
        )
        .unwrap();
        dir
    }

    #[test]
    fn a_dsh_package_the_base_pins_at_another_version_is_refused() {
        let adapters = adapters();
        let dir = tempfile::tempdir().unwrap();
        let other = manifest(dir.path(), "[[plugin]]\ndsh = \"dsh-free-search@0.9.0\"\n");
        assert_eq!(
            pins(&other, adapters.path()).unwrap_err().to_string(),
            "plugin dsh-free-search: dsh-free-search@0.9.0 is pinned by the DSH base at 0.8.1"
        );
        let same = manifest(dir.path(), "[[plugin]]\ndsh = \"dsh-free-search@0.8.1\"\n");
        assert_eq!(
            pins(&same, adapters.path()).unwrap()[&Set::Dsh]["dsh-free-search"],
            "0.8.1"
        );
    }

    #[test]
    fn dsh_files_and_dsh_packages_do_not_run_together() {
        let adapters = adapters();
        let dir = tempfile::tempdir().unwrap();
        let mixed = manifest(
            dir.path(),
            "[[plugin]]\ndsh = \"./mine.ts\"\n[[plugin]]\ndsh = \"dsh-free-search@0.8.1\"\n",
        );
        assert_eq!(
            pins(&mixed, adapters.path()).unwrap_err().to_string(),
            "DSH files and DSH packages cannot run together yet (plugin mine, a file; plugin dsh-free-search, a package): the packages run in the cache, the files in adapters/dsh"
        );
        // Files alone install nothing.
        let files = manifest(
            dir.path(),
            "[[plugin]]\ndsh = \"./mine.ts\"\n[[plugin]]\npi = \"./p.ts\"\n",
        );
        assert!(pins(&files, adapters.path()).unwrap().is_empty());
    }

    #[test]
    fn nothing_pinned_writes_no_lock_and_needs_no_home() {
        let dir = tempfile::tempdir().unwrap();
        let none = manifest(dir.path(), "");
        let installed = install(&none, Path::new("/nowhere"), None, true).unwrap();
        assert!(installed.said().is_empty() && installed.get(Set::Pi).is_none());
        assert!(!dir.path().join("agent.lock").exists());
    }

    #[test]
    fn locked_refuses_a_set_no_longer_needed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("agent.lock"),
            r#"{"lockVersion":1,"sets":{"pi":{"requested":{"x":"1.0.0"},"packages":{}}}}"#,
        )
        .unwrap();
        let none = manifest(dir.path(), "");
        let e = install(&none, Path::new("/nowhere"), None, true).unwrap_err();
        assert!(
            e.to_string().ends_with(
                "its `pi` set is no longer needed (--locked: run without it to update the lock)"
            ),
            "{e}"
        );
        // Without --locked, the set goes, and with it the file.
        let installed = install(&none, Path::new("/nowhere"), None, false).unwrap();
        assert_eq!(
            installed.said(),
            ["agent.lock: `pi` removed (no pins left)"]
        );
        assert!(!dir.path().join("agent.lock").exists());
    }

    #[test]
    fn an_empty_lock_removes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.lock");
        std::fs::write(&path, "{}").unwrap();
        Lock::empty().write(&path).unwrap();
        assert!(!path.exists());
    }

    /// A set entry with these pins, installed at the top at their versions.
    fn entry_with(pins: &[(&str, &str)]) -> SetLock {
        let requested: BTreeMap<String, String> = pins
            .iter()
            .map(|(n, v)| (n.to_string(), v.to_string()))
            .collect();
        let mut packages = Map::new();
        packages.insert(String::new(), json!({ "dependencies": requested }));
        for (name, version) in pins {
            packages.insert(
                format!("node_modules/{name}"),
                json!({ "version": version }),
            );
        }
        SetLock {
            base: None,
            requested,
            packages,
        }
    }

    #[test]
    fn a_set_is_stale_when_its_pins_base_or_contents_disagree() {
        let wanted: BTreeMap<String, String> = [("x".to_owned(), "1.0.0".to_owned())].into();
        let entry = entry_with(&[("x", "1.0.0")]);
        assert_eq!(stale(Some(&entry), &wanted, None), None);
        assert_eq!(stale(None, &wanted, None).as_deref(), Some("is missing"));
        let other: BTreeMap<String, String> = [("x".to_owned(), "1.0.1".to_owned())].into();
        assert!(stale(Some(&entry), &other, None)
            .unwrap()
            .starts_with("is not current"));
        // The DSH base: another checksum makes it stale; the same does not.
        let mut dsh = entry.clone();
        dsh.base = Some("aaa".into());
        assert_eq!(stale(Some(&dsh), &wanted, Some(&"aaa".to_owned())), None);
        assert!(stale(Some(&dsh), &wanted, Some(&"bbb".to_owned()))
            .unwrap()
            .starts_with("is not current"));
        // A lock edited by hand: its pin not installed at its version.
        let mut edited = entry.clone();
        edited
            .packages
            .insert("node_modules/x".into(), json!({ "version": "0.9.0" }));
        assert_eq!(
            stale(Some(&edited), &wanted, None).as_deref(),
            Some("does not agree with itself: it does not hold x@1.0.0")
        );
        let mut lacking = entry.clone();
        lacking.packages.insert(
            "node_modules/x".into(),
            json!({ "version": "1.0.0", "dependencies": { "y": "1" } }),
        );
        assert_eq!(
            stale(Some(&lacking), &wanted, None).as_deref(),
            Some("does not agree with itself: it lacks y of x")
        );
    }

    #[test]
    fn locked_refuses_a_missing_set_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let m = manifest(dir.path(), "[[plugin]]\npi = \"x@1.0.0\"\n");
        let e = install(&m, Path::new("/nowhere"), Some(home.path()), true).unwrap_err();
        assert!(
            e.to_string()
                .ends_with("agent.lock: its `pi` set is missing (--locked: run without it to resolve the set again)"),
            "{e}"
        );
        assert!(!dir.path().join("agent.lock").exists());
    }

    #[test]
    fn a_current_lock_is_left_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let package = dir.path().join("x");
        std::fs::create_dir(&package).unwrap();
        std::fs::write(
            package.join("package.json"),
            r#"{"name":"x","version":"1.0.0"}"#,
        )
        .unwrap();
        let manifest_file = dir.path().join("agent.toml");
        let seeded = seed(
            home.path(),
            &manifest_file,
            Set::Pi,
            &[("x", "1.0.0", &package)],
        )
        .unwrap();
        // The same lock in another layout: read as the same, never rewritten.
        let lock = dir.path().join("agent.lock");
        let compact: Json = serde_json::from_str(&std::fs::read_to_string(&lock).unwrap()).unwrap();
        std::fs::write(&lock, compact.to_string()).unwrap();
        let before = std::fs::read(&lock).unwrap();
        let m = manifest(dir.path(), "[[plugin]]\npi = \"x@1.0.0\"\n");
        let installed = install(&m, Path::new("/nowhere"), Some(home.path()), false).unwrap();
        assert_eq!(installed.get(Set::Pi), Some(seeded.as_path()));
        assert!(installed.said().is_empty());
        assert_eq!(std::fs::read(&lock).unwrap(), before);
    }

    #[test]
    fn a_dsh_hash_covers_the_adapter_files_and_not_the_pins() {
        let adapters = tempfile::tempdir().unwrap();
        std::fs::create_dir(adapters.path().join("dsh")).unwrap();
        for name in DSH_FILES {
            std::fs::write(adapters.path().join("dsh").join(name), name).unwrap();
        }
        let entry = entry_with(&[("x", "1.0.0")]);
        let first = hash(Set::Dsh, &entry, adapters.path()).unwrap();
        let mut asked_otherwise = entry.clone();
        asked_otherwise.requested.clear();
        assert_eq!(
            hash(Set::Dsh, &asked_otherwise, adapters.path()).unwrap(),
            first
        );
        std::fs::write(adapters.path().join("dsh/host-dialogs.ts"), "edited").unwrap();
        assert_ne!(hash(Set::Dsh, &entry, adapters.path()).unwrap(), first);
    }

    #[test]
    fn old_partial_folders_go_and_new_ones_stay() {
        let parent = tempfile::tempdir().unwrap();
        let make = |name: &str, age: u64| {
            let dir = parent.path().join(name);
            std::fs::create_dir(&dir).unwrap();
            let then = std::time::SystemTime::now() - Duration::from_secs(age);
            std::fs::File::open(&dir)
                .unwrap()
                .set_modified(then)
                .unwrap();
            dir
        };
        let old = make("abc.partial-1", 2 * 3600);
        let new = make("abc.partial-2", 60);
        let kept = make("abc", 2 * 3600);
        remove_old_partials(parent.path());
        assert!(!old.exists() && new.exists() && kept.exists());
    }

    #[test]
    fn a_stamp_that_cannot_be_read_is_an_error_not_a_reinstall() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!stamped(dir.path(), "h").unwrap());
        std::fs::write(dir.path().join(STAMP), "h\n").unwrap();
        assert!(stamped(dir.path(), "h").unwrap());
        assert!(!stamped(dir.path(), "other").unwrap());
        std::fs::remove_file(dir.path().join(STAMP)).unwrap();
        std::fs::create_dir(dir.path().join(STAMP)).unwrap();
        assert!(stamped(dir.path(), "h").is_err());
    }

    #[test]
    fn peers_skip_pis_own_and_the_pins_and_intersect() {
        let pins: BTreeMap<String, String> = [
            ("a".to_owned(), "1.0.0".to_owned()),
            ("b".to_owned(), "2.0.0".to_owned()),
        ]
        .into();
        let mut peers = BTreeMap::new();
        let view = json!({ "peerDependencies": { "@earendil-works/pi-tui": "*", "typebox": "*", "b": "^1", "c": "^1 || ^2" } });
        peers_of("a@1.0.0", &view, &pins, &mut peers).unwrap();
        peers_of(
            "b@2.0.0",
            &json!({ "peerDependencies": { "c": "^2.1.0" } }),
            &pins,
            &mut peers,
        )
        .unwrap();
        assert_eq!(
            peers,
            BTreeMap::from([("c".to_owned(), "^1 ^2.1.0 || ^2 ^2.1.0".to_owned())])
        );
        assert!(peers_of(
            "x@1.0.0",
            &json!({ "peerDependencies": [] }),
            &pins,
            &mut peers
        )
        .is_err());
    }

    /// Whether npm is here (`install_set` runs it; CI has it).
    fn npm_here() -> bool {
        std::process::Command::new("npm")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    /// An `mcp` set with nothing in it: `npm ci` installs it offline.
    fn nothing() -> SetLock {
        SetLock {
            base: None,
            requested: BTreeMap::new(),
            packages: serde_json::from_str(r#"{"":{"name":"yobuild-mcp"}}"#).unwrap(),
        }
    }

    #[test]
    fn a_folder_without_its_stamp_is_installed_again_and_a_stamped_one_kept() {
        if !npm_here() {
            return;
        }
        let home = tempfile::tempdir().unwrap();
        let entry = nothing();
        let hash = hash(Set::Mcp, &entry, Path::new("")).unwrap();
        let dir = home.path().join("plugins/mcp").join(&hash);
        // Changed by hand: no stamp, or another hash.
        for stamp in [None, Some("another")] {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("junk"), "x").unwrap();
            if let Some(stamp) = stamp {
                std::fs::write(dir.join(STAMP), stamp).unwrap();
            }
            assert_eq!(
                install_set(home.path(), Set::Mcp, &entry, Path::new("")).unwrap(),
                dir
            );
            assert!(!dir.join("junk").exists(), "{stamp:?}");
            assert_eq!(
                std::fs::read_to_string(dir.join(STAMP)).unwrap().trim(),
                hash
            );
            assert!(dir.join("package.json").is_file() && dir.join(".npmrc").is_file());
        }
        // Stamped: kept as it is (npm not run again).
        std::fs::write(dir.join("junk"), "x").unwrap();
        install_set(home.path(), Set::Mcp, &entry, Path::new("")).unwrap();
        assert!(dir.join("junk").exists());
        let leftovers: Vec<_> = std::fs::read_dir(home.path().join("plugins/mcp"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, [std::ffi::OsString::from(&hash)]);
    }

    #[test]
    fn a_failed_install_leaves_nothing_behind() {
        let home = tempfile::tempdir().unwrap();
        // Root dependencies that are not an object: the install fails once
        // its folder is made.
        let mut entry = nothing();
        entry.packages.insert(
            String::new(),
            json!({ "name": "yobuild-mcp", "dependencies": 5 }),
        );
        let e = install_set(home.path(), Set::Mcp, &entry, Path::new(""))
            .unwrap_err()
            .to_string();
        assert!(e.contains("root dependencies are not an object"), "{e}");
        let left: Vec<_> = std::fs::read_dir(home.path().join("plugins/mcp"))
            .unwrap()
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }
}
