//! A manifest's plugins as plugin-host rows, by ecosystem — checked before
//! anything starts. A pinned package comes from its set in the cache
//! (`crate::install`): a pi package from the `pi` set, a DSH package from
//! the `dsh` set, where the `dsh` runtime then runs; DSH files run in
//! `adapters/dsh`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::{json, Value as Json};

use super::Layout;
use crate::frontend::host::{rows_from_file, rows_from_json, Row};
use crate::install::{Installed, Set};
use crate::manifest::{ExecGrant, Manifest, Package, Plugin, PluginKind, Runtime, Source};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The package a rutis plugin's Node runtime runs (in the manifest's
/// folder), at the version yobuild's own runtimes use.
const RUTIS_RUNTIME: &str = "@arcships/rutis-runtime";

/// The version yobuild's own runtimes run (`frontend/plugins/package.json`'s
/// pin, unit-tested equal), which a rutis plugin's package must install.
const RUTIS_RUNTIME_VERSION: &str = "0.12.0";

/// The rows re-providing `host_approval` and `host_session` to the pi
/// adapter (`pi-host-approval`, `pi-host-session`).
pub(crate) const PI_HOST_APPROVAL: &str = "pi-host-approval";
pub(crate) const PI_HOST_SESSION: &str = "pi-host-session";

/// The DSH host's session log (`dsh-host.json`'s row of this package): its
/// folder is given at start, in the run's `DshHome`. Found by its package,
/// as yoagent-rutis's `dsh_host_rows` finds it.
const DSH_SESSION_LOG: &str = "@deepseek-ai/dsh-session-persistence-jsonl";

/// The DSH host's rows from the `dsh` runtime's folder (yoagent-rutis's
/// `dsh_host_rows`): with DSH's own host services, all of
/// `host-services.json` with the host's in place of the rows of their ids;
/// without, only the host's — a grant never brings DSH's files, credentials
/// or storage services in. The session log's folder is set at start.
fn dsh_host_rows(dsh: &Path, services: bool) -> Result<Vec<Row>, BoxError> {
    // No folder here (`start` gives it): one that would be read as the
    // runtime's own is never written.
    let rows = yoagent_rutis::adapters::dsh_host_rows(dsh, Path::new(""))?;
    let rows = match services {
        true => rows,
        false => {
            let host = rows_from_file(&dsh.join("dsh-host.json"), "dsh")?;
            rows.into_iter()
                .filter(|row| host.iter().any(|h| row["id"] == h.id()))
                .collect()
        }
    };
    rows_from_json(json!(rows), "dsh")
        .map_err(|e| format!("{}: {e}", dsh.join("dsh-host.json").display()).into())
}

impl Plan {
    /// Each pi plugin's extension files, canonical and as planned: what
    /// names a pi extension's plugin when the adapter calls or logs.
    pub(crate) fn pi_names(&self) -> Vec<(String, Vec<PathBuf>)> {
        self.pi_files
            .iter()
            .chain(&self.pi_spelled)
            .map(|(name, files)| (name.clone(), files.clone()))
            .collect()
    }
}

/// The rows to load, the runtimes they need, and the handlers to wait for.
#[derive(Debug, Default)]
pub(crate) struct Plan {
    pub rows: Vec<Row>,
    /// Start the `pi` runtime (`adapters/pi`).
    pub pi: bool,
    /// Start the `dsh` runtime, in `dsh_dir`.
    pub dsh: bool,
    /// Load the DSH host (its stand-in agent; design §12, "Step 2"): the DSH
    /// plugins have a grant the host serves (`env` alone is none). Its rows
    /// are in `rows`.
    pub dsh_host: bool,
    /// The DSH host's session log row, by id: `start` gives it its folder
    /// (set whenever `dsh_host` is).
    pub dsh_session_log: Option<String>,
    /// DSH's own host services load (`[dsh] host_services`).
    pub dsh_services: bool,
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
    /// The same files as `extensions` spells them, which the pi adapter
    /// names back to the host services.
    pub pi_spelled: Vec<(String, Vec<PathBuf>)>,
    /// What each runtime's `env` grant passes (design §12, "Step 5"):
    /// names, or prefixes ending in `*`, by runtime name, for every runtime
    /// a plugin runs in (none granted: an empty list): kept in its
    /// environment.
    pub env: Vec<(String, Vec<String>)>,
    /// The first plugin of each runtime in `env`, by runtime name: the one
    /// [`Plan::keep`]'s refusal names beside the plugin that differs.
    pub keepers: Vec<(String, String)>,
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

/// `command`'s environment: on unix only what of `vars` (this process's)
/// the kept set holds; elsewhere every variable but `keys`.
fn kept_only(
    command: &mut std::process::Command,
    vars: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    keys: &[String],
) {
    if cfg!(unix) {
        command.env_clear();
        command.envs(
            vars.into_iter()
                .filter(|(name, _)| crate::environment::kept(name)),
        );
    } else {
        for key in keys {
            command.env_remove(key);
        }
    }
}

/// How long a helper script may take (it reads package files and imports
/// a plugin's modules).
const HELPER_TIME: std::time::Duration = std::time::Duration::from_secs(60);

/// Runs one of the adapters' helper scripts with Node; its JSON output, an
/// object (its stderr goes to the debug log). `inspect-package.mjs`
/// imports a plugin's code, so on unix it gets only the kept environment
/// (design §12 "Step 5"; no grants); elsewhere `keys` (the credentials
/// yoagent reads) are removed — Node on Windows needs variables the kept
/// set does not hold.
fn node_json(script: &Path, args: &[&Path], keys: &[String]) -> Result<Helper, BoxError> {
    use std::io::Read;
    let failed = |e: &dyn std::fmt::Display| format!("node {}: {e}", script.display());
    let mut command = std::process::Command::new("node");
    kept_only(&mut command, std::env::vars_os(), keys);
    let mut child = command
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

/// A plugin's `env` grant as a refusal says it, as the manifest's
/// (`manifest::env_differ`): "`a` has X, Y" or "`b` none".
fn said(plugin: &str, env: &[String]) -> String {
    match env {
        [] => format!("`{plugin}` none"),
        env => format!("`{plugin}` has {}", env.join(", ")),
    }
}

impl Plan {
    /// The runtime `runtime` keeps what `env` passes, granted to its plugin
    /// `plugin`.
    /// Each plugin of one runtime is granted the same (the manifest and
    /// `plan` refuse otherwise, naming the plugins); should those checks
    /// ever loosen, this refuses rather than let one plugin's grant decide
    /// its runtime's environment — naming the plugin that came first, in
    /// their words.
    fn keep(&mut self, runtime: &str, plugin: &str, env: &[String]) -> Result<(), BoxError> {
        match self.env.iter().find(|(r, _)| r == runtime) {
            Some((_, kept)) if kept != env => {
                let first = self
                    .keepers
                    .iter()
                    .find(|(r, _)| r == runtime)
                    .map_or("another", |(_, p)| p.as_str());
                Err(format!(
                    "it runs in one runtime with plugin {first}, whose environment is theirs together: give each the same `env` grant or none ({}, {})",
                    said(first, kept),
                    said(plugin, env)
                )
                .into())
            }
            Some(_) => Ok(()),
            None => {
                self.env.push((runtime.to_owned(), env.to_vec()));
                self.keepers.push((runtime.to_owned(), plugin.to_owned()));
                Ok(())
            }
        }
    }

    /// What the runtime `runtime` keeps beyond the kept set: its `env`
    /// grant's names and prefixes.
    pub fn kept(&self, runtime: &str) -> &[String] {
        self.env
            .iter()
            .find(|(r, _)| r == runtime)
            .map_or(&[], |(_, env)| env.as_slice())
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
    let keys = manifest.key_variables();
    let mut plan = Plan {
        pi_handler: format!("pi-extensions-{suffix}"),
        dsh_handler: format!("dsh-tools-{suffix}"),
        dsh_dir: sets
            .get(Set::Dsh)
            .map_or_else(|| layout.adapters.join("dsh"), Path::to_path_buf),
        ..Plan::default()
    };
    let mut extensions: Vec<Json> = Vec::new();
    // The pi plugins' grants (design §12, "Step 3"): the adapter's settings
    // by extension file, and whether it is told the model.
    let mut exec_for = serde_json::Map::new();
    let mut trusted_for = serde_json::Map::new();
    let mut wake_for = serde_json::Map::new();
    let mut dsh_rows: Vec<Row> = Vec::new();
    for plugin in &manifest.plugins {
        let failed = |e: BoxError| -> BoxError { format!("plugin {}: {e}", plugin.name).into() };
        let config = plugin.config_with(lookup).map_err(|e| failed(e.into()))?;
        match &plugin.kind {
            PluginKind::Pi(source) => {
                plan.keep("pi", &plugin.name, &plugin.grants.env)
                    .map_err(failed)?;
                if config.is_some() {
                    return Err(failed("a pi extension takes no config".into()));
                }
                let found = pi_extensions(layout, sets, plugin, source, &keys).map_err(failed)?;
                // Each file as planned (how the adapter names it back) and
                // canonical (how a tool's source is compared); one file is
                // one plugin's: two would have the adapter and yobuild apply
                // different grants.
                let (mut files, mut spelled) = (Vec::new(), Vec::new());
                for e in found.extensions.iter().filter_map(Json::as_str) {
                    let canonical = std::fs::canonicalize(e)
                        .map_err(|why| failed(format!("its extension {e}: {why}").into()))?;
                    let other = plan.pi_files.iter().find(|(_, fs)| fs.contains(&canonical));
                    if let Some((other, _)) = other {
                        return Err(failed(
                            format!("its extension {e} is plugin {other}'s too").into(),
                        ));
                    }
                    spelled.push(PathBuf::from(e));
                    files.push(canonical);
                }
                plan.pi_files.push((plugin.name.clone(), files));
                plan.pi_spelled.push((plugin.name.clone(), spelled));
                // Its grants, by each extension file as `extensions` spells
                // it (the adapter's keys resolve as its entries do).
                let g = &plugin.grants;
                for file in found.extensions.iter().filter_map(Json::as_str) {
                    match g.exec {
                        ExecGrant::Off => {}
                        ExecGrant::Gate => _ = exec_for.insert(file.to_owned(), json!("gate")),
                        ExecGrant::Free => _ = exec_for.insert(file.to_owned(), json!(true)),
                    }
                    if g.trust {
                        trusted_for.insert(file.to_owned(), json!(true));
                    }
                    if g.wake {
                        wake_for.insert(file.to_owned(), json!(true));
                    }
                }
                extensions.extend(found.extensions);
                plan.notes.extend(found.notes);
            }
            PluginKind::Dsh(source) => {
                plan.keep("dsh", &plugin.name, &plugin.grants.env)
                    .map_err(failed)?;
                let (rows, peers) =
                    dsh_rows_of(layout, &plan.dsh_dir, plugin, source, config, &keys)
                        .map_err(failed)?;
                plan.dsh_modules
                    .extend(rows.iter().map(|row| row.name().to_owned()));
                plan.dsh_peers.extend(peers);
                dsh_rows.extend(rows);
            }
            PluginKind::Rutis { path, runtime, .. } => {
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
                        rutis_runtime(&package).map_err(failed)?;
                        plan.rutis.push(package);
                        plan.rutis.len() - 1
                    }
                };
                // One runtime per package, its environment its plugins'
                // together: the same `env` grant on each.
                let first = manifest.plugins.iter().find(|p| match &p.kind {
                    PluginKind::Rutis { path, .. } => {
                        package_of(path).is_ok_and(|p| p == plan.rutis[at])
                    }
                    _ => false,
                });
                if let Some(first) = first.filter(|f| f.grants.env != plugin.grants.env) {
                    return Err(failed(
                        format!(
                            "it runs in one runtime with plugin {} (the package at {}), whose environment is theirs together: give each the same `env` grant or none ({})",
                            first.name,
                            plan.rutis[at].display(),
                            crate::manifest::env_differ(first, plugin)
                        )
                        .into(),
                    ));
                }
                plan.keep(&rutis_runtime_name(at), &plugin.name, &plugin.grants.env)
                    .map_err(failed)?;
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
        // `own-extension`: a pi tool's own calls to other tools
        // (`ctx.executeTool`) reach only its extension's tools, so allowing
        // it does not allow the others' (design §2).
        let mut config = json!({
            "extensions": extensions,
            "cwd": cwd,
            "name": plan.pi_handler,
            "nestedCalls": "own-extension",
        });
        let gated = exec_for.values().any(|g| g == "gate");
        let waking = !wake_for.is_empty();
        for (key, map) in [
            ("execFor", exec_for),
            ("projectTrustedFor", trusted_for),
            ("wakeFor", wake_for),
        ] {
            if !map.is_empty() {
                config[key] = Json::Object(map);
            }
        }
        // One model for every extension (the manifest's one rule).
        if manifest.pi_model() {
            config["model"] = super::host::model_info(manifest);
        }
        plan.rows.push(
            Row::new("pi", pi.join("pi-extensions-adapter.ts").display())
                .runtime("pi")
                .config(config),
        );
        plan.rows
            .push(Row::new("pi-host-ui", pi.join("host-ui.ts").display()).runtime("pi"));
        // The host services the adapter looks up, re-provided in its runtime
        // (rutis shows a host service only to a plugin injecting it): only
        // when a grant uses one.
        if gated {
            plan.rows.push(
                Row::new(PI_HOST_APPROVAL, pi.join("host-approval.ts").display()).runtime("pi"),
            );
        }
        if waking {
            plan.rows.push(
                Row::new(PI_HOST_SESSION, pi.join("host-session.ts").display()).runtime("pi"),
            );
        }
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
        plan.dsh_services = manifest.dsh.host_services;
        plan.dsh_host = manifest.dsh_grants().is_some();
        if plan.dsh_host {
            let rows = dsh_host_rows(&dsh, plan.dsh_services)?;
            let log = rows.iter().find(|row| row.name() == DSH_SESSION_LOG);
            let Some(log) = log else {
                return Err(format!(
                    "{}: the DSH host's session log ({DSH_SESSION_LOG}) is not among its rows",
                    dsh.join("dsh-host.json").display()
                )
                .into());
            };
            plan.dsh_session_log = Some(log.id().to_owned());
            plan.rows.extend(rows);
        } else if plan.dsh_services {
            // Beside the adapter: it names packages, which resolve from `dsh`.
            plan.rows
                .extend(rows_from_file(&dsh.join("host-services.json"), "dsh")?);
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
    keys: &[String],
) -> Result<PiFound, BoxError> {
    let dir = match source {
        Source::Package(p) => in_set(sets, Set::Pi, p)?,
        Source::Path(path) => {
            exists(path)?;
            path.clone()
        }
        Source::Builtin(name) => {
            return Err(format!("`yobuild:{name}` was not resolved to its file").into())
        }
    };
    if dir.is_file() {
        return Ok(PiFound {
            extensions: vec![json!(dir)],
            notes: Vec::new(),
        });
    }
    // What pi's package manager finds in it.
    let found = node_json(
        &layout.adapters.join("pi/resolve-package.mjs"),
        &[&dir],
        keys,
    )?;
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
    keys: &[String],
) -> Result<(Vec<Row>, Vec<String>), BoxError> {
    let id = row_id(&plugin.name);
    let p = match source {
        // Refused when read: yobuild has no built-in DSH plugin.
        Source::Builtin(name) => return Err(format!("`yobuild:{name}` is not a DSH plugin").into()),
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
        keys,
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
fn rutis_runtime(dir: &Path) -> Result<(), BoxError> {
    let package = Package::new(RUTIS_RUNTIME, RUTIS_RUNTIME_VERSION)?;
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

    /// The helpers import a package's code: on unix they get only the
    /// kept environment, a credential and an unrelated variable gone.
    #[cfg(unix)]
    #[test]
    fn the_helpers_get_only_the_kept_environment() {
        let mut command = std::process::Command::new("/usr/bin/env");
        let vars = [
            ("PATH", "/usr/bin:/bin"),
            ("LANG", "C"),
            ("OPENAI_API_KEY", "k"),
            ("UNRELATED", "u"),
        ]
        .map(|(k, v)| (k.into(), v.into()));
        kept_only(&mut command, vars, &["OPENAI_API_KEY".to_owned()]);
        let out = String::from_utf8(command.output().unwrap().stdout).unwrap();
        let mut lines: Vec<&str> = out.lines().collect();
        lines.sort();
        assert_eq!(lines, ["LANG=C", "PATH=/usr/bin:/bin"]);
    }

    #[test]
    fn a_runtime_whose_plugins_keep_different_env_grants_is_refused() {
        let mut plan = Plan::default();
        let key = |k: &str| vec![k.to_owned()];
        plan.keep("pi", "a", &key("OPENAI_API_KEY")).unwrap();
        plan.keep("pi", "b", &key("OPENAI_API_KEY")).unwrap();
        plan.keep("dsh", "d", &[]).unwrap();
        assert_eq!(
            plan.keep("pi", "c", &[]).unwrap_err().to_string(),
            "it runs in one runtime with plugin a, whose environment is theirs together: give each the same `env` grant or none (`a` has OPENAI_API_KEY, `c` none)"
        );
        assert!(plan.keep("dsh", "e", &key("GROQ_API_KEY")).is_err());
        assert_eq!(plan.kept("pi"), key("OPENAI_API_KEY"));
        assert_eq!(plan.kept("dsh"), [] as [String; 0]);
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
        // The constant is the frontend's own pin.
        assert_eq!(version, RUTIS_RUNTIME_VERSION);
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
        // One runtime's environment is its plugins' together: the same
        // `env` grant on the plugins of one package; another package's its own.
        let keys = |p: &str, q: &str, r: &str| {
            plan_of(
                dir.path(),
                &format!(
                    "[[plugin]]\nrutis = \"./p.ts\"\nruntime = \"node\"\ngrants = {{ env = [{p}] }}\n\
                     [[plugin]]\nrutis = \"./other/q.ts\"\nruntime = \"node\"\ngrants = {{ env = [{q}] }}\n\
                     [[plugin]]\nrutis = \"./r.ts\"\nruntime = \"node\"\ngrants = {{ env = [{r}] }}\n"
                ),
            )
        };
        let granted = keys(r#""API_KEY""#, "", r#""API_KEY""#).unwrap();
        assert_eq!(
            granted.env,
            [
                ("rutis-0".to_owned(), vec!["API_KEY".to_owned()]),
                ("rutis-1".to_owned(), vec![])
            ]
        );
        assert!(granted.kept("rutis-1").is_empty());
        let e = keys(r#""API_KEY""#, "", "").err().unwrap();
        assert_eq!(
            e,
            format!(
                "plugin r: it runs in one runtime with plugin p (the package at {}), whose environment is theirs together: give each the same `env` grant or none (`p` has API_KEY, `r` none)",
                canonical(dir.path()).display()
            )
        );
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
        // And as planned, which the adapter names back.
        assert_eq!(plan.pi_spelled, [("p".to_owned(), vec![link.join("p.ts")])]);
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
    /// The DSH host's rows from the adapters' lists: with the grants,
    /// `dsh-host.json`'s, and `host-services.json`'s only with DSH's host
    /// services (DSH's files, credentials, storage never through a grant);
    /// the session log's row known by its package.
    #[test]
    fn a_granted_dsh_plugin_loads_the_dsh_host_and_dshs_own_services_only_on_request() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("d.ts"), "").unwrap();
        let dsh = Layout::default().adapters.join("dsh");
        let ids = |file: &str| -> Vec<String> {
            rows_from_file(&dsh.join(file), "dsh")
                .unwrap()
                .iter()
                .map(|r| r.id().to_owned())
                .collect()
        };
        let (host, services) = (ids("dsh-host.json"), ids("host-services.json"));
        let loaded = |grants: &str, services: bool| {
            let plan = plan_of(
                dir.path(),
                &format!(
                    "[[plugin]]\ndsh = \"./d.ts\"\n{grants}[dsh]\nhost_services = {services}\n"
                ),
            )
            .unwrap();
            unique(&plan.rows).unwrap();
            plan
        };
        let has = |plan: &Plan, id: &str| plan.rows.iter().any(|r| r.id() == id);
        // Granted, without DSH's services: the host's rows, none of the others.
        let plan = loaded("grants = { workspace = true }\n", false);
        assert!(plan.dsh_host && !plan.dsh_services);
        assert!(host.iter().all(|id| has(&plan, id)), "{:?}", plan.rows);
        for id in services.iter().filter(|id| !host.contains(id)) {
            assert!(!has(&plan, id), "{id}: {:?}", plan.rows);
        }
        // The session log: one row, the jsonl backend, its folder not yet.
        let log = plan.dsh_session_log.clone().unwrap();
        let logs: Vec<&Row> = plan.rows.iter().filter(|r| r.id() == log).collect();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].name(), DSH_SESSION_LOG);
        // Granted, with them: all, the host's in place of their ids.
        let plan = loaded("grants = { workspace = true }\n", true);
        assert!(services.iter().chain(&host).all(|id| has(&plan, id)));
        let named: Vec<&str> = plan
            .rows
            .iter()
            .filter(|r| r.id() == log)
            .map(Row::name)
            .collect();
        assert_eq!(named, [DSH_SESSION_LOG]);
        // Not granted: no host; DSH's services only on request.
        let plan = loaded("", true);
        assert!(!plan.dsh_host && plan.dsh_session_log.is_none());
        assert!(!has(&plan, "dsh-host") && services.iter().all(|id| has(&plan, id)));
        let plan = loaded("", false);
        assert!(!has(&plan, "dsh-host") && !services.iter().any(|id| has(&plan, id)));
        // `env` alone: no host, the `dsh` runtime keeping them.
        let plan = loaded("grants = { env = [\"API_KEY\"] }\n", false);
        assert!(!plan.dsh_host && !has(&plan, "dsh-host"));
        assert_eq!(plan.kept("dsh"), ["API_KEY"]);
        assert!(plan.kept("pi").is_empty());
    }

    /// The pi plugins' grants become the adapter's settings, by each
    /// extension file as `extensions` spells it; the rows re-providing the
    /// host services load only when a grant uses them.
    #[test]
    fn a_pi_plugins_grants_become_the_adapters_settings_by_file() {
        let dir = tempfile::tempdir().unwrap();
        for f in ["a.ts", "b.ts", "c.ts"] {
            std::fs::write(dir.path().join(f), "").unwrap();
        }
        let file = |f: &str| dir.path().join(f).display().to_string();
        let pi = |plan: &Plan| -> Json {
            plan.rows
                .iter()
                .find(|r| r.id() == "pi")
                .unwrap()
                .config_json()
                .clone()
        };
        let ids =
            |plan: &Plan| -> Vec<String> { plan.rows.iter().map(|r| r.id().to_owned()).collect() };
        let plan = plan_of(
            dir.path(),
            "[[plugin]]\npi = \"./a.ts\"\ngrants = { exec = \"gate\", wake = true }\n\
             [[plugin]]\npi = \"./b.ts\"\ngrants = { exec = \"free\", trust = true }\n\
             [[plugin]]\npi = \"./c.ts\"\n",
        )
        .unwrap();
        let config = pi(&plan);
        assert_eq!(
            config["execFor"],
            json!({ file("a.ts"): "gate", file("b.ts"): true })
        );
        assert_eq!(config["projectTrustedFor"], json!({ file("b.ts"): true }));
        assert_eq!(config["wakeFor"], json!({ file("a.ts"): true }));
        assert!(config.get("model").is_none(), "{config}");
        let ids_ = ids(&plan);
        assert!(
            ids_.contains(&PI_HOST_APPROVAL.to_owned())
                && ids_.contains(&PI_HOST_SESSION.to_owned())
        );
        // `model` on all: the manifest's model, for every extension.
        let plan = plan_of(
            dir.path(),
            "[[plugin]]\npi = \"./a.ts\"\ngrants = { model = \"info\" }\n\
             [[plugin]]\npi = \"./b.ts\"\ngrants = { model = \"info\" }\n",
        )
        .unwrap();
        let m = manifest(dir.path(), "");
        assert_eq!(pi(&plan)["model"], crate::run::host::model_info(&m));
        // The re-providing rows only for what uses them.
        for (grants, approval, session) in [
            ("", false, false),
            ("exec = \"free\"", false, false),
            ("exec = \"gate\"", true, false),
            ("wake = true", false, true),
        ] {
            let plan = plan_of(
                dir.path(),
                &format!("[[plugin]]\npi = \"./a.ts\"\ngrants = {{ {grants} }}\n"),
            )
            .unwrap();
            let ids = ids(&plan);
            assert_eq!(
                ids.contains(&PI_HOST_APPROVAL.to_owned()),
                approval,
                "{grants}: {ids:?}"
            );
            assert_eq!(
                ids.contains(&PI_HOST_SESSION.to_owned()),
                session,
                "{grants}: {ids:?}"
            );
        }
        // One file, one plugin.
        let e = plan_of(
            dir.path(),
            "[[plugin]]\npi = \"./a.ts\"\n[[plugin]]\nname = \"again\"\npi = \"./a.ts\"\n",
        )
        .unwrap_err();
        assert!(
            e.contains("plugin again: its extension") && e.contains("is plugin a's too"),
            "{e}"
        );
    }

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

    #[test]
    fn a_helper_runs_with_the_kept_environment_only() {
        // `cargo test` sets `CARGO_PKG_NAME` for the test: a variable this
        // process surely holds, which the kept set does not.
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("env.mjs");
        std::fs::write(
            &script,
            "const v = (n) => process.env[n] ?? 'unset'\n\
             console.log(JSON.stringify({ name: v('CARGO_PKG_NAME'), path: v('PATH') }))\n",
        )
        .unwrap();
        // On unix no name to drop is needed: only the kept set gets there.
        // Elsewhere only the names given are removed.
        let keys: Vec<String> = match cfg!(unix) {
            true => Vec::new(),
            false => vec!["CARGO_PKG_NAME".to_owned()],
        };
        let found = node_json(&script, &[], &keys).unwrap();
        assert_eq!(found.field("name").unwrap(), "unset");
        assert_eq!(
            found.field("path").unwrap(),
            &std::env::var("PATH").unwrap()
        );
    }
}
