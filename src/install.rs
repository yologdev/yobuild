//! A manifest's pinned packages, installed (design §4): pi plugins, DSH
//! plugins and stdio MCP servers, each ecosystem's an npm project of its
//! own (a *set*), resolved into the lock beside the manifest (`agent.toml`
//! → `agent.lock`) and installed with `npm ci` into a cache folder named
//! after the set's hash,
//! `<home>/plugins/<set>/<hash>/` (`home`: `YOBUILD_HOME`, else
//! `~/.yobuild`), shared by every agent whose lock holds the same set.
//! Never a package script; npm's settings that decide a result passed on
//! its command line. Unix only.
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

/// A manifest's lock: beside it, named after it (`agent.toml` →
/// `agent.lock`), so manifests sharing a folder keep their own.
pub fn lock_path(manifest: &Manifest) -> PathBuf {
    let name = manifest.path.file_stem().unwrap_or_default();
    manifest.dir.join(Path::new(name).with_extension("lock"))
}
/// The file an installed set's folder holds, last: its hash.
pub const STAMP: &str = ".yobuild-installed";
/// The adapter files the `dsh` set holds at its root, where they resolve
/// the set's Cordis: the runtime loads them from there.
pub const DSH_FILES: [&str; 3] = [
    "dsh-tools-adapter.ts", "host-dialogs.ts", "probe-services.ts",
];
/// How long one npm call may take.
const NPM_TIME: Duration = Duration::from_secs(600);
/// A `.partial-*` folder older than this was left by an install that died.
const PARTIAL_AGE: Duration = Duration::from_secs(3600);

/// The cache's home: `YOBUILD_HOME`, else `~/.yobuild`.
pub fn home() -> Result<PathBuf, BoxError> {
    match std::env::var_os("YOBUILD_HOME").filter(|v| !v.is_empty()) {
        Some(home) => Ok(PathBuf::from(home)),
        None => match std::env::var_os("HOME").filter(|v| !v.is_empty()) {
            Some(home) => Ok(Path::new(&home).join(".yobuild")),
            None => Err("neither YOBUILD_HOME nor HOME is set: where would packages go?".into()),
        },
    }
}

/// One ecosystem's npm project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Set {
    Pi,
    Dsh,
    Mcp,
}

impl Set {
    pub const ALL: [Set; 3] = [Set::Pi, Set::Dsh, Set::Mcp];

    pub fn name(self) -> &'static str {
        match self {
            Set::Pi => "pi",
            Set::Dsh => "dsh",
            Set::Mcp => "mcp",
        }
    }

    fn named(name: &str) -> Option<Set> {
        Set::ALL.into_iter().find(|s| s.name() == name)
    }
}

/// `agent.lock`: per set, what the manifest pinned and npm's own lock.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Lock {
    pub lock_version: u32,
    pub sets: BTreeMap<String, SetLock>,
}

/// One set in `agent.lock`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetLock {
    /// For `dsh`: the checksum of the DSH adapter's `package.json` and
    /// lock, the base the set was resolved over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// The manifest's pins in this set, name → version.
    pub requested: BTreeMap<String, String>,
    /// npm's lock (`package-lock.json` v3's `packages`), as npm wrote it.
    pub packages: Map<String, Json>,
}

impl Lock {
    const VERSION: u32 = 1;

    /// The lock beside the manifest, if there is one (strictly read).
    pub fn read(path: &Path) -> Result<Option<Lock>, BoxError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{}: {e}", path.display()).into()),
        };
        let lock: Lock =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        if lock.lock_version != Lock::VERSION {
            return Err(format!(
                "{}: lockVersion {}, which this yobuild does not read (it writes {})",
                path.display(),
                lock.lock_version,
                Lock::VERSION
            )
            .into());
        }
        if let Some(name) = lock.sets.keys().find(|n| Set::named(n).is_none()) {
            return Err(format!(
                "{}: no set is named {name:?} (pi, dsh, mcp)",
                path.display()
            )
            .into());
        }
        Ok(Some(lock))
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
pub struct Installed {
    pub pi: Option<PathBuf>,
    pub dsh: Option<PathBuf>,
    pub mcp: Option<PathBuf>,
    /// What changed in `agent.lock` (an Info notice each).
    pub said: Vec<String>,
}

impl Installed {
    pub fn get(&self, set: Set) -> Option<&Path> {
        match set {
            Set::Pi => self.pi.as_deref(),
            Set::Dsh => self.dsh.as_deref(),
            Set::Mcp => self.mcp.as_deref(),
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
pub fn canonical(value: &Json) -> String {
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

/// A set's folder name: the first 16 hex digits of a sha256 over its lock
/// entry's `base` and `packages` (canonical) and, for `dsh`, the adapter
/// files it copies.
pub fn hash(set: Set, entry: &SetLock, adapters: &Path) -> Result<String, BoxError> {
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

/// A set's folder in the cache.
pub fn folder(
    home: &Path,
    set: Set,
    entry: &SetLock,
    adapters: &Path,
) -> Result<PathBuf, BoxError> {
    Ok(home
        .join("plugins")
        .join(set.name())
        .join(hash(set, entry, adapters)?))
}

/// The settings that decide npm's result, on its command line (they win
/// over every config file and variable). Not `--omit=`: npm warns that it
/// is invalid; `include` wins over an `omit` set anywhere.
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

/// The same settings, for anyone running npm in the folder by hand.
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

/// Runs npm in `dir`; its stdout. A failure carries the end of its stderr.
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
        if let Some(key) = key.to_str() {
            if !npm_variable_kept(key) {
                command.env_remove(key);
            }
        }
    }
    let mut child = command.spawn().map_err(|e| failed(&e))?;
    let drain = |mut from: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = from.read_to_end(&mut bytes);
            bytes
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
    let stdout = String::from_utf8_lossy(&stdout.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&stderr.join().unwrap_or_default()).into_owned();
    if !status.success() {
        return Err(failed(&npm_error(&stderr)).into());
    }
    if !stderr.trim().is_empty() {
        tracing::debug!("npm {} in {}: {}", args[0], dir.display(), stderr.trim());
    }
    Ok(stdout)
}

/// What npm said went wrong: its message up to the usage text it prints
/// after a misuse (`npm error Usage:` and the option list), at most 20 lines;
/// the whole log stays in npm's own log file, which it names last.
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
        let view: Json =
            serde_json::from_str(&view).map_err(|e| format!("npm view {spec}: {e}"))?;
        let declared = match &view["peerDependencies"] {
            Json::Null => continue,
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
    }
    let mut deps = Map::new();
    for (name, version) in pins.iter().chain(peers.iter()) {
        deps.insert(name.clone(), json!(version));
    }
    Ok(deps)
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
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()).into())
}

/// Resolves a set anew in a temp folder: its lock entry (nothing unpacked).
fn resolve(
    set: Set,
    pins: &BTreeMap<String, String>,
    adapters: &Path,
) -> Result<SetLock, BoxError> {
    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join(".npmrc"), npmrc(set))?;
    let (dependencies, base) = match set {
        Set::Pi => (pi_dependencies(dir.path(), pins)?, None),
        Set::Mcp => (
            pins.iter().map(|(n, v)| (n.clone(), json!(v))).collect(),
            None,
        ),
        Set::Dsh => {
            // The base's lock as the start: DSH's packages stay as pinned.
            std::fs::copy(
                adapters.join("dsh/package-lock.json"),
                dir.path().join("package-lock.json"),
            )
            .map_err(|e| format!("{}: {e}", adapters.join("dsh/package-lock.json").display()))?;
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
        let version = npm(dir.path(), &["--version".to_owned()]).unwrap_or_default();
        return Err(format!(
            "npm {} wrote an incomplete lock for the `{}` set (another npm would refuse to install it): {}; upgrade npm (11.19 resolves it whole)",
            version.trim(),
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
                // `a/node_modules/b` looks in `a/node_modules/b/node_modules`,
                // then `a/node_modules`, then the root's.
                let mut at = path.as_str();
                let found = loop {
                    let candidate = match at.is_empty() {
                        true => format!("node_modules/{dep}"),
                        false => format!("{at}/node_modules/{dep}"),
                    };
                    if packages.contains_key(&candidate) {
                        break true;
                    }
                    if at.is_empty() {
                        break false;
                    }
                    at = match at.rfind("/node_modules/") {
                        Some(i) => &at[..i],
                        None => "",
                    };
                };
                if !found {
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
    let stamped =
        |dir: &Path| std::fs::read_to_string(dir.join(STAMP)).is_ok_and(|s| s.trim() == hash);
    if stamped(&dir) {
        return Ok(dir);
    }
    std::fs::create_dir_all(&parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    remove_old_partials(&parent);
    if dir.exists() {
        // Changed by hand (or not ours): made again.
        tracing::warn!(
            "{}: no stamp or another hash; installing it again",
            dir.display()
        );
        std::fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let partial = parent.join(format!("{hash}.partial-{}", uuid::Uuid::new_v4().simple()));
    let made = make(&partial, set, entry, adapters, &hash);
    if let Err(e) = made {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&partial, &dir) {
        let _ = std::fs::remove_dir_all(&partial);
        // Another yobuild made it first: the same lock, the same content.
        if !stamped(&dir) {
            return Err(format!("{}: {e}", dir.display()).into());
        }
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
    std::fs::write(dir.join(".npmrc"), npmrc(set))?;
    if set == Set::Dsh {
        for name in DSH_FILES {
            let from = adapters.join("dsh").join(name);
            std::fs::copy(&from, dir.join(name)).map_err(|e| format!("{}: {e}", from.display()))?;
        }
    }
    let mut args = vec!["ci".to_owned(), "--prefer-offline".to_owned()];
    args.extend(npm_flags(set));
    npm(dir, &args)?;
    std::fs::write(dir.join(STAMP), format!("{hash}\n"))?;
    Ok(())
}

/// Removes `.partial-*` folders an install that died left (an hour old).
fn remove_old_partials(parent: &Path) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > PARTIAL_AGE);
        if old && entry.file_name().to_string_lossy().contains(".partial-") {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Installs the manifest's pinned packages: `agent.lock` read, a missing or
/// stale set resolved again (refused with `locked`) and the lock rewritten,
/// then each set installed into the cache under `home` (`None`: [`home()`],
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
    let mut lock = old.clone().unwrap_or(Lock {
        lock_version: Lock::VERSION,
        sets: BTreeMap::new(),
    });
    let mut installed = Installed::default();
    let base = match pins.contains_key(&Set::Dsh) {
        true => Some(base_checksum(adapters)?),
        false => None,
    };
    // Sets the manifest no longer needs.
    let unneeded: Vec<String> = lock
        .sets
        .keys()
        .filter(|name| Set::named(name).is_none_or(|s| !pins.contains_key(&s)))
        .cloned()
        .collect();
    for name in unneeded {
        if locked {
            return Err(format!(
                "{}: its `{name}` set is no longer needed (--locked: run without it to update the lock)",
                path.display()
            )
            .into());
        }
        lock.sets.remove(&name);
        installed.said.push(format!(
            "{}: `{name}` removed (no pins left)",
            file_name(&path)
        ));
    }
    for (set, wanted) in &pins {
        let current = lock
            .sets
            .get(set.name())
            .filter(|entry| entry.requested == *wanted && (*set != Set::Dsh || entry.base == base));
        if current.is_none() {
            if locked {
                let why = match lock.sets.contains_key(set.name()) {
                    true => "is not current: the manifest's pins or the DSH base changed",
                    false => "is missing",
                };
                return Err(format!(
                    "{}: its `{}` set {why} (--locked: run without it to resolve the set again)",
                    path.display(),
                    set.name()
                )
                .into());
            }
            let entry = resolve(*set, wanted, adapters)
                .map_err(|e| format!("resolving the `{}` set: {e}", set.name()))?;
            let previous = lock.sets.get(set.name());
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
            lock.sets.insert(set.name().to_owned(), entry);
        }
    }
    if old.as_ref() != Some(&lock) && !(old.is_none() && lock.sets.is_empty()) {
        lock.write(&path)?;
    }
    for set in pins.keys() {
        let entry = &lock.sets[set.name()];
        let dir = install_set(&home, *set, entry, adapters)
            .map_err(|e| format!("installing the `{}` set: {e}", set.name()))?;
        match set {
            Set::Pi => installed.pi = Some(dir),
            Set::Dsh => installed.dsh = Some(dir),
            Set::Mcp => installed.mcp = Some(dir),
        }
    }
    Ok(installed)
}

/// For tests: a set installed by hand, with a lock beside `manifest_path`
/// that holds it (through the same hash, so a run finds it current and
/// installed, and never calls npm). `packages`: each package's name,
/// version and the folder copied into `node_modules/`. Not for `dsh`.
#[doc(hidden)]
pub fn seed(
    home: &Path,
    manifest_path: &Path,
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
    let dir = folder(home, set, &entry, Path::new(""))?;
    for (name, _, from) in packages {
        copy_tree(from, &dir.join("node_modules").join(name))?;
    }
    std::fs::write(
        dir.join(STAMP),
        format!("{}\n", hash(set, &entry, Path::new(""))?),
    )?;
    let lock_path = manifest_path.with_extension("lock");
    let mut lock = Lock::read(&lock_path)?.unwrap_or(Lock {
        lock_version: Lock::VERSION,
        sets: BTreeMap::new(),
    });
    lock.sets.insert(set.name().to_owned(), entry);
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
        std::fs::write(
            &path,
            r#"{"lockVersion":1,"sets":{"go":{"requested":{},"packages":{}}}}"#,
        )
        .unwrap();
        assert!(Lock::read(&path)
            .unwrap_err()
            .to_string()
            .contains("\"go\""));
        std::fs::write(&path, r#"{"lockVersion":1,"sets":{},"extra":1}"#).unwrap();
        assert!(Lock::read(&path).is_err());
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
        assert!(installed.said.is_empty() && installed.pi.is_none());
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
        assert_eq!(installed.said, ["agent.lock: `pi` removed (no pins left)"]);
        assert!(!dir.path().join("agent.lock").exists());
    }

    #[test]
    fn an_empty_lock_removes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.lock");
        std::fs::write(&path, "{}").unwrap();
        Lock {
            lock_version: 1,
            sets: BTreeMap::new(),
        }
        .write(&path)
        .unwrap();
        assert!(!path.exists());
    }
}
