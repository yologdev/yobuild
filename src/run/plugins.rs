//! A manifest's plugins as plugin-host rows, by ecosystem — checked before
//! anything starts. Until `yobuild` installs packages (M2b), a pinned
//! package must already be installed, at exactly its version: a pi package
//! in `node_modules/` beside the manifest, a DSH plugin in the DSH runtime's
//! (`adapters/dsh`, installed with `--no-save`: copying the adapters again,
//! or `npm ci` there, drops it).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::{json, Value as Json};

use super::Layout;
use crate::frontend::host::{rows_from_file, Row};
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
    /// Start the `dsh` runtime (`adapters/dsh`).
    pub dsh: bool,
    /// Start the `rutis` runtime: Node, in the manifest's folder.
    pub rutis: bool,
    /// Handlers the agent depends on (a TypeScript row runs before its
    /// async start-up is done).
    pub handlers: Vec<&'static str>,
    /// What loads only in part, or is set and has no effect: said before the
    /// agent starts.
    pub notes: Vec<String>,
    /// The modules of the DSH plugins' rows: the services they inject are
    /// checked once the runtime runs.
    pub dsh_modules: Vec<String>,
    /// DSH packages the plugins list as peers and no row loads: what a
    /// missing service may need.
    pub dsh_peers: Vec<String>,
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

/// Runs one of the adapters' helper scripts with Node; its JSON output.
fn node_json(script: &Path, args: &[&Path]) -> Result<Json, BoxError> {
    let out = std::process::Command::new("node")
        .arg(script)
        .args(args)
        .output()
        .map_err(|e| format!("node {}: {e}", script.display()))?;
    if !out.status.success() {
        return Err(format!(
            "node {}: {}",
            script.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    serde_json::from_slice(&out.stdout).map_err(|e| {
        let text = String::from_utf8_lossy(&out.stdout);
        let head: String = text.chars().take(200).collect();
        format!("node {}: {e} in {head:?}", script.display()).into()
    })
}

/// What the manifest's plugins load. Every refusal comes from here, before
/// anything starts.
pub(crate) fn plan(
    manifest: &Manifest,
    layout: &Layout,
    cwd: &Path,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<Plan, BoxError> {
    let mut plan = Plan::default();
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
                let found = pi_extensions(manifest, layout, plugin, source).map_err(failed)?;
                extensions.extend(found.extensions);
                plan.notes.extend(found.notes);
            }
            PluginKind::Dsh(source) => {
                let (rows, peers) = dsh_rows_of(layout, plugin, source, config).map_err(failed)?;
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
                let mut row =
                    Row::new(format!("plugin:{}", plugin.name), path.display()).runtime("rutis");
                if let Some(config) = config {
                    row = row.config(Json::Object(config));
                }
                if !plan.rutis {
                    rutis_runtime(manifest, layout)?;
                }
                plan.rutis = true;
                plan.rows.push(row);
            }
        }
    }
    if !extensions.is_empty() {
        let pi = layout.adapters.join("pi");
        plan.pi = true;
        plan.handlers.push("pi-extensions");
        plan.rows.push(
            Row::new("pi", pi.join("pi-extensions-adapter.ts").display())
                .runtime("pi")
                .config(json!({ "extensions": extensions, "cwd": cwd })),
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
        let dsh = layout.adapters.join("dsh");
        plan.dsh = true;
        plan.handlers.push("dsh-tools");
        if manifest.dsh.host_services {
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
            Row::new("dsh", dsh.join("dsh-tools-adapter.ts").display()).runtime("dsh"),
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
    let mut ids = HashSet::new();
    for row in &plan.rows {
        if !ids.insert(row.id()) {
            return Err(format!("two plugin rows named {}", row.id()).into());
        }
    }
    Ok(plan)
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
    manifest: &Manifest,
    layout: &Layout,
    plugin: &Plugin,
    source: &Source,
) -> Result<PiFound, BoxError> {
    let dir = match source {
        Source::Package(p) => installed(
            p,
            &manifest.dir.join("node_modules"),
            &format!("npm install {} in {}", p.spec(), manifest.dir.display()),
        )?,
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
    let list = |key: &str| found[key].as_array().cloned().unwrap_or_default();
    let extensions = list("extensions");
    if extensions.is_empty() {
        return Err(format!("no pi extension in {}", dir.display()).into());
    }
    let skipped: Vec<String> = ["skills", "prompts", "themes"]
        .into_iter()
        .filter(|key| !list(key).is_empty())
        .map(|key| format!("{} {key}", list(key).len()))
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

/// A DSH plugin's rows — a file is one row; a package, the rows its bundle
/// patch inserts (as DSH installs it), each `plugin:<name>:<row id>`, the
/// plugin's config replacing its first row's — and its peers.
fn dsh_rows_of(
    layout: &Layout,
    plugin: &Plugin,
    source: &Source,
    config: Option<serde_json::Map<String, Json>>,
) -> Result<(Vec<Row>, Vec<String>), BoxError> {
    let id = format!("plugin:{}", plugin.name);
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
    let base = layout.adapters.join("dsh");
    let modules = base.join("node_modules");
    let install = format!(
        "npm install --no-save --prefix {} {}; copying the adapters again, or npm ci there, drops it",
        base.display(),
        p.spec()
    );
    installed(p, &modules, &install)?;
    let inspected = node_json(
        &layout.adapters.join("dsh/inspect-package.mjs"),
        &[&base, Path::new(p.name())],
    )?;
    if let Some(errors) = inspected["importErrors"]
        .as_object()
        .filter(|e| !e.is_empty())
    {
        return Err(format!(
            "its rows cannot be imported: {}",
            Json::Object(errors.clone())
        )
        .into());
    }
    if let Some(ops) = inspected["otherPatchOps"]
        .as_array()
        .filter(|o| !o.is_empty())
    {
        return Err(format!(
            "its bundle patch does more than insert rows ({}), which yobuild does not apply",
            Json::Array(ops.clone())
        )
        .into());
    }
    let mut peers = Vec::new();
    for peer in inspected["peers"].as_array().into_iter().flatten() {
        let peer = peer.as_str().unwrap_or_default();
        if !modules.join(peer).join("package.json").is_file() {
            return Err(format!("it needs {peer}, not installed in {}", modules.display()).into());
        }
        peers.push(peer.to_owned());
    }
    let rows = inspected["rows"]
        .as_array()
        .filter(|rows| !rows.is_empty())
        .ok_or_else(|| format!("no rows in what DSH would install: {inspected}"))?;
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
            (_, _, Some(own)) => built = built.config(own.clone()),
            _ => {}
        }
        out.push(built);
    }
    Ok((out, peers))
}

/// A rutis plugin's Node runtime: `@arcships/rutis-runtime` installed in the
/// manifest's folder, at the version yobuild's own runtimes use.
fn rutis_runtime(manifest: &Manifest, layout: &Layout) -> Result<(), BoxError> {
    let pins = layout.frontend.join("plugins/package.json");
    let text = std::fs::read_to_string(&pins).map_err(|e| format!("{}: {e}", pins.display()))?;
    let version = serde_json::from_str::<Json>(&text)
        .map_err(|e| format!("{}: {e}", pins.display()))?["dependencies"][RUTIS_RUNTIME]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("{}: no {RUTIS_RUNTIME}", pins.display()))?;
    let package = Package::new(RUTIS_RUNTIME, &version)?;
    let install = format!(
        "a rutis plugin runs in a Node runtime in the manifest's folder: npm install {} in {}",
        package.spec(),
        manifest.dir.display()
    );
    installed(&package, &manifest.dir.join("node_modules"), &install)?;
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
        plan(&manifest(dir, text), &Layout::default(), dir, &|_: &str| {
            None
        })
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
                "a rutis plugin runs in a Node runtime in the manifest's folder: npm install @arcships/rutis-runtime@",
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
        assert!(!none.pi && !none.dsh && !none.rutis && none.rows.is_empty());
        assert!(none.handlers.is_empty() && none.notes.is_empty());
        // A plugin named like yobuild's own rows does not replace them.
        let plan = plan_of(
            dir.path(),
            "[[plugin]]\nname = \"dsh\"\ndsh = \"./dsh.ts\"\nconfig = { a = 1 }\n[[plugin]]\nname = \"pi\"\npi = \"./pi.ts\"\n",
        )
        .unwrap();
        assert!(plan.pi && plan.dsh && !plan.rutis);
        assert_eq!(plan.handlers, ["pi-extensions", "dsh-tools"]);
        let ids: Vec<&str> = plan.rows.iter().map(Row::id).collect();
        assert!(ids.contains(&"plugin:dsh") && ids.contains(&"dsh") && ids.contains(&"pi"));
        let own = plan.rows.iter().find(|r| r.id() == "plugin:dsh").unwrap();
        assert_eq!(own.config_json(), &json!({ "a": 1 }));
        let notes = plan_of(dir.path(), "[dsh]\nhost_services = true\n")
            .unwrap()
            .notes;
        assert_eq!(notes, ["[dsh] host_services has no effect: no DSH plugin"]);
    }
}
