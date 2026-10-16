//! A manifest's plugins as plugin-host rows, by ecosystem. Until `yobuild`
//! installs packages (M2b), a pinned package must already be installed, at
//! exactly its version: a pi package in `node_modules/` beside the
//! manifest, a DSH plugin in the DSH runtime's (`adapters/dsh`).

use std::path::{Path, PathBuf};

use serde_json::{json, Value as Json};

use super::Layout;
use crate::frontend::host::{rows_from_file, Row};
use crate::manifest::{Manifest, Package, PluginKind, Runtime, Source};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The rows to load and the handlers to wait for, by runtime.
#[derive(Default)]
pub(crate) struct Plan {
    pub rows: Vec<Row>,
    /// Runtimes to start: `pi`, `dsh`, `rutis` (a Node runtime in the
    /// manifest's folder).
    pub pi: bool,
    pub dsh: bool,
    pub rutis: bool,
    /// Handlers the agent depends on (a TypeScript row runs before its
    /// async start-up is done).
    pub handlers: Vec<&'static str>,
}

/// The folder of an installed package, at exactly `package`'s version.
pub(crate) fn installed(package: &Package, node_modules: &Path) -> Result<PathBuf, BoxError> {
    let dir = node_modules.join(package.name());
    let manifest = dir.join("package.json");
    let found = std::fs::read_to_string(&manifest)
        .ok()
        .and_then(|text| serde_json::from_str::<Json>(&text).ok())
        .and_then(|m| m["version"].as_str().map(str::to_owned));
    match found {
        Some(version) if version == package.version() => Ok(dir),
        Some(version) => Err(format!(
            "{} is installed at {version} in {}, not {}: install the pinned version",
            package.name(),
            node_modules.display(),
            package.version()
        )
        .into()),
        None => Err(format!(
            "{} is not installed in {} (until yobuild installs packages: npm install {})",
            package.name(),
            node_modules.display(),
            package.spec()
        )
        .into()),
    }
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
    Ok(serde_json::from_slice(&out.stdout)?)
}

/// What the manifest's plugins load.
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
        let config = plugin
            .config_with(lookup)
            .map_err(|e| format!("plugin {}: {e}", plugin.name))?;
        match &plugin.kind {
            PluginKind::Pi(source) => {
                if config.is_some() {
                    return Err(
                        format!("plugin {}: a pi extension takes no config", plugin.name).into(),
                    );
                }
                let dir = match source {
                    Source::Package(p) => installed(p, &manifest.dir.join("node_modules"))?,
                    Source::Path(path) => path.clone(),
                };
                if dir.is_file() {
                    extensions.push(json!(dir));
                } else {
                    // What pi's package manager finds in it.
                    let found =
                        node_json(&layout.adapters.join("pi/resolve-package.mjs"), &[&dir])?;
                    let found = found["extensions"].as_array().cloned().unwrap_or_default();
                    if found.is_empty() {
                        return Err(format!(
                            "plugin {}: no pi extension in {}",
                            plugin.name,
                            dir.display()
                        )
                        .into());
                    }
                    extensions.extend(found);
                }
            }
            PluginKind::Dsh(source) => {
                let rows = match source {
                    Source::Package(p) => {
                        let base = layout.adapters.join("dsh");
                        installed(p, &base.join("node_modules"))?;
                        // The rows DSH would insert (its bundle patch, or one row naming it).
                        let inspected = node_json(
                            &layout.adapters.join("dsh/inspect-package.mjs"),
                            &[&base, Path::new(p.name())],
                        )?;
                        inspected["rows"].as_array().cloned().unwrap_or_default()
                    }
                    Source::Path(path) => vec![json!({ "id": plugin.name, "name": path })],
                };
                for (at, row) in rows.iter().enumerate() {
                    let id = row["id"].as_str().unwrap_or("target");
                    let name = row["name"].as_str().ok_or_else(|| {
                        format!("plugin {}: a row without a name: {row}", plugin.name)
                    })?;
                    // Each row under the plugin's name; the plugin's config on its first.
                    let mut out = Row::new(format!("{}-{id}", plugin.name), name).runtime("dsh");
                    match (at, &config, row.get("config")) {
                        (0, Some(config), _) => out = out.config(Json::Object(config.clone())),
                        (_, _, Some(own)) => out = out.config(own.clone()),
                        _ => {}
                    }
                    dsh_rows.push(out);
                }
            }
            PluginKind::Rutis { path, runtime } => {
                if *runtime != Runtime::Node {
                    return Err(format!(
                        "plugin {}: yobuild run starts Node runtimes only, for now ({runtime:?})",
                        plugin.name
                    )
                    .into());
                }
                let mut row = Row::new(plugin.name.clone(), path.display()).runtime("rutis");
                if let Some(config) = config {
                    row = row.config(Json::Object(config));
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
    if !dsh_rows.is_empty() {
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
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(spec: &str) -> Package {
        let manifest = crate::manifest::Manifest::parse(
            &format!(
                "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n[[plugin]]\npi = \"{spec}\"\n"
            ),
            Path::new("a.toml"),
            Path::new("/"),
        )
        .unwrap();
        match &manifest.plugins[0].kind {
            PluginKind::Pi(Source::Package(p)) => p.clone(),
            _ => unreachable!(),
        }
    }

    #[test]
    fn a_package_must_be_installed_at_its_pinned_version() {
        let dir = tempfile::tempdir().unwrap();
        let modules = dir.path().join("node_modules");
        std::fs::create_dir_all(modules.join("@s/x")).unwrap();
        std::fs::write(modules.join("@s/x/package.json"), r#"{"version":"1.2.3"}"#).unwrap();
        assert_eq!(
            installed(&package("@s/x@1.2.3"), &modules).unwrap(),
            modules.join("@s/x")
        );
        let e = installed(&package("@s/x@1.2.4"), &modules)
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("installed at 1.2.3") && e.contains("not 1.2.4"),
            "{e}"
        );
        let e = installed(&package("y@1.0.0"), &modules)
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("not installed") && e.contains("npm install y@1.0.0"),
            "{e}"
        );
    }
}
