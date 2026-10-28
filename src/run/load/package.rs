//! A pi or DSH package `load_plugin` loads (design §14 "Stage 2"): installed
//! as its set says, in a Node runtime of its own (`loaded-<n>`) that the
//! bridge marks restricted before it starts — every handler registered
//! there gets its tools and its own `before_tool` and `finish`, no other
//! hook (yoagent-rutis 0.9) — given `yoagent` and no other service, no
//! grant, no config, no host service. Its tools are read for the second
//! question through the bridge's late source, as the model would get them,
//! and offered from the next turn once kept.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use serde_json::{json, Value as Json};
use yoagent::ToolContext;
use yoagent_rutis::RutisBridge;

use super::{
    clip, failed, first_details, reads, refused, second_details, tools_problem, Kept, LoadPlugin,
    Reads, Refused, Running, START_TIME,
};
use crate::builtins::Candidate;
use crate::frontend::host::{PluginHost, Row};
use crate::frontend::NoticeLevel;
use crate::install::Set;
use crate::manifest::ExtendKind;
use crate::run::plugins;

/// What a loaded package runs on, shared with `run`.
#[derive(Clone)]
pub(crate) struct Plugins {
    /// The session's plugin host: `run`'s tick looks at it too.
    pub host: Arc<tokio::sync::Mutex<PluginHost>>,
    /// Marks a loaded runtime restricted; its registry shows the handler.
    pub bridge: RutisBridge,
    /// yobuild's adapters runtime set: `pi/`, `dsh/`.
    pub adapters: PathBuf,
}

/// What reading the installed package found.
pub(super) enum Found {
    /// The pi extensions, and what it ships besides that is not loaded.
    Pi {
        extensions: Vec<Json>,
        notes: Vec<String>,
    },
    /// The DSH package's rows (`loaded:<name>:<row>`, runtime `dsh` until
    /// placed), and their modules for the service check.
    Dsh {
        rows: Vec<Row>,
        modules: Vec<String>,
    },
}

/// Step 4 for a package: its set's folder, what it holds, the install
/// scripts not run, what its code reads.
struct Installed {
    dir: PathBuf,
    found: Found,
    scripts: Vec<String>,
    reads: Reads,
}

/// Removes an added runtime when dropped armed (a refusal, a run stopped,
/// the call dropped), in a task of its own: it waits for the host's lock.
pub(super) struct RemoveOnDrop {
    host: Arc<tokio::sync::Mutex<PluginHost>>,
    runtime: Option<String>,
}

impl RemoveOnDrop {
    /// Armed by [`LoadPlugin::start_package`] once the runtime runs.
    pub(super) fn new(host: Arc<tokio::sync::Mutex<PluginHost>>) -> Self {
        Self {
            host,
            runtime: None,
        }
    }

    /// Removed now, waited for (the owner's Block).
    pub(super) async fn now(mut self) {
        if let Some(runtime) = self.runtime.take() {
            remove(&self.host, &runtime).await;
        }
    }

    /// Kept: nothing removed.
    fn disarm(mut self) {
        self.runtime = None;
    }
}

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            let host = self.host.clone();
            tokio::spawn(async move { remove(&host, &runtime).await });
        }
    }
}

async fn remove(host: &tokio::sync::Mutex<PluginHost>, runtime: &str) {
    if let Err(e) = host.lock().await.remove_runtime(runtime).await {
        tracing::warn!("removing a loaded plugin's runtime: {e}");
    }
}

impl LoadPlugin {
    /// Steps 4 to 7 for a pi or DSH package (`candidate`), named `name`.
    pub(super) async fn load_package(
        &self,
        candidate: &Candidate,
        package: &str,
        version: &str,
        name: &str,
        ctx: &ToolContext,
    ) -> Result<String, Refused> {
        let kind = candidate.kind;
        let Some(on) = self.plugins.clone() else {
            return Err(refused("this session runs no plugin host to load it in"));
        };
        // 4. Installed, running nothing; what it holds read from its files.
        let set = match kind {
            ExtendKind::Pi => Set::Pi,
            _ => Set::Dsh,
        };
        let (adapters, keys, withheld) = (
            on.adapters.clone(),
            self.keys.clone(),
            self.withheld.clone(),
        );
        let (package_, id) = (package.to_owned(), format!("loaded:{name}"));
        let Installed {
            dir,
            found,
            scripts,
            reads,
        } = self
            .install(set, package, version, ctx, move |dir, scripts| {
                let code = dir.join("node_modules").join(&package_);
                let found = match kind {
                    ExtendKind::Pi => {
                        let found = plugins::pi_package(&adapters, &code, &package_, &keys);
                        let found = found.map_err(|e| format!("{package_}: {e}"))?;
                        Found::Pi {
                            extensions: found.extensions,
                            notes: found.notes,
                        }
                    }
                    _ => {
                        let rows = plugins::dsh_package(
                            &adapters, &dir, &package_, &code, &id, None, &keys,
                        );
                        let (rows, _) = rows.map_err(|e| format!("{package_}: {e}"))?;
                        let modules = rows.iter().map(|r| r.name().to_owned()).collect();
                        Found::Dsh { rows, modules }
                    }
                };
                Ok(Installed {
                    reads: reads(&code, &withheld),
                    dir,
                    found,
                    scripts,
                })
            })
            .await?;
        // 5. Ask: start it?
        let mut extra = match &found {
            Found::Pi { notes, .. } => notes.clone(),
            Found::Dsh { .. } => Vec::new(),
        };
        extra.push(
            "It runs in a runtime of its own with its tools only: its hooks are not called, and it gets no grants, config or host services.".into(),
        );
        let details = first_details(
            candidate,
            version,
            name,
            &[],
            &self.cwd,
            &scripts,
            &reads,
            &extra,
        );
        let label = kind.label();
        let title =
            format!("Start the {label} {package}@{version} as {name}? (asked by the agent)");
        self.ask(title, details, package, "start it", ctx).await?;
        // 6. Started in a runtime of its own; its tools read and judged.
        let runtime = {
            let mut state = super::locked(&self.state);
            state.runtimes += 1;
            format!("loaded-{}", state.runtimes)
        };
        let handler = format!("loaded-{name}-{}", uuid::Uuid::new_v4().simple());
        let _loading = self.loaded.loading(&handler, name);
        let mut remove = RemoveOnDrop::new(on.host.clone());
        let id = format!("loaded:{name}");
        let started = self.start_package(
            &on, &runtime, &handler, &id, &dir, found, package, &mut remove,
        );
        tokio::select! {
            _ = ctx.cancel.cancelled() => return Err(refused("the run was stopped while it started")),
            started = started => started?,
        }
        let tools = self.loaded.read_tools(name).await;
        let prefix = format!("{name}__");
        let own: Vec<String> = tools
            .iter()
            .filter_map(|t| t.name().strip_prefix(&prefix).map(str::to_owned))
            .collect();
        let own = tools_problem(name, &own).map_err(|e| {
            failed(format!(
                "{package}: {e} (as a loaded {label} gets them: in a runtime of its own, no grants, no config)"
            ))
        })?;
        // 7. Ask: keep it?
        let title = format!(
            "Keep the {label} {name}? It offers: {}",
            clip(&own.join(", "), 200)
        );
        let details = second_details(
            &format!("the {label}"),
            tools.iter().map(|t| {
                (
                    t.name().to_owned(),
                    t.description().to_owned(),
                    t.parameters_schema(),
                )
            }),
        );
        if let Err(no) = self.ask(title, details, package, "keep it", ctx).await {
            remove.now().await;
            return Err(no);
        }
        // Gone while the owner was asked: not announced as loaded.
        if let Some(why) = on.host.lock().await.added_stopped(&runtime) {
            return Err(failed(format!(
                "{package} stopped while you were asked to keep it: {why}"
            )));
        }
        let names: Vec<String> = own.iter().map(|t| format!("{name}__{t}")).collect();
        let kept = Kept {
            kind,
            name: name.to_owned(),
            package: package.to_owned(),
            version: version.to_owned(),
            args: Vec::new(),
            tools: own.clone(),
            stopped: None,
        };
        self.session.notice(
            NoticeLevel::Info,
            format!(
                "The agent loaded {package}@{version} as {name}, for this session. {}",
                super::kept_text(std::slice::from_ref(&kept), &self.files)
            ),
        );
        self.loaded.add_package(
            kept,
            Running {
                name: name.to_owned(),
                runtime,
                handler,
                tools: own,
            },
        );
        remove.disarm();
        Ok(format!(
            "Loaded {package}@{version} as {name}, for this session. Its tools are yours from your next step: {}. Each call asks the owner.",
            names.join(", ")
        ))
    }

    /// The runtime added, restricted before it starts (`remove` armed once
    /// it runs), its rows loaded — the adapter's as `id`, `loaded:<name>`,
    /// so no grant and no host row matches it — a DSH package's services
    /// checked, and its adapter's handler waited for, all under the host's
    /// lock.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn start_package(
        &self,
        on: &Plugins,
        runtime: &str,
        handler: &str,
        id: &str,
        dir: &Path,
        found: Found,
        package: &str,
        remove: &mut RemoveOnDrop,
    ) -> Result<(), Refused> {
        let did_not = |why: String| failed(format!("{package} did not start: {why}"));
        let mut host = on.host.lock().await;
        let pi = on.adapters.join("pi");
        let folder = match found {
            Found::Pi { .. } => pi.clone(),
            Found::Dsh { .. } => dir.to_path_buf(),
        };
        let bridge = on.bridge.clone();
        host.add_runtime_with(runtime, &folder, &["yoagent"], |handle| {
            bridge.restrict_runtime(handle.clone())
        })
        .await
        .map_err(|e| did_not(e.to_string()))?;
        remove.runtime = Some(runtime.to_owned());
        let (rows, modules) = match found {
            Found::Pi { extensions, .. } => {
                // No grant (`execFor`, `projectTrustedFor`, `wakeFor`, the
                // model) and no `pi-host-ui`: pi's print mode.
                let config = json!({
                    "extensions": extensions,
                    "cwd": self.cwd,
                    "name": handler,
                    "nestedCalls": "own-extension",
                });
                let row = Row::new(id, pi.join("pi-extensions-adapter.ts").display())
                    .runtime(runtime)
                    .config(config);
                (vec![row], Vec::new())
            }
            Found::Dsh { rows, modules } => {
                // The base's rows as the `dsh` runtime has them, the
                // package's, then the adapter; no host services, no DSH
                // host, no dialogs.
                let base = |row: &str, module: &str| {
                    Row::new(format!("{id}/{row}"), module).runtime(runtime)
                };
                let mut all = vec![
                    base("dsh-web", "@deepseek-ai/dsh-web"),
                    base("dsh-system-prompt", "@deepseek-ai/dsh-system-prompt"),
                    base("dsh-tools", "@deepseek-ai/dsh-tools"),
                ];
                all.extend(rows.into_iter().map(|row| row.runtime(runtime)));
                all.push(
                    Row::new(id, dir.join("dsh-tools-adapter.ts").display())
                        .runtime(runtime)
                        .config(json!({ "name": handler })),
                );
                (all, modules)
            }
        };
        host.load(rows).await.map_err(|e| did_not(e.to_string()))?;
        if !modules.is_empty() {
            let mut missing = Vec::new();
            for wait in [1000, 3000] {
                missing = crate::run::probe_services(&mut host, runtime, dir, &modules, &[], wait)
                    .await
                    .map_err(|e| did_not(e.to_string()))?;
                if missing.is_empty() {
                    break;
                }
            }
            if !missing.is_empty() {
                return Err(did_not(format!(
                    "it waits for services nobody provides here ({}): a loaded DSH package gets none of DSH's host services",
                    missing.join(", ")
                )));
            }
        }
        registered(&on.bridge, handler, &host, runtime)
            .await
            .map_err(did_not)
    }
}

/// Waits until the adapter's handler, `handler`, is registered and
/// restricted ([`START_TIME`]); a runtime or row that stops meanwhile fails
/// it at once.
async fn registered(
    bridge: &RutisBridge,
    handler: &str,
    host: &PluginHost,
    runtime: &str,
) -> Result<(), String> {
    let deadline = Instant::now() + START_TIME;
    loop {
        if let Some(h) = bridge
            .registry()
            .handlers()
            .into_iter()
            .find(|h| h.name() == handler)
        {
            return match h.restricted() {
                true => Ok(()),
                // Never so: the runtime was marked before it started.
                false => Err("its handler was not registered as restricted".into()),
            };
        }
        if let Some(why) = host.added_stopped(runtime) {
            return Err(why);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "its adapter registered no tools within {} s",
                START_TIME.as_secs()
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}
