//! A manifest's plugins as plugin-host rows, by ecosystem — checked before
//! anything starts. A pinned package comes from its set in the cache
//! (`crate::install`): a pi package from the `pi` set, a DSH package from
//! the `dsh` set, where the `dsh` runtime then runs; DSH files run in
//! `adapters/dsh`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::{json, Value as Json};

use super::Layout;
use crate::frontend::host::{rows_from_file, Row};
use crate::install::{Installed, Set};
use crate::manifest::{Manifest, Package, Plugin, PluginKind, Runtime, Source};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The package a rutis plugin's Node runtime runs (in the manifest's
/// folder), at the version yobuild's own runtimes use.
const RUTIS_RUNTIME: &str = "@arcships/rutis-runtime";

/// The rows to load, the runtimes they need, and the handlers to wait for.
#[derive(Debug, Default)]
pub(crate) struct Plan {
    pub rows: Vec<Row>,
    /// Start the `pi` runtime (`adapters/pi`).
    pub pi: bool,
    /// Start the `dsh` runtime, in `dsh_dir`.
    pub dsh: bool,
    /// The `dsh` runtime's folder: the `dsh` set's when DSH packages are
    /// pinned, else `adapters/dsh`; the DSH adapter's files it loads are
    /// there.
    pub dsh_dir: PathBuf,
    /// The rutis plugins' packages, each a Node runtime of its own
    /// (`rutis-<index>`): a plugin runs in its package's, where its imports
    /// resolve (and its Cordis: rutis refuses a plugin that resolves
    /// another than its runtime's).
    pub rutis: Vec<PathBuf>,
    /// Handlers the agent depends on (a TypeScript row runs before its
    /// async start-up is done).
    pub handlers: Vec<String>,
    /// The pi and DSH adapters' handler names: per process, unguessable, so
    /// no plugin can register one first and pass for that adapter (handler
    /// names are unique registry-wide; the first to register wins).
    pub pi_handler: String,
    pub dsh_handler: String,
    /// What loads only in part, or is set and has no effect: said before the
    /// agent starts.
    pub notes: Vec<String>,
    /// The modules of the DSH plugins' rows: the services they inject are
    /// checked once the runtime runs.
    pub dsh_modules: Vec<String>,
    /// DSH packages the plugins list as peers and no row loads: what a
    /// missing service may need.
    pub dsh_peers: Vec<String>,
    /// Each pi plugin's extension files, canonical: a tool it allows runs
    /// without asking only when one of them registered it.
    pub pi_files: Vec<(String, Vec<PathBuf>)>,
}

/// The folder of an installed package, at exactly `package`'s version;
/// `install` is what to do when it is not.
pub(crate) fn installed(
    package: &Package,
    node_modules: &Path,
    install: &str,
) -> Result<PathBuf, BoxError> {
    let dir = node_modules.join(package.name());
    let manifest = dir.join("package.json");
    let text = match std::fs::read_to_string(&manifest) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "{} is not installed in {} ({install})",
                package.name(),
                node_modules.display()
            )
            .into())
        }
        Err(e) => return Err(format!("{}: {e}", manifest.display()).into()),
    };
    let version = serde_json::from_str::<Json>(&text)
        .map_err(|e| format!("{}: {e}", manifest.display()))?["version"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("{}: no version", manifest.display()))?;
    if version != package.version() {
        return Err(format!(
            "{} is installed at {version} in {}, not {} ({install})",
            package.name(),
            node_modules.display(),
            package.version()
        )
        .into());
    }
    Ok(dir)
}

/// A pinned package in its installed set, at exactly its version.
pub(crate) fn in_set(sets: &Installed, set: Set, package: &Package) -> Result<PathBuf, BoxError> {
    let dir = sets.get(set).ok_or_else(|| {
        format!(
            "{} is pinned, but the `{}` set was not installed",
            package.spec(),
            set.name()
        )
    })?;
    let fix = format!(
        "the cache folder {} is not what the lock says: delete it and run again",
        dir.display()
    );
    installed(package, &dir.join("node_modules"), &fix)
}

/// How long a helper script may take (it reads package files and imports
/// a plugin's modules).
const HELPER_TIME: std::time::Duration = std::time::Duration::from_secs(60);

/// Runs one of the adapters' helper scripts with Node; its JSON output, an
/// object (its stderr goes to the debug log).
fn node_json(script: &Path, args: &[&Path]) -> Result<Helper, BoxError> {
    use std::io::Read;
    let failed = |e: &dyn std::fmt::Display| format!("node {}: {e}", script.display());
    let mut child = std::process::Command::new("node")
        .arg(script)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| failed(&e))?;
    let read = |mut from: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = from.read_to_end(&mut bytes);
            bytes
        })
    };
    let stdout = read(Box::new(child.stdout.take().expect("piped")));
    let stderr = read(Box::new(child.stderr.take().expect("piped")));
    let deadline = std::time::Instant::now() + HELPER_TIME;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| failed(&e))? {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(failed(&format!("no answer in {} s", HELPER_TIME.as_secs())).into());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let stdout = stdout.join().unwrap_or_default();
    let stderr = String::from_utf8_lossy(&stderr.join().unwrap_or_default()).into_owned();
    if !status.success() {
        return Err(failed(&stderr.trim()).into());
    }
    if !stderr.trim().is_empty() {
        tracing::debug!("node {}: {}", script.display(), stderr.trim());
    }
    let text = String::from_utf8_lossy(&stdout);
    let head: String = text.chars().take(200).collect();
    match serde_json::from_slice(&stdout) {
        Ok(Json::Object(fields)) => Ok(Helper {
            script: script.display().to_string(),
            fields,
        }),
        Ok(_) => Err(failed(&format!("not an object: {head:?}")).into()),
        Err(e) => Err(failed(&format!("{e} in {head:?}")).into()),
    }
}

/// A helper script's answer, read by key: a key missing or of another kind
/// is an error naming the script (never read as "nothing there").
struct Helper {
    script: String,
    fields: serde_json::Map<String, Json>,
}

impl Helper {
    fn field(&self, key: &str) -> Result<&Json, BoxError> {
        self.fields
            .get(key)
            .ok_or_else(|| format!("node {}: no {key:?} in its answer", self.script).into())
    }

    fn list(&self, key: &str) -> Result<&Vec<Json>, BoxError> {
        self.field(key)?
            .as_array()
            .ok_or_else(|| format!("node {}: {key:?} is not a list", self.script).into())
    }

    fn map(&self, key: &str) -> Result<&serde_json::Map<String, Json>, BoxError> {
        self.field(key)?
            .as_object()
            .ok_or_else(|| format!("node {}: {key:?} is not an object", self.script).into())
    }

    fn texts(&self, key: &str) -> Result<Vec<String>, BoxError> {
        self.list(key)?
            .iter()
            .map(|v| {
                v.as_str().map(str::to_owned).ok_or_else(|| -> BoxError {
                    format!("node {}: {key:?} holds {v}, not text", self.script).into()
                })
            })
            .collect()
    }
}

/// What the manifest's plugins load. Every refusal about their files and
/// packages comes from here, before anything starts (what a running DSH
/// runtime lacks is checked once it runs: `run::dsh_services`).
pub(crate) fn plan(
    manifest: &Manifest,
    layout: &Layout,
    sets: &Installed,
    cwd: &Path,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<Plan, BoxError> {
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let mut plan = Plan {
        pi_handler: format!("pi-extensions-{suffix}"),
        dsh_handler: format!("dsh-tools-{suffix}"),
        dsh_dir: sets
            .get(Set::Dsh)
            .map_or_else(|| layout.adapters.join("dsh"), Path::to_path_buf),
        ..Plan::default()
    };
    let mut extensions: Vec<Json> = Vec::new();
    let mut dsh_rows: Vec<Row> = Vec::new();
    for plugin in &manifest.plugins {
        let failed = |e: BoxError| -> BoxError { format!("plugin {}: {e}", plugin.name).into() };
        let config = plugin.config_with(lookup).map_err(|e| failed(e.into()))?;
        match &plugin.kind {
            PluginKind::Pi(source) => {
                if config.is_some() {
                    return Err(failed("a pi extension takes no config".into()));
                }
                let found = pi_extensions(layout, sets, plugin, source).map_err(failed)?;
                let files = found
                    .extensions
                    .iter()
                    .filter_map(|e| e.as_str())
                    .map(|e| std::fs::canonicalize(e).unwrap_or_else(|_| PathBuf::from(e)))
                    .collect();
                plan.pi_files.push((plugin.name.clone(), files));
                extensions.extend(found.extensions);
                plan.notes.extend(found.notes);
            }
            PluginKind::Dsh(source) => {
                let (rows, peers) =
                    dsh_rows_of(layout, &plan.dsh_dir, plugin, source, config).map_err(failed)?;
                plan.dsh_modules
                    .extend(rows.iter().map(|row| row.name().to_owned()));
                plan.dsh_peers.extend(peers);
                dsh_rows.extend(rows);
            }
            PluginKind::Rutis { path, runtime } => {
                if *runtime != Runtime::Node {
                    return Err(failed(
                        format!("yobuild run starts Node runtimes only, for now ({runtime:?})")
                            .into(),
                    ));
                }
                exists(path).map_err(failed)?;
                let package = package_of(path).map_err(failed)?;
                let at = match plan.rutis.iter().position(|p| *p == package) {
                    Some(at) => at,
                    None => {
                        rutis_runtime(&package, layout).map_err(failed)?;
                        plan.rutis.push(package);
                        plan.rutis.len() - 1
                    }
                };
                let mut row =
                    Row::new(row_id(&plugin.name), path.display()).runtime(rutis_runtime_name(at));
                if let Some(config) = config {
                    row = row.config(Json::Object(config));
                }
                plan.rows.push(row);
            }
        }
    }
    if !extensions.is_empty() {
        let pi = layout.adapters.join("pi");
        plan.pi = true;
        plan.handlers.push(plan.pi_handler.clone());
        plan.rows.push(
            Row::new("pi", pi.join("pi-extensions-adapter.ts").display())
                .runtime("pi")
                // `own-extension`: a pi tool's own calls to other tools
                // (`ctx.executeTool`) reach only its extension's tools, so
                // allowing it does not allow the others' (design §2).
                .config(json!({
                    "extensions": extensions,
                    "cwd": cwd,
                    "name": plan.pi_handler,
                    "nestedCalls": "own-extension",
                })),
        );
        plan.rows
            .push(Row::new("pi-host-ui", pi.join("host-ui.ts").display()).runtime("pi"));
    }
    if dsh_rows.is_empty() {
        if manifest.dsh.host_services {
            plan.notes
                .push("[dsh] host_services has no effect: no DSH plugin".into());
        }
    } else {
        let dsh = plan.dsh_dir.clone();
        plan.dsh = true;
        plan.handlers.push(plan.dsh_handler.clone());
        if manifest.dsh.host_services {
            // Read in place: it names packages, which resolve from `dsh`.
            plan.rows.extend(rows_from_file(
                &layout.adapters.join("dsh/host-services.json"),
                "dsh",
            )?);
        }
        let base = |id: &str, name: &str| Row::new(id, name).runtime("dsh");
        plan.rows.extend([
            base("dsh-web", "@deepseek-ai/dsh-web"),
            base("dsh-system-prompt", "@deepseek-ai/dsh-system-prompt"),
            base("dsh-tools", "@deepseek-ai/dsh-tools"),
        ]);
        plan.rows.extend(dsh_rows);
        plan.rows.extend([
            Row::new("dsh", dsh.join("dsh-tools-adapter.ts").display())
                .runtime("dsh")
                .config(json!({ "name": plan.dsh_handler })),
            Row::new("dsh-host-dialogs", dsh.join("host-dialogs.ts").display()).runtime("dsh"),
        ]);
    }
    let loaded: HashSet<&str> = plan.rows.iter().map(Row::name).collect();
    plan.dsh_peers
        .retain(|peer| !loaded.contains(peer.as_str()));
    plan.dsh_peers.sort();
    plan.dsh_peers.dedup();
    // The manifest's rows are `plugin:…`, yobuild's are not; and a bundle
    // could still repeat a row.
    unique(&plan.rows)?;
    Ok(plan)
}

/// No two rows of one id (one would replace the other).
fn unique(rows: &[Row]) -> Result<(), BoxError> {
    let mut ids = HashSet::new();
    for row in rows {
        if !ids.insert(row.id()) {
            return Err(format!("two plugin rows named {}", row.id()).into());
        }
    }
    Ok(())
}

fn exists(path: &Path) -> Result<(), BoxError> {
    match path.try_exists() {
        Ok(true) => Ok(()),
        Ok(false) => Err(format!("{} does not exist", path.display()).into()),
        Err(e) => Err(format!("{}: {e}", path.display()).into()),
    }
}

/// A pi plugin's extensions, and what it ships that yobuild does not load.
struct PiFound {
    extensions: Vec<Json>,
    notes: Vec<String>,
}

fn pi_extensions(
    layout: &Layout,
    sets: &Installed,
    plugin: &Plugin,
    source: &Source,
) -> Result<PiFound, BoxError> {
    let dir = match source {
        Source::Package(p) => in_set(sets, Set::Pi, p)?,
        Source::Path(path) => {
            exists(path)?;
            path.clone()
        }
    };
    if dir.is_file() {
        return Ok(PiFound {
            extensions: vec![json!(dir)],
            notes: Vec::new(),
        });
    }
    // What pi's package manager finds in it.
    let found = node_json(&layout.adapters.join("pi/resolve-package.mjs"), &[&dir])?;
    let extensions = found.list("extensions")?.clone();
    if extensions.is_empty() {
        return Err(format!("no pi extension in {}", dir.display()).into());
    }
    let skipped: Vec<String> = ["skills", "prompts", "themes"]
        .into_iter()
        .map(|key| Ok((key, found.list(key)?.len())))
        .collect::<Result<Vec<_>, BoxError>>()?
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(key, n)| format!("{n} {key}"))
        .collect();
    let notes = match skipped.is_empty() {
        true => Vec::new(),
        false => vec![format!(
            "plugin {}: its {} are not loaded (yobuild loads pi extensions only)",
            plugin.name,
            skipped.join(", ")
        )],
    };
    Ok(PiFound { extensions, notes })
}

/// A manifest plugin's row id, `plugin:<name>` (a DSH package's rows add
/// `:<row id>`): the owner the host records for what the row registers.
pub(crate) fn row_id(name: &str) -> String {
    format!("plugin:{name}")
}

/// The manifest plugin a row belongs to: `plugin:<name>` and
/// `plugin:<name>:<row id>` are `<name>`'s (names hold no `:`); else `None`.
pub(crate) fn plugin_of(row: &str) -> Option<&str> {
    row.strip_prefix("plugin:")?.split(':').next()
}

/// The ids of a DSH plugin's rows in `plan`: `plugin:<name>`, or each
/// `plugin:<name>:<row id>` (names hold no `:`, so no other plugin's).
pub(crate) fn dsh_plugin_rows(plan: &Plan, name: &str) -> Vec<String> {
    let id = row_id(name);
    plan.rows
        .iter()
        .filter(|row| row.runtime_is("dsh"))
        .map(|row| row.id())
        .filter(|row| {
            *row == id
                || row
                    .strip_prefix(id.as_str())
                    .is_some_and(|rest| rest.starts_with(':'))
        })
        .map(str::to_owned)
        .collect()
}

/// A DSH plugin's rows — a file is one row; a package, the rows its bundle
/// patch inserts (as DSH installs it), each `plugin:<name>:<row id>`, the
/// plugin's config replacing its first row's — and its peers.
fn dsh_rows_of(
    layout: &Layout,
    base: &Path,
    plugin: &Plugin,
    source: &Source,
    config: Option<serde_json::Map<String, Json>>,
) -> Result<(Vec<Row>, Vec<String>), BoxError> {
    let id = row_id(&plugin.name);
    let p = match source {
        Source::Path(path) => {
            exists(path)?;
            let mut row = Row::new(id, path.display()).runtime("dsh");
            if let Some(config) = config {
                row = row.config(Json::Object(config));
            }
            return Ok((vec![row], Vec::new()));
        }
        Source::Package(p) => p,
    };
    let modules = base.join("node_modules");
    let fix = format!(
        "the cache folder {} is not what the lock says: delete it and run again",
        base.display()
    );
    let dir = installed(p, &modules, &fix)?;
    let inspected = node_json(
        &layout.adapters.join("dsh/inspect-package.mjs"),
        &[base, Path::new(p.name())],
    )?;
    rows_of_bundle(&id, &inspected, config, &modules, &optional_peers(&dir)?)
}

/// The peers a package marks optional (`peerDependenciesMeta`): it works
/// without them (a DSH package's browser-client peers, say).
fn optional_peers(dir: &Path) -> Result<HashSet<String>, BoxError> {
    let path = dir.join("package.json");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let manifest: Json =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let meta = match &manifest["peerDependenciesMeta"] {
        Json::Null => return Ok(HashSet::new()),
        Json::Object(meta) => meta,
        _ => {
            return Err(format!("{}: peerDependenciesMeta is not an object", path.display()).into())
        }
    };
    let mut optional = HashSet::new();
    for (name, entry) in meta {
        match entry.get("optional") {
            None | Some(Json::Bool(false)) => {}
            Some(Json::Bool(true)) => {
                optional.insert(name.clone());
            }
            Some(other) => {
                return Err(format!(
                    "{}: peerDependenciesMeta {name}: optional is {other}, not true or false",
                    path.display()
                )
                .into())
            }
        }
    }
    Ok(optional)
}

/// The rows of what DSH would install (`inspected`, from
/// `inspect-package.mjs`), each `<id>:<row id>`, `config` replacing the
/// first row's own, and the package's peers (each installed in `modules`).
fn rows_of_bundle(
    id: &str,
    inspected: &Helper,
    config: Option<serde_json::Map<String, Json>>,
    modules: &Path,
    optional: &HashSet<String>,
) -> Result<(Vec<Row>, Vec<String>), BoxError> {
    let errors = inspected.map("importErrors")?;
    if !errors.is_empty() {
        return Err(format!(
            "its rows cannot be imported: {}",
            Json::Object(errors.clone())
        )
        .into());
    }
    let ops = inspected.texts("otherPatchOps")?;
    if !ops.is_empty() {
        return Err(format!(
            "its bundle patch does more than insert rows ({}), which yobuild does not apply",
            ops.join(", ")
        )
        .into());
    }
    let peers = inspected.texts("peers")?;
    for peer in peers.iter().filter(|peer| !optional.contains(*peer)) {
        if !modules.join(peer).join("package.json").is_file() {
            return Err(format!("it needs {peer}, not installed in {}", modules.display()).into());
        }
    }
    let rows = inspected.list("rows")?;
    if rows.is_empty() {
        return Err("no rows in what DSH would install".into());
    }
    let mut out = Vec::new();
    for (at, row) in rows.iter().enumerate() {
        let fields = row
            .as_object()
            .ok_or_else(|| format!("a row that is not an object: {row}"))?;
        if let Some(key) = fields
            .keys()
            .find(|k| !["id", "name", "config"].contains(&k.as_str()))
        {
            return Err(format!("a row with {key:?}, which yobuild does not apply: {row}").into());
        }
        let name = row["name"]
            .as_str()
            .ok_or_else(|| format!("a row without a name: {row}"))?;
        let row_id = match &row["id"] {
            Json::String(id) => id.clone(),
            Json::Null => at.to_string(),
            other => return Err(format!("a row whose id is not text: {other}").into()),
        };
        let mut built = Row::new(format!("{id}:{row_id}"), name).runtime("dsh");
        match (at, &config, row.get("config")) {
            (0, Some(config), _) => built = built.config(Json::Object(config.clone())),
            (_, _, Some(own)) if !own.is_null() => built = built.config(own.clone()),
            _ => {}
        }
        out.push(built);
    }
    Ok((out, peers))
}

/// The name of the runtime for the rutis plugins of package `at`
/// (`Plan::rutis`).
pub(crate) fn rutis_runtime_name(at: usize) -> String {
    format!("rutis-{at}")
}

/// The package a rutis plugin belongs to: the nearest folder above it with
/// a `package.json` (where Node resolves its imports) — above the file
/// itself, so `..` and symlinks resolved first, as Node does; one spelling
/// per package, so one runtime.
fn package_of(path: &Path) -> Result<PathBuf, BoxError> {
    let real = std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    real.parent()
        .into_iter()
        .flat_map(Path::ancestors)
        .find(|dir| dir.join("package.json").is_file())
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            format!(
                "a rutis plugin runs in the Node runtime of its package: no package.json above {}",
                path.display()
            )
            .into()
        })
}

/// A rutis plugin's Node runtime: `@arcships/rutis-runtime` installed in its
/// package (`dir`), at the version yobuild's own runtimes use.
fn rutis_runtime(dir: &Path, layout: &Layout) -> Result<(), BoxError> {
    let pins = layout.frontend.join("plugins/package.json");
    let text = std::fs::read_to_string(&pins).map_err(|e| format!("{}: {e}", pins.display()))?;
    let version = serde_json::from_str::<Json>(&text)
        .map_err(|e| format!("{}: {e}", pins.display()))?["dependencies"][RUTIS_RUNTIME]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("{}: no {RUTIS_RUNTIME}", pins.display()))?;
    let package = Package::new(RUTIS_RUNTIME, &version)?;
    let install = format!(
        "a rutis plugin runs in a Node runtime in its package: npm install {} in {}",
        package.spec(),
        dir.display()
    );
    installed(&package, &dir.join("node_modules"), &install)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(dir: &Path, text: &str) -> Manifest {
        Manifest::parse(
            &format!("[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n{text}"),
            &dir.join("a.toml"),
            dir,
        )
        .unwrap()
    }

    fn package(spec: &str) -> Package {
        match &manifest(Path::new("/"), &format!("[[plugin]]\npi = \"{spec}\"\n")).plugins[0].kind {
            PluginKind::Pi(Source::Package(p)) => p.clone(),
            _ => unreachable!(),
        }
    }

    fn plan_of(dir: &Path, text: &str) -> Result<Plan, String> {
        plan(
            &manifest(dir, text),
            &Layout::default(),
            &Installed::default(),
            dir,
            &|_: &str| None,
        )
        .map_err(|e| e.to_string())
    }

    #[test]
    fn a_package_must_be_installed_at_its_pinned_version() {
        let dir = tempfile::tempdir().unwrap();
        let modules = dir.path().join("node_modules");
        std::fs::create_dir_all(modules.join("@s/x")).unwrap();
        std::fs::write(modules.join("@s/x/package.json"), r#"{"version":"1.2.3"}"#).unwrap();
        assert_eq!(
            installed(&package("@s/x@1.2.3"), &modules, "do this").unwrap(),
            modules.join("@s/x")
        );
        let e = installed(&package("@s/x@1.2.4"), &modules, "do this")
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("installed at 1.2.3") && e.contains("not 1.2.4 (do this)"),
            "{e}"
        );
        let e = installed(&package("y@1.0.0"), &modules, "do this")
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("not installed") && e.contains("(do this)"),
            "{e}"
        );
        // A broken package.json is not "not installed".
        std::fs::write(modules.join("@s/x/package.json"), "{").unwrap();
        let e = installed(&package("@s/x@1.2.3"), &modules, "do this")
            .unwrap_err()
            .to_string();
        assert!(e.contains("package.json") && !e.contains("do this"), "{e}");
        std::fs::write(modules.join("@s/x/package.json"), "{}").unwrap();
        let e = installed(&package("@s/x@1.2.3"), &modules, "do this")
            .unwrap_err()
            .to_string();
        assert!(e.ends_with("no version"), "{e}");
    }

    #[test]
    fn refusals_name_the_plugin_and_need_no_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("p.ts");
        std::fs::write(&file, "").unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        let cases = [
            (
                String::from("[[plugin]]\nrutis = \"./p.ts\"\nruntime = \"bun\"\n"),
                "plugin p: yobuild run starts Node runtimes only",
            ),
            (
                "[[plugin]]\npi = \"./p.ts\"\nconfig = { a = 1 }\n".into(),
                "plugin p: a pi extension takes no config",
            ),
            (
                "[[plugin]]\npi = \"./gone.ts\"\n".into(),
                "plugin gone: ",
            ),
            (
                "[[plugin]]\nrutis = \"./p.ts\"\nruntime = \"node\"\n".into(),
                "a rutis plugin runs in a Node runtime in its package: npm install @arcships/rutis-runtime@",
            ),
        ];
        for (text, expected) in cases {
            let Err(e) = plan_of(dir.path(), &text) else {
                panic!("{text} is refused")
            };
            assert!(e.contains(expected), "{text}: {e}");
        }
        assert!(plan_of(dir.path(), "[[plugin]]\npi = \"./gone.ts\"\n")
            .unwrap_err()
            .ends_with("gone.ts does not exist"));
    }

    #[test]
    fn rows_are_the_manifests_own_and_runtimes_start_only_when_needed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pi.ts"), "").unwrap();
        std::fs::write(dir.path().join("dsh.ts"), "").unwrap();
        let none = plan_of(dir.path(), "").unwrap();
        assert!(!none.pi && !none.dsh && none.rutis.is_empty() && none.rows.is_empty());
        assert!(none.handlers.is_empty() && none.notes.is_empty());
        // A plugin named like yobuild's own rows does not replace them.
        let plan = plan_of(
            dir.path(),
            "[[plugin]]\nname = \"dsh\"\ndsh = \"./dsh.ts\"\nconfig = { a = 1 }\n[[plugin]]\nname = \"pi\"\npi = \"./pi.ts\"\n",
        )
        .unwrap();
        assert!(plan.pi && plan.dsh && plan.rutis.is_empty());
        // The adapters' handler names: their usual ones, made unguessable.
        assert_eq!(
            plan.handlers,
            [plan.pi_handler.clone(), plan.dsh_handler.clone()]
        );
        assert!(
            plan.pi_handler.starts_with("pi-extensions-")
                && plan.dsh_handler.starts_with("dsh-tools-")
        );
        let again = plan_of(dir.path(), "").unwrap();
        assert_ne!(again.pi_handler, plan.pi_handler);
        let ids: Vec<&str> = plan.rows.iter().map(Row::id).collect();
        assert!(ids.contains(&"plugin:dsh") && ids.contains(&"dsh") && ids.contains(&"pi"));
        let own = plan.rows.iter().find(|r| r.id() == "plugin:dsh").unwrap();
        assert_eq!(own.config_json(), &json!({ "a": 1 }));
        let notes = plan_of(dir.path(), "[dsh]\nhost_services = true\n")
            .unwrap()
            .notes;
        assert_eq!(notes, ["[dsh] host_services has no effect: no DSH plugin"]);
    }

    #[test]
    fn the_dsh_runtime_runs_in_the_dsh_set_for_packages_and_adapters_dsh_for_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("dsh.ts"), "").unwrap();
        let file = plan_of(dir.path(), "[[plugin]]\ndsh = \"./dsh.ts\"\n").unwrap();
        let adapters = Layout::default().adapters.join("dsh");
        assert_eq!(file.dsh_dir, adapters);
        let adapter_row = file.rows.iter().find(|r| r.id() == "dsh").unwrap();
        assert!(Path::new(adapter_row.name()).starts_with(&adapters));
        // A package is looked for in the set's folder (an empty one here).
        let set = tempfile::tempdir().unwrap();
        let e = plan(
            &manifest(dir.path(), "[[plugin]]\ndsh = \"dsh-free-search@0.8.1\"\n"),
            &Layout::default(),
            &Installed::with(Set::Dsh, set.path()),
            dir.path(),
            &|_: &str| None,
        )
        .unwrap_err()
        .to_string();
        assert!(
            e.contains(&format!(
                "is not installed in {}",
                set.path().join("node_modules").display()
            )) && e.contains("the cache folder"),
            "{e}"
        );
    }

    fn inspected(value: Json) -> Helper {
        match value {
            Json::Object(fields) => Helper {
                script: "inspect".into(),
                fields,
            },
            _ => unreachable!(),
        }
    }

    #[test]
    fn a_bundle_gives_its_rows_with_the_plugins_config_on_the_first() {
        let modules = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(modules.path().join("@d/peer")).unwrap();
        std::fs::write(modules.path().join("@d/peer/package.json"), "{}").unwrap();
        let bundle = |rows: Json| {
            inspected(json!({
                "importErrors": {}, "otherPatchOps": [], "peers": ["@d/peer"], "rows": rows,
            }))
        };
        let rows = json!([
            { "id": "a", "name": "@d/a", "config": { "own": 1 } },
            { "name": "@d/b", "config": { "own": 2 } },
        ]);
        let config = json!({ "mine": true }).as_object().cloned();
        let (built, peers) = rows_of_bundle(
            "plugin:p",
            &bundle(rows.clone()),
            config,
            modules.path(),
            &HashSet::new(),
        )
        .unwrap();
        assert_eq!(peers, ["@d/peer"]);
        let ids: Vec<&str> = built.iter().map(Row::id).collect();
        assert_eq!(ids, ["plugin:p:a", "plugin:p:1"]);
        assert_eq!(built[0].config_json(), &json!({ "mine": true }));
        assert_eq!(built[1].config_json(), &json!({ "own": 2 }));
        // Without the plugin's config, each row keeps its own.
        let (built, _) = rows_of_bundle(
            "plugin:p",
            &bundle(rows),
            None,
            modules.path(),
            &HashSet::new(),
        )
        .unwrap();
        assert_eq!(built[0].config_json(), &json!({ "own": 1 }));
    }

    #[test]
    fn a_bundle_yobuild_cannot_apply_whole_is_refused() {
        let modules = tempfile::tempdir().unwrap();
        let good_row = json!({ "id": "a", "name": "@d/a" });
        let cases = [
            (
                json!({ "importErrors": { "a": "boom" } }),
                "its rows cannot be imported",
            ),
            (
                json!({ "otherPatchOps": ["remove"] }),
                "does more than insert rows (remove)",
            ),
            (
                json!({ "peers": ["@d/gone"] }),
                "it needs @d/gone, not installed in",
            ),
            (json!({ "peers": [1] }), "\"peers\" holds 1, not text"),
            (json!({ "rows": [] }), "no rows in what DSH would install"),
            (
                json!({ "rows": [{ "id": "a", "name": "@d/a", "when": "x" }] }),
                "a row with \"when\"",
            ),
            (json!({ "rows": [{ "id": "a" }] }), "a row without a name"),
            (
                json!({ "rows": [{ "id": 3, "name": "@d/a" }] }),
                "a row whose id is not text",
            ),
            (json!({ "rows": "x" }), "\"rows\" is not a list"),
        ];
        for (change, expected) in cases {
            let mut value = json!({
                "importErrors": {}, "otherPatchOps": [], "peers": [], "rows": [good_row],
            });
            for (key, v) in change.as_object().unwrap() {
                value[key] = v.clone();
            }
            let e = rows_of_bundle(
                "plugin:p",
                &inspected(value),
                None,
                modules.path(),
                &HashSet::new(),
            )
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
            assert!(e.contains(expected), "{change}: {e}");
        }
        let missing = inspected(json!({ "otherPatchOps": [], "peers": [], "rows": [] }));
        let e = rows_of_bundle("plugin:p", &missing, None, modules.path(), &HashSet::new())
            .err()
            .unwrap()
            .to_string();
        assert_eq!(e, "node inspect: no \"importErrors\" in its answer");
    }

    #[test]
    fn an_optional_peer_need_not_be_installed() {
        let modules = tempfile::tempdir().unwrap();
        let value = json!({
            "importErrors": {}, "otherPatchOps": [], "peers": ["@d/ui"],
            "rows": [{ "id": "a", "name": "@d/a" }],
        });
        assert!(rows_of_bundle(
            "plugin:p",
            &inspected(value.clone()),
            None,
            modules.path(),
            &HashSet::new()
        )
        .is_err());
        let optional: HashSet<String> = ["@d/ui".to_owned()].into();
        let (_, peers) = rows_of_bundle(
            "plugin:p",
            &inspected(value),
            None,
            modules.path(),
            &optional,
        )
        .unwrap();
        assert_eq!(peers, ["@d/ui"]);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{ "peerDependenciesMeta": { "@d/ui": { "optional": true }, "@d/core": {} } }"#,
        )
        .unwrap();
        assert_eq!(
            optional_peers(dir.path()).unwrap(),
            ["@d/ui".to_owned()].into()
        );
        // Another shape is an error, not "none optional".
        for (meta, said) in [
            (r#""yes""#, "is not an object"),
            (
                r#"{ "@d/ui": { "optional": "true" } }"#,
                "optional is \"true\"",
            ),
        ] {
            let text = format!(r#"{{ "peerDependenciesMeta": {meta} }}"#);
            std::fs::write(dir.path().join("package.json"), text).unwrap();
            let e = optional_peers(dir.path()).unwrap_err().to_string();
            assert!(e.contains(said), "{e}");
        }
    }

    #[test]
    fn two_rows_of_one_id_are_refused() {
        let rows = [Row::new("plugin:a", "x"), Row::new("plugin:b", "y")];
        assert!(unique(&rows).is_ok());
        let rows = [Row::new("plugin:a", "x"), Row::new("plugin:a", "y")];
        assert_eq!(
            unique(&rows).unwrap_err().to_string(),
            "two plugin rows named plugin:a"
        );
    }

    #[test]
    fn a_rutis_plugin_runs_in_its_packages_runtime_one_per_package() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("p.ts"), "").unwrap();
        // No package above it: refused.
        let e = plan_of(
            dir.path(),
            "[[plugin]]\nrutis = \"./p.ts\"\nruntime = \"node\"\n",
        )
        .err()
        .unwrap();
        assert!(e.contains("no package.json above"), "{e}");
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        let pins = Layout::default().frontend.join("plugins/package.json");
        let pins: Json = serde_json::from_str(&std::fs::read_to_string(pins).unwrap()).unwrap();
        let version = pins["dependencies"][RUTIS_RUNTIME].as_str().unwrap();
        let runtime = dir.path().join("node_modules").join(RUTIS_RUNTIME);
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::write(
            runtime.join("package.json"),
            format!(r#"{{"version":"{version}"}}"#),
        )
        .unwrap();
        let plan = plan_of(
            dir.path(),
            "[[plugin]]\nrutis = \"./p.ts\"\nruntime = \"node\"\nconfig = { a = 1 }\n",
        )
        .unwrap();
        assert_eq!(plan.rutis, [dir.path().canonicalize().unwrap()]);
        // Two plugins of one package (one by another spelling) share its
        // runtime; another package's has its own.
        let other = dir.path().join("other");
        std::fs::create_dir_all(other.join("node_modules")).unwrap();
        copy_dir(
            &dir.path().join("node_modules"),
            &other.join("node_modules"),
        );
        std::fs::write(other.join("package.json"), "{}").unwrap();
        std::fs::write(other.join("q.ts"), "").unwrap();
        std::fs::write(dir.path().join("r.ts"), "").unwrap();
        let three = plan_of(
            dir.path(),
            "[[plugin]]\nrutis = \"./p.ts\"\nruntime = \"node\"\n\
             [[plugin]]\nrutis = \"./other/q.ts\"\nruntime = \"node\"\n\
             [[plugin]]\nrutis = \"./other/../r.ts\"\nruntime = \"node\"\n",
        )
        .unwrap();
        let canonical = |p: &Path| p.canonicalize().unwrap();
        assert_eq!(three.rutis, [canonical(dir.path()), canonical(&other)]);
        let runtimes: Vec<&str> = three
            .rows
            .iter()
            .map(|r| r.runtime_name().unwrap())
            .collect();
        assert_eq!(runtimes, ["rutis-0", "rutis-1", "rutis-0"]);
        // `..` is resolved before looking up: a folder the path names on
        // its way is not the plugin's package.
        // (The runtime taken away, so the refusal names the folder.)
        let outer = dir.path().join("outer");
        std::fs::remove_dir_all(dir.path().join("node_modules")).unwrap();
        std::fs::create_dir_all(outer.join("in")).unwrap();
        std::fs::write(outer.join("in/package.json"), "{}").unwrap();
        let moved = plan_of(
            &outer.join("in"),
            "[[plugin]]\nrutis = \"../../p.ts\"\nruntime = \"node\"\n",
        );
        let e = moved.err().unwrap();
        assert!(
            e.contains(&format!(
                "in {}",
                dir.path().canonicalize().unwrap().display()
            )),
            "{e}"
        );
        assert!(!plan.pi && !plan.dsh);
        assert_eq!(plan.rows.len(), 1);
        assert_eq!(plan.rows[0].id(), "plugin:p");
        assert_eq!(plan.rows[0].config_json(), &json!({ "a": 1 }));
    }

    #[cfg(unix)]
    #[test]
    fn a_pi_plugins_files_are_recorded_canonical() {
        // A manifest reached through a symlink: its pi file recorded as the
        // file itself (pi reports the path it resolved).
        let real = tempfile::tempdir().unwrap();
        std::fs::write(real.path().join("p.ts"), "").unwrap();
        let outer = tempfile::tempdir().unwrap();
        let link = outer.path().join("agent");
        std::os::unix::fs::symlink(real.path(), &link).unwrap();
        let plan = plan_of(&link, "[[plugin]]\npi = \"./p.ts\"\n").unwrap();
        let canonical = std::fs::canonicalize(real.path().join("p.ts")).unwrap();
        assert_eq!(plan.pi_files, [("p".to_owned(), vec![canonical])]);
    }

    #[test]
    fn a_dsh_plugins_rows_are_its_own_and_no_other_plugins() {
        let plan = Plan {
            rows: vec![
                Row::new("plugin:a", "x").runtime("dsh"),
                Row::new("plugin:a:x", "x").runtime("dsh"),
                Row::new("plugin:ab", "x").runtime("dsh"),
                Row::new("plugin:ab:y", "x").runtime("dsh"),
                // A rutis plugin's row, of another runtime.
                Row::new("plugin:a2", "x").runtime("rutis-0"),
                Row::new("dsh", "x").runtime("dsh"),
            ],
            ..Plan::default()
        };
        assert_eq!(dsh_plugin_rows(&plan, "a"), ["plugin:a", "plugin:a:x"]);
        assert_eq!(dsh_plugin_rows(&plan, "ab"), ["plugin:ab", "plugin:ab:y"]);
        assert!(dsh_plugin_rows(&plan, "a2").is_empty());
    }

    /// Copies a folder of files and folders (no links).
    fn copy_dir(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_dir(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }
}
