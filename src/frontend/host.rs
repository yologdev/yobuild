//! A few lines in place of the rutis setup: Node runtimes, a loader, shared
//! services, and plugin rows routed to the runtime each names.
//!
//! ```no_run
//! # async fn run(root: &rutis::Ctx) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
//! use yobuild::frontend::host::{PluginHost, Row};
//! let mut host = PluginHost::builder()
//!     .node("pi", "plugins/pi")        // package.json + node_modules here
//!     .node("dsh", "plugins/dsh")
//!     .share("yoagent")
//!     .start(root)
//!     .await?;
//! host.load([
//!     Row::new("pi", "plugins/pi/pi-extensions-adapter.ts").runtime("pi"),
//!     Row::new("dsh-tools", "@deepseek-ai/dsh-tools").runtime("dsh"),
//! ])
//! .await?;
//! # Ok(()) }
//! ```

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rutis::{BoxFuture, Ctx, FiberState};
use rutis_bridge::runtime::{Launcher, LocalRuntime, Stdio};
use rutis_loader::{
    Chain, EntryStatus, Layer, Loader, LoaderError, LoaderOptions, LoaderPlugin, Patch, Resolved,
    Resolver, RuntimeResolver, RuntimeRowsPlugin, ServiceCatalog,
};
use serde_json::{json, Value as Json};

/// How long [`PluginHost::load`] waits for plugins to start.
pub const LOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// A plugin to load: an id, what to load (an npm name or a file), the
/// runtime that loads it, and its config.
#[derive(Debug, Clone)]
pub struct Row {
    id: String,
    name: String,
    runtime: Option<String>,
    config: Json,
    inject: Vec<String>,
}

impl Row {
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Whether it names `runtime` as the one that loads it.
    pub(crate) fn runtime_is(&self, runtime: &str) -> bool {
        self.runtime.as_deref() == Some(runtime)
    }

    pub(crate) fn config_json(&self) -> &Json {
        &self.config
    }

    #[cfg(test)]
    pub(crate) fn runtime_name(&self) -> Option<&str> {
        self.runtime.as_deref()
    }

    pub fn new(id: impl Into<String>, name: impl ToString) -> Self {
        Row {
            id: id.into(),
            name: name.to_string(),
            runtime: None,
            config: json!({}),
            inject: Vec::new(),
        }
    }

    /// The runtime that loads it (a name given to [`PluginHostBuilder::node`]).
    /// Without one, the first runtime that can resolve the name does.
    pub fn runtime(mut self, runtime: impl Into<String>) -> Self {
        self.runtime = Some(runtime.into());
        self
    }

    pub fn config(mut self, config: Json) -> Self {
        self.config = config;
        self
    }

    /// The services injected into this plugin, in place of those it declares.
    ///
    /// The plugin then waits for every one of them. (On rutis 0.7 and 0.8 a
    /// TypeScript plugin given a row inject list never applied; 0.9 applies
    /// it.) To hand a plugin a service it may go without — it only looks it
    /// up, as the pi adapter does with `ui` — load a small plugin that
    /// injects it and provides it again under the name looked up instead:
    /// see `yoagent-rutis/plugins/pi/host-ui.ts`.
    pub fn inject(mut self, services: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.inject = services.into_iter().map(Into::into).collect();
        self
    }

    fn json(&self) -> Json {
        let mut row = json!({ "id": self.id, "name": self.name, "config": self.config });
        if !self.inject.is_empty() {
            row["inject"] = json!(self.inject);
        }
        row
    }
}

/// Where DSH's own services (`yoagent-rutis/plugins/dsh/host-services.json`)
/// keep their data: the credentials file, attachments, skills. They resolve
/// it from `DSH_HOME`, else `~/.dsh` — the user's real DSH home, with its
/// stored keys — and skills also from `DSH_AGENTS_HOME`, else `~/.agents`.
///
/// [`DshHome::new`] picks a home that is not the user's: `YOAGENT_DSH_HOME`
/// when set (non-empty), else a private directory created for this process
/// (owner-only, removed when the `DshHome` drops). [`DshHome::env`] is what
/// to give the DSH runtime ([`PluginHostBuilder::env`]).
///
/// What this does not keep from DSH plugins: the runtime inherits the host's
/// environment (an API key there is readable, and DSH's credentials service
/// ranks it first), the project's `.env` and skill folders (DSH reads them
/// from the working directory), and the filesystem (`dsh-fs-local` has no
/// sandbox). A guard against reusing `~/.dsh` by accident, not a sandbox.
pub struct DshHome {
    path: PathBuf,
    /// Set when the directory is ours to remove.
    _private: Option<tempfile::TempDir>,
}

impl DshHome {
    /// `YOAGENT_DSH_HOME` (created if missing), else a new private directory.
    /// An empty `YOAGENT_DSH_HOME` counts as unset: DSH reads an empty
    /// `DSH_HOME` as `~/.dsh`.
    pub fn new() -> std::io::Result<Self> {
        match std::env::var_os("YOAGENT_DSH_HOME")
            .filter(|v| !v.to_string_lossy().trim().is_empty())
        {
            Some(path) => {
                let path = PathBuf::from(path);
                std::fs::create_dir_all(&path)?;
                Ok(DshHome {
                    path,
                    _private: None,
                })
            }
            None => {
                let mut builder = tempfile::Builder::new();
                builder.prefix("yoagent-dsh-home-");
                // Owner-only: what DSH stores here (credentials) is ours alone.
                #[cfg(unix)]
                builder.permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700));
                let dir = builder.tempdir()?;
                Ok(DshHome {
                    path: dir.path().to_path_buf(),
                    _private: Some(dir),
                })
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `DSH_HOME`, and — for a private home — `DSH_AGENTS_HOME` under it, so
    /// `~/.agents` skills are not read either. A `YOAGENT_DSH_HOME` the user
    /// chose (`~/.dsh`, to share their keys on purpose) leaves skills alone.
    pub fn env(&self) -> Vec<(&'static str, PathBuf)> {
        let mut env = vec![("DSH_HOME", self.path.clone())];
        if self._private.is_some() {
            env.push(("DSH_AGENTS_HOME", self.path.join("agents")));
        }
        env
    }
}

/// The rows listed in the JSON file `path` (`[{ "id", "name", "config"? }]`,
/// such as `yoagent-rutis/plugins/dsh/host-services.json`), each routed to
/// the runtime `runtime`. Other keys are ignored.
pub fn rows_from_file(path: &Path, runtime: &str) -> Result<Vec<Row>, BoxError> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let listed: Json =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    rows_from_json(listed, runtime).map_err(|e| format!("{}: {e}", path.display()).into())
}

/// The rows of a JSON list (`[{ "id", "name", "config"? }]`, as
/// [`rows_from_file`] reads one), each routed to the runtime `runtime`.
pub fn rows_from_json(listed: Json, runtime: &str) -> Result<Vec<Row>, BoxError> {
    #[derive(serde::Deserialize)]
    struct Listed {
        id: String,
        name: String,
        #[serde(default)]
        config: Option<Json>,
    }
    let listed: Vec<Listed> = serde_json::from_value(listed)?;
    Ok(listed
        .into_iter()
        .map(|row| {
            let next = Row::new(row.id, row.name).runtime(runtime);
            match row.config {
                Some(config) => next.config(config),
                None => next,
            }
        })
        .collect())
}

/// `launcher` with the variables `names` taken out of its environment
/// ([`env_without`]), so nothing of rutis's command is copied, and what
/// rutis adds as it spawns (its channel) is not among them. `env -u`
/// removes a name whoever set it, so `names` holds none the launch sets.
#[cfg(unix)]
fn without(mut launcher: Launcher, names: &[&OsString]) -> Result<Launcher, BoxError> {
    let program = std::mem::take(&mut launcher.program);
    let args = std::mem::take(&mut launcher.args);
    // The child's `PATH`, when the launch sets one.
    let path = launcher
        .env
        .iter()
        .rev()
        .find(|(name, _)| name == "PATH")
        .map(|(_, value)| value.clone());
    let (program, args) = env_without(
        names,
        program,
        args,
        launcher.cwd.as_deref(),
        path.as_deref(),
    )?;
    launcher.program = program;
    launcher.args = args;
    Ok(launcher)
}

/// No `env` to exec through: the variables are set empty instead.
#[cfg(not(unix))]
fn without(launcher: Launcher, names: &[&OsString]) -> Result<Launcher, BoxError> {
    Ok(names
        .iter()
        .fold(launcher, |launcher, name| launcher.env(*name, "")))
}

/// Where [`env_without`] finds `env`, and the shell it runs a program
/// through when `env` would misread its path.
#[cfg(unix)]
const ENV: &str = "/usr/bin/env";
#[cfg(unix)]
const SH: &str = "/bin/sh";

/// `program args` started with the variables `names` taken out of its
/// environment (design §12 "Step 4", "Step 5"): `/usr/bin/env -u NAME …
/// program args`, each name once — `env` execs it, the same process (pid,
/// process group, inherited descriptors). A launcher can only add
/// variables. `env` reads a word holding `=` before the program as an
/// assignment, `--` or not, so such a program goes through `/bin/sh -c
/// 'exec "$0" "$@"'`. Through `env` even with nothing to take out, so what
/// reaches the program does not depend on what is set here; no
/// `/usr/bin/env` (or `/bin/sh` when needed): an error, not the variables
/// left in. (On macOS with System Integrity Protection on, the default,
/// `env` is protected, so `DYLD_*` variables do not reach the program
/// either: `run::dyld_note`.)
///
/// Spawning `env` succeeds whatever `program` is, so a program that is not
/// there or cannot run would only show as `env` exiting 126 or 127: it is
/// looked for first ([`startable`]), as the child will — a path from `cwd`
/// (the child's folder; `None`: this process's), a bare name on `path`
/// (the child's `PATH` when the launch sets one; `None`: this process's).
/// `/bin/sh` is needed only for a program whose path holds `=`.
#[cfg(unix)]
pub(crate) fn env_without(
    names: &[impl AsRef<std::ffi::OsStr>],
    program: OsString,
    args: Vec<OsString>,
    cwd: Option<&Path>,
    path: Option<&std::ffi::OsStr>,
) -> Result<(OsString, Vec<OsString>), BoxError> {
    startable(&program, cwd, path)?;
    let shell = program.to_string_lossy().contains('=');
    let needed = [Some(ENV), shell.then_some(SH)];
    if let Some(missing) = needed
        .into_iter()
        .flatten()
        .find(|p| !Path::new(p).is_file())
    {
        // Not the names: with the kept environment there are about 100.
        return Err(no_env(Path::new(&program), missing).into());
    }
    let mut wrapped: Vec<OsString> = Vec::new();
    for (at, name) in names.iter().enumerate() {
        let name = name.as_ref();
        if !names[..at].iter().any(|n| n.as_ref() == name) {
            wrapped.push("-u".into());
            wrapped.push(name.to_owned());
        }
    }
    if shell {
        wrapped.extend([SH.into(), "-c".into(), r#"exec "$0" "$@""#.into()]);
    }
    wrapped.push(program);
    wrapped.extend(args);
    Ok((ENV.into(), wrapped))
}

/// `program` names a file this user may execute (`access(X_OK)`, the
/// kernel's answer for the real user, so an ACL counts and another
/// user's 0744 file does not; a `noexec` mount is not seen), found as
/// `execvp` finds it: a name holding `/` from `cwd`, else each folder of
/// `path`, else of this process's `PATH`, else of `/bin:/usr/bin`
/// (glibc's default; macOS's searches the same two the other way round)
/// (an empty entry is the child's folder). The error says which program and
/// why: "yobuild cannot start X: no such file", "it is not a file", "it is
/// not executable", "it is not on PATH", "it is on PATH but not
/// executable", "it is not on PATH where it could be looked at (…)", or
/// the system's own error (a folder it may not search, a symlink loop) —
/// never "no such file" for a file that is there.
#[cfg(unix)]
pub(crate) fn startable(
    program: &std::ffi::OsStr,
    cwd: Option<&Path>,
    path: Option<&std::ffi::OsStr>,
) -> Result<(), BoxError> {
    let from = |p: &Path| match cwd {
        Some(cwd) if p.is_relative() => cwd.join(p),
        _ => p.to_path_buf(),
    };
    let shown = Path::new(program).display();
    let failed = |why: &dyn std::fmt::Display| -> BoxError {
        format!("yobuild cannot start {shown}: {why}").into()
    };
    if program.as_encoded_bytes().contains(&b'/') {
        return match runnable(&from(Path::new(program))) {
            Ok(()) => Ok(()),
            Err(Unrunnable::Missing) => Err(failed(&"no such file")),
            Err(Unrunnable::NotFile) => Err(failed(&"it is not a file")),
            Err(Unrunnable::NotExecutable) => Err(failed(&"it is not executable")),
            Err(Unrunnable::Failed(e)) => Err(failed(&e)),
        };
    }
    let search = path
        .map(OsString::from)
        .or_else(|| std::env::var_os("PATH"))
        .unwrap_or_else(|| "/bin:/usr/bin".into());
    // As `execvp`: the first that runs; else what kept the others from it.
    // Two differences, both rare and both for a bare name (a runtime's
    // `node`; a server's command is always a path): glibc's `execvp` stops
    // at an error other than ENOENT, ENOTDIR or EACCES (ELOOP, …), where
    // this goes on looking — so it may let pass what `env` then fails on,
    // never refuse what would run; and a folder named like the program is
    // passed over here, where `execvp` notes EACCES — so "it is not on
    // PATH" is said where a folder of that name is.
    let mut not_executable = false;
    let mut error: Option<(PathBuf, std::io::Error)> = None;
    for dir in std::env::split_paths(&search) {
        let candidate = from(&dir.join(program));
        match runnable(&candidate) {
            Ok(()) => return Ok(()),
            Err(Unrunnable::NotExecutable) => not_executable = true,
            Err(Unrunnable::Failed(e)) => {
                error.get_or_insert((candidate, e));
            }
            Err(Unrunnable::Missing | Unrunnable::NotFile) => {}
        }
    }
    Err(match (not_executable, error) {
        (true, _) => failed(&"it is on PATH but not executable"),
        (false, Some((at, e))) => failed(&format!(
            "it is not on PATH where it could be looked at ({}: {e})",
            at.display()
        )),
        (false, None) => failed(&"it is not on PATH"),
    })
}

/// Why [`runnable`] refused a path.
#[cfg(unix)]
enum Unrunnable {
    /// Nothing there (`ENOENT`, `ENOTDIR`).
    Missing,
    /// A folder, a socket, …
    NotFile,
    /// A file this user may not execute.
    NotExecutable,
    /// The system's error (`EACCES` on a folder in the way, `ELOOP`, …).
    Failed(std::io::Error),
}

/// `path` is a file this user may execute.
#[cfg(unix)]
fn runnable(path: &Path) -> Result<(), Unrunnable> {
    use std::os::unix::ffi::OsStrExt;
    match std::fs::metadata(path) {
        Ok(meta) if !meta.is_file() => return Err(Unrunnable::NotFile),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Unrunnable::Missing),
        Err(e) if e.raw_os_error() == Some(libc::ENOTDIR) => return Err(Unrunnable::Missing),
        Err(e) => return Err(Unrunnable::Failed(e)),
    }
    // A NUL in the path: `metadata` refused it already (InvalidInput);
    // said as the system's error should one ever get here.
    let Ok(text) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return Err(Unrunnable::Failed(std::io::ErrorKind::InvalidInput.into()));
    };
    // SAFETY: `text` is a valid NUL-terminated string that outlives the call.
    match unsafe { libc::access(text.as_ptr(), libc::X_OK) } {
        0 => Ok(()),
        _ => match std::io::Error::last_os_error() {
            e if e.raw_os_error() == Some(libc::EACCES) => Err(Unrunnable::NotExecutable),
            e => Err(Unrunnable::Failed(e)),
        },
    }
}

/// What a runtime inherits of this process's environment
/// ([`PluginHostBuilder::inherit`]): these names and the names beginning
/// with these prefixes (design §12 "Step 5").
#[derive(Debug, Clone, Default)]
pub struct Inherit {
    names: Vec<OsString>,
    prefixes: Vec<OsString>,
}

impl Inherit {
    /// Only `names` and the names beginning with one of `prefixes`.
    pub fn only(
        names: impl IntoIterator<Item = impl Into<OsString>>,
        prefixes: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Self {
        Self {
            names: names.into_iter().map(Into::into).collect(),
            prefixes: prefixes.into_iter().map(Into::into).collect(),
        }
    }

    /// `name` is inherited.
    pub fn keeps(&self, name: &std::ffi::OsStr) -> bool {
        self.names.iter().any(|n| n == name)
            || self
                .prefixes
                .iter()
                .any(|p| name.as_encoded_bytes().starts_with(p.as_encoded_bytes()))
    }
}

/// `name` matches `entry`, a name or a prefix ending in `*` (`PI_TUI_*`):
/// [`PluginHostBuilder::keep`]'s entries, an `env` grant's.
pub(crate) fn matches(entry: &std::ffi::OsStr, name: &std::ffi::OsStr) -> bool {
    match entry.as_encoded_bytes().strip_suffix(b"*") {
        Some(prefix) => name.as_encoded_bytes().starts_with(prefix),
        None => entry == name,
    }
}

/// [`env_without`]'s error when `missing` (`/usr/bin/env`, or `/bin/sh`
/// for a path holding `=`) is not there: the `no-env` hint's words, which
/// must not begin as `startable`'s ("yobuild cannot start …", the
/// `cannot-start` hint's, earlier in the table).
pub(crate) fn no_env(program: &Path, missing: &str) -> String {
    format!(
        "cannot start {} with the kept environment: {missing} is not there, and yobuild needs it to start a plugin runtime or stdio MCP server",
        program.display()
    )
}

/// The names in `vars` (this process's environment, as `vars_os` reads it)
/// not `kept`: what `env -u` takes out of a runtime's or stdio server's
/// environment (design §12 "Step 5"). Not `RUTIS_CHANNEL_TOKEN` when
/// `loopback` — what is started is a runtime rutis spawns and hands its
/// channel over by loopback ([`loopback`]), setting a fresh token on the
/// command, which a `-u` would remove; otherwise (a runtime handed fd 3, a
/// stdio server rutis does not start) one here is this process's own, say
/// from a rutis parent, and is taken out like any other. A name holding `=` (an entry `=x=1` reads as the name `=x`),
/// which `env -u` refuses, failing the start, passes as it is, warned once
/// a process. (An entry with no `=` after its first byte `vars_os` does not
/// return: it passes too.)
#[cfg(unix)]
pub(crate) fn not_inherited(
    vars: impl IntoIterator<Item = OsString>,
    kept: impl Fn(&std::ffi::OsStr) -> bool,
    loopback: bool,
) -> Vec<OsString> {
    use rutis_bridge::transport::local::CHANNEL_TOKEN;
    vars.into_iter()
        .filter(|name| !(kept(name) || loopback && name == CHANNEL_TOKEN))
        .filter(|name| {
            let odd = name.is_empty() || name.as_encoded_bytes().contains(&b'=');
            if odd {
                warn_once(name);
            }
            !odd
        })
        .collect()
}

/// rutis hands a spawned runtime its channel by loopback (with a token in
/// its environment): as rutis decides it — off unix, or with
/// `RUTIS_LOCAL_HANDOVER=loopback` in this process.
#[cfg(unix)]
pub(crate) fn loopback() -> bool {
    std::env::var_os(rutis_bridge::transport::local::HANDOVER_VARIABLE)
        .is_some_and(|value| value == "loopback")
}

/// Says once a process that the variable `name` passes to every plugin
/// runtime and stdio MCP server.
#[cfg(unix)]
fn warn_once(name: &std::ffi::OsStr) {
    static SAID: Mutex<Vec<OsString>> = Mutex::new(Vec::new());
    let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
    if !said.iter().any(|n| n == name) {
        said.push(name.to_owned());
        tracing::warn!(
            "the environment variable {:?} cannot be taken out (`env -u` refuses its name): every plugin runtime and stdio MCP server gets it",
            name
        );
    }
}

/// The type [`PluginHostBuilder::local`] names its services for: never
/// provided, so they never cross runtimes.
struct RuntimeLocal;

#[derive(Default)]
pub struct PluginHostBuilder {
    runtimes: Vec<(String, PathBuf)>,
    shared: Vec<String>,
    local: Vec<String>,
    terminal: Option<String>,
    env: Vec<(String, OsString, OsString)>,
    withhold: Vec<OsString>,
    keep: Vec<(String, OsString)>,
    inherit: Option<Inherit>,
}

impl PluginHostBuilder {
    /// A Node runtime named `name`, its packages in `dir` (`package.json`
    /// and `node_modules`, with `@arcships/rutis-runtime` installed).
    pub fn node(mut self, name: impl Into<String>, dir: impl Into<PathBuf>) -> Self {
        self.runtimes.push((name.into(), dir.into()));
        self
    }

    /// The runtime `name` reads the terminal (a terminal UI): it gets this
    /// process's stdin, and so stays in the terminal's process group. Every
    /// other runtime runs in a group of its own (since rutis 0.9), where Ctrl-C does
    /// not reach it — and where reading or setting up the terminal would
    /// stop it (SIGTTIN / SIGTTOU).
    pub fn terminal(mut self, name: impl Into<String>) -> Self {
        self.terminal = Some(name.into());
        self
    }

    /// Set the environment variable `name` for the runtime `runtime`'s
    /// process, on top of the environment it inherits (say DSH's `DSH_HOME`:
    /// see [`DshHome`]). A runtime this builder does not start is an error
    /// at [`start`](Self::start).
    pub fn env(
        mut self,
        runtime: impl Into<String>,
        name: impl Into<OsString>,
        value: impl Into<OsString>,
    ) -> Self {
        self.env.push((runtime.into(), name.into(), value.into()));
        self
    }

    /// Take the variable `name` out of every runtime's environment (a
    /// credential, design §12 "Step 4"), but where [`keep`](Self::keep) or an
    /// [`env`](Self::env) entry of that name says otherwise. rutis's
    /// launcher can only add variables, so the runtime starts as
    /// `/usr/bin/env -u NAME … <rutis's command>`: `env` execs it, the same
    /// process. Without `env` (not unix) it is set empty instead.
    pub fn withhold(mut self, name: impl Into<OsString>) -> Self {
        self.withhold.push(name.into());
        self
    }

    /// Leave the variable `name` in the runtime `runtime`, withheld or not
    /// inherited (an `env` grant; the terminal UI's own settings) — or,
    /// ending in `*`, every name beginning with what precedes it
    /// (`PI_TUI_*`). A runtime this builder does not start is an error at
    /// [`start`](Self::start).
    pub fn keep(mut self, runtime: impl Into<String>, name: impl Into<OsString>) -> Self {
        self.keep.push((runtime.into(), name.into()));
        self
    }

    /// Every runtime inherits only what `inherit` keeps of this process's
    /// environment, and what [`keep`](Self::keep) and [`env`](Self::env)
    /// say for it (design §12 "Step 5"): the rest is taken out as
    /// [`withhold`](Self::withhold) takes a name out, on unix. Off unix it
    /// has no effect (there is no `env` to start through: only what is
    /// withheld is set empty). Unset: the whole environment, but what is
    /// withheld.
    pub fn inherit(mut self, inherit: Inherit) -> Self {
        self.inherit = Some(inherit);
        self
    }

    /// A host service plugins may inject (`yoagent`, `frontend`, `ui`, …).
    pub fn share(mut self, service: impl Into<String>) -> Self {
        self.shared.push(service.into());
        self
    }

    /// A service name a row provides for the plugins of its own runtime
    /// only (such as the DSH adapter's `yoagent-dsh-host`, the stand-in
    /// agent): not shared by name, so a row of another runtime that injects
    /// it waits for a provider that never comes, and never starts. rutis
    /// 0.12 shares every name otherwise.
    pub fn local(mut self, service: impl Into<String>) -> Self {
        self.local.push(service.into());
        self
    }

    /// What [`inherit`](Self::inherit) takes out of a runtime's environment
    /// beside what it `kept`: on unix every name of this process's not
    /// inherited; off unix nothing (there is no `env` to start through).
    #[cfg(unix)]
    fn not_inherited(&self, kept: &dyn Fn(&std::ffi::OsStr) -> bool) -> Vec<OsString> {
        match &self.inherit {
            Some(inherit) => not_inherited(
                std::env::vars_os().map(|(k, _)| k),
                |key| inherit.keeps(key) || kept(key),
                loopback(),
            ),
            None => Vec::new(),
        }
    }

    #[cfg(not(unix))]
    fn not_inherited(&self, _kept: &dyn Fn(&std::ffi::OsStr) -> bool) -> Vec<OsString> {
        let _ = &self.inherit;
        Vec::new()
    }

    /// Start the runtimes and the loader on `root`.
    pub async fn start(self, root: &Ctx) -> Result<PluginHost, BoxError> {
        let started = |runtime: &String| self.runtimes.iter().any(|(r, _)| r == runtime);
        if let Some((runtime, name, _)) = self.env.iter().find(|(runtime, ..)| !started(runtime)) {
            return Err(format!(
                "environment variable {} set for runtime `{runtime}`, which is not started",
                name.to_string_lossy()
            )
            .into());
        }
        if let Some((runtime, name)) = self.keep.iter().find(|(runtime, _)| !started(runtime)) {
            return Err(format!(
                "environment variable {} kept for runtime `{runtime}`, which is not started",
                name.to_string_lossy()
            )
            .into());
        }
        // Every name shared (rutis 0.12's default): a row injecting one
        // nobody provides waits in rutis, and `load` names what it lacks.
        let mut catalog = ServiceCatalog::new();
        for service in &self.shared {
            catalog.register_shared(service);
        }
        // Named for a Rust service nobody provides: never shared, left to
        // the providing runtime's own Cordis.
        for service in &self.local {
            catalog.register::<RuntimeLocal>(service);
        }
        let routes: Arc<Mutex<HashMap<String, String>>> = Arc::default();
        let mut resolvers = Vec::new();
        // Every launch first: one that cannot withhold what it must fails
        // `start` before any runtime is registered.
        let mut launchers = Vec::new();
        for (name, dir) in &self.runtimes {
            let env: Vec<_> = self.env.iter().filter(|(r, ..)| r == name).collect();
            // Kept for this runtime: an entry of `keep`, or set by its
            // launch (`env -u` removes a name whoever set it).
            let kept = |key: &std::ffi::OsStr| {
                self.keep.iter().any(|(r, k)| r == name && matches(k, key))
                    || env.iter().any(|(_, k, _)| k == key)
            };
            let withheld: Vec<OsString> = self
                .withhold
                .iter()
                .filter(|key| !kept(key))
                .cloned()
                .chain(self.not_inherited(&kept))
                .collect();
            let withheld: Vec<&OsString> = withheld.iter().collect();
            let launcher = env.iter().fold(
                Launcher::node(&dir.join("node_modules/@arcships/rutis-runtime")),
                |launcher, (_, key, value)| launcher.env(key, value),
            );
            let launcher =
                without(launcher, &withheld).map_err(|e| format!("runtime `{name}`: {e}"))?;
            launchers.push((name, dir, launcher));
        }
        for (name, dir, launcher) in launchers {
            let mut runtime =
                LocalRuntime::launcher(name.clone(), launcher, dir.join("package.json"));
            if self.terminal.as_ref() == Some(name) {
                runtime = runtime.stdin(Stdio::Inherit);
            }
            let rows = Arc::new(RuntimeResolver::node(runtime.handle()).with_catalog(&catalog));
            root.plugin(runtime);
            resolvers.push((name.clone(), rows));
        }
        let routed = Routed {
            routes: routes.clone(),
            runtimes: resolvers.clone(),
        };
        let runtimes = resolvers.clone();
        let loader_plugin = LoaderPlugin::new(
            Chain::new().with(routed),
            LoaderOptions {
                catalog,
                ..LoaderOptions::default()
            },
        );
        let loader = loader_plugin.handle();
        root.plugin(loader_plugin).await?;
        for (_, rows) in resolvers {
            root.plugin(RuntimeRowsPlugin::new(rows));
        }
        Ok(PluginHost {
            loader,
            root: root.clone(),
            routes,
            runtimes,
            rows: Vec::new(),
        })
    }
}

/// Running runtimes and a loader; [`PluginHost::load`] adds plugins.
pub struct PluginHost {
    loader: Loader,
    /// The context the loader runs under: where a waiting row's services
    /// are looked up.
    root: Ctx,
    routes: Arc<Mutex<HashMap<String, String>>>,
    /// Each runtime's resolver: `reload` makes them forget a module.
    runtimes: Vec<(String, Arc<RuntimeResolver>)>,
    rows: Vec<Row>,
}

impl PluginHost {
    pub fn builder() -> PluginHostBuilder {
        PluginHostBuilder::default()
    }

    /// Each loaded row and its state (`Active`, `Pending`, an error, …).
    pub fn status(&self) -> Vec<(String, String)> {
        self.rows
            .iter()
            .map(|row| {
                let state = match self.loader.get(&row.id).map(|info| info.status) {
                    Some(EntryStatus::Running(s)) => match s.error {
                        Some(e) => format!("{:?}: {e}", s.state),
                        None => format!("{:?}", s.state),
                    },
                    Some(EntryStatus::Unresolved(e)) => format!("unresolved: {e}"),
                    Some(EntryStatus::Disabled) => "disabled".into(),
                    Some(EntryStatus::Inactive) => "inactive".into(),
                    Some(EntryStatus::Stopped) => "stopped".into(),
                    // `EntryStatus` is non-exhaustive.
                    Some(other) => format!("{other:?}"),
                    None => "not loaded".into(),
                };
                (row.id.clone(), state)
            })
            .collect()
    }

    /// Load more plugins, after those already loaded. Fails, naming the
    /// failures, when any plugin fails to load.
    ///
    /// Rows are routed by `name`: two rows loading the same name must name
    /// the same runtime. A batch that fails is dropped again, so a later
    /// `load` does not keep failing on it.
    pub async fn load(&mut self, rows: impl IntoIterator<Item = Row>) -> Result<(), BoxError> {
        let kept = self.rows.len();
        let routes = self.routes.lock().unwrap().clone();
        let result = self.add(rows);
        let result = match result {
            Ok(()) => {
                let added: Vec<String> = self.rows[kept..].iter().map(|r| r.id.clone()).collect();
                self.reconcile(&added).await
            }
            Err(e) => Err(e),
        };
        if result.is_err() {
            self.rows.truncate(kept);
            *self.routes.lock().unwrap() = routes;
            if let Err(e) = self.reconcile(&[]).await {
                tracing::warn!("dropping a plugin batch that failed to load also failed: {e}");
            }
        }
        result
    }

    fn add(&mut self, rows: impl IntoIterator<Item = Row>) -> Result<(), BoxError> {
        for row in rows {
            if let Some(runtime) = &row.runtime {
                let mut routes = self.routes.lock().unwrap();
                match routes.get(&row.name) {
                    Some(other) if other != runtime => {
                        return Err(format!(
                            "`{}` is loaded by runtime `{other}` already, not `{runtime}`",
                            row.name
                        )
                        .into())
                    }
                    _ => {
                        routes.insert(row.name.clone(), runtime.clone());
                    }
                }
            }
            self.rows.push(row);
        }
        Ok(())
    }

    /// Unload one plugin: its fiber is disposed, and with it what it
    /// registered (handlers and tools, services, UI plugin offers). A run in
    /// progress keeps the tools it started with; a call into the plugin after
    /// it left fails ("no longer available").
    pub async fn unload(&mut self, id: &str) -> Result<(), BoxError> {
        let index = self
            .rows
            .iter()
            .position(|row| row.id == id)
            .ok_or_else(|| format!("no plugin `{id}` is loaded"))?;
        let routes = self.routes.lock().unwrap().clone();
        let row = self.rows.remove(index);
        if !self.rows.iter().any(|other| other.name == row.name) {
            self.routes.lock().unwrap().remove(&row.name);
        }
        let result = self.reconcile(&[]).await;
        if result.is_err() {
            self.rows.insert(index, row);
            *self.routes.lock().unwrap() = routes;
            if let Err(e) = self.reconcile(&[]).await {
                tracing::warn!("putting back plugin `{id}` after a failed unload also failed: {e}");
            }
        }
        result
    }

    /// Whether the plugin `id` is running (started, not failed or stopped).
    /// A plugin that disposes its own fiber is not: since rutis 0.9 the Node
    /// runtime reports that to the loader, and its row reads inactive.
    pub fn is_running(&self, id: &str) -> bool {
        matches!(
            self.loader.get(id).map(|info| info.status),
            Some(EntryStatus::Running(s)) if s.state == FiberState::Active
        )
    }

    /// Reload one plugin, picking up its edited file, and restart it. The
    /// runtime imports the plugin's own module again when its file changed
    /// (not the modules that one imports: an edit there needs the runtime
    /// restarted). Waits until it runs again — though, as on `load`, a
    /// TypeScript plugin's async start-up may still be going.
    ///
    /// Not all or nothing. New code that does not import (a syntax error) is
    /// refused before anything stops: the running version stays. New code
    /// that imports but fails when it starts has replaced the old by then,
    /// and the plugin stays stopped until a later reload succeeds — so a
    /// plugin that is a policy should run under a bridge that requires one
    /// (`RutisExtension::require_policy`). The error says which happened;
    /// [`PluginHost::is_running`] tells too.
    pub async fn reload(&mut self, id: &str) -> Result<(), BoxError> {
        let name = self
            .rows
            .iter()
            .find(|row| row.id == id)
            .map(|row| row.name.clone())
            .ok_or_else(|| format!("no plugin `{id}` is loaded"))?;
        // A runtime resolver keeps a module's description until its package
        // version changes, and a file has none: forget it, so the row is
        // described (and imported) again and replaced.
        for (_, resolver) in &self.runtimes {
            resolver.invalidate(&name);
        }
        let result = match self.loader.reload(id).await {
            Ok(report) if !report.new_failures.is_empty() => {
                Err(format!("{:?}", report.new_failures).into())
            }
            Ok(_) => self.wait_running(&[id.to_owned()]).await,
            Err(e) => Err(e.into()),
        };
        result.map_err(|e: BoxError| {
            if self.is_running(id) {
                format!("`{id}` was not reloaded; its running version stays: {e}").into()
            } else {
                format!("`{id}` failed to start with its new code and is stopped until a reload succeeds: {e}").into()
            }
        })
    }

    /// The loader's layers for the rows loaded now.
    fn layers(&self) -> Result<Vec<Layer>, BoxError> {
        let insert: Vec<Json> = self.rows.iter().map(Row::json).collect();
        let patches: Vec<Patch> = serde_json::from_value(json!([{ "insert": insert }]))?;
        Ok(vec![Layer::new("app", patches)])
    }

    /// Apply the rows; fail on plugins this newly broke, and wait for
    /// `started` (the rows just added) to run. A plugin already failing — one
    /// a reload left stopped — does not fail every later change.
    async fn reconcile(&mut self, started: &[String]) -> Result<(), BoxError> {
        let report = self.loader.reconcile(self.layers()?, None).await?;
        if !report.new_failures.is_empty() {
            return Err(format!("plugins failed to load: {:?}", report.new_failures).into());
        }
        // `reconcile` counts a row still waiting (for a runtime, or for a
        // service it injects) as settled. A plugin that never starts — a
        // policy among them — must not pass for loaded: wait until the new
        // rows run, and name the ones that do not.
        self.wait_running(started).await
    }

    /// What of the loaded plugins does not run now, each with why: a row
    /// that failed or stopped itself; and, per runtime, the rows waiting —
    /// a runtime that died leaves its rows `Pending`, and nothing restarts
    /// it — said once for the runtime, without their passing states.
    /// `except` rows are left out (one the caller watches itself).
    pub fn stopped(&self, except: &[&str]) -> Vec<String> {
        let mut out = Vec::new();
        let mut waiting: Vec<(String, Vec<&str>)> = Vec::new();
        for row in self
            .rows
            .iter()
            .filter(|r| !except.contains(&r.id.as_str()))
        {
            match self.state(&row.id) {
                Started::Running => {}
                Started::Failed(why) => out.push(why),
                Started::Waiting => {
                    let runtime = row.runtime.clone().unwrap_or_else(|| "?".into());
                    match waiting.iter_mut().find(|(r, _)| *r == runtime) {
                        Some((_, rows)) => rows.push(&row.id),
                        None => waiting.push((runtime, vec![&row.id])),
                    }
                }
            }
        }
        for (runtime, rows) in waiting {
            out.push(format!(
                "the `{runtime}` runtime is not running (its plugins wait: {})",
                rows.join(", ")
            ));
        }
        out
    }

    /// Whether the row `id` runs, waits, or failed (and why).
    fn state(&self, id: &str) -> Started {
        let stopped = || Started::Failed(format!("plugin `{id}` stopped itself"));
        match self.loader.get(id).map(|info| info.status) {
            Some(EntryStatus::Running(s)) if s.state == FiberState::Active => Started::Running,
            Some(EntryStatus::Running(s)) if s.state == FiberState::Failed => {
                Started::Failed(format!("plugin `{id}` failed: {:?}", s.error))
            }
            Some(EntryStatus::Running(s)) if s.state == FiberState::Disposed => stopped(),
            Some(EntryStatus::Unresolved(e)) => {
                Started::Failed(format!("plugin `{id}` cannot load: {e}"))
            }
            // Copies inside instanced groups only, which `PluginHost`
            // does not create.
            Some(EntryStatus::Stopped) => stopped(),
            Some(EntryStatus::Disabled) => Started::Failed(format!("plugin `{id}` is disabled")),
            Some(EntryStatus::Running(_)) => Started::Waiting,
            // `EntryStatus` is non-exhaustive.
            Some(_) => Started::Waiting,
            None => Started::Waiting,
        }
    }

    /// Wait until these rows run; fail naming those that failed or did not,
    /// and for one still waiting at the end, the services it injects that
    /// nobody provides (yoagent-rutis's `rows::wait_running`).
    async fn wait_running(&self, ids: &[String]) -> Result<(), BoxError> {
        let layers = self.layers()?;
        // A look at 15 s, then until LOAD_TIMEOUT: rows that only wait for
        // services nobody provides are said at the first (a provider may
        // start slowly), not after a minute; rows loading, or waiting for
        // services that exist, get the whole time. (A row that cannot run
        // ends either look at once; each look that ends with rows waiting
        // asks the runtimes what those inject, for up to 10 s.) The error is
        // kept as it is (`RowsNotRunning`): a caller may say what a row
        // waits for in its own words.
        let start = tokio::time::Instant::now();
        let looks = [std::time::Duration::from_secs(15), LOAD_TIMEOUT];
        for (n, look) in looks.into_iter().enumerate() {
            let left = look.saturating_sub(start.elapsed());
            let error = match yoagent_rutis::rows::wait_running(
                &self.loader, &self.root, &layers, ids, left,
            )
            .await
            {
                Ok(()) => return Ok(()),
                Err(e) => e,
            };
            let only_missing = error.rows.iter().all(|row| {
                matches!(row.state, yoagent_rutis::rows::RowState::Waiting)
                    && row.missing.as_ref().is_some_and(|m| !m.is_empty())
            });
            let last = n + 1 == looks.len();
            let ends = error.rows.iter().any(|row| !row.state.may_run());
            if last || ends || only_missing {
                return Err(error.into());
            }
        }
        unreachable!("the last look returns")
    }
}

/// Resolves a row with the runtime it named; other names (no runtime given)
/// with the first runtime that knows them.
struct Routed {
    routes: Arc<Mutex<HashMap<String, String>>>,
    runtimes: Vec<(String, Arc<RuntimeResolver>)>,
}

impl Resolver for Routed {
    fn resolve<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<Arc<Resolved>, LoaderError>> {
        let route = self.routes.lock().unwrap().get(name).cloned();
        Box::pin(async move {
            if let Some(route) = route {
                let Some((_, resolver)) = self.runtimes.iter().find(|(n, _)| *n == route) else {
                    return Err(LoaderError::NotFound {
                        name: name.to_owned(),
                    });
                };
                return resolver.resolve(name).await;
            }
            for (_, resolver) in &self.runtimes {
                match resolver.resolve(name).await {
                    Err(LoaderError::NotFound { .. }) => continue,
                    other => return other,
                }
            }
            Err(LoaderError::NotFound {
                name: name.to_owned(),
            })
        })
    }
}

/// How far a row got.
enum Started {
    Running,
    Waiting,
    Failed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `env -u` is given: every name not kept, never rutis's channel
    /// token, and no name `env -u` would refuse.
    #[test]
    fn what_is_not_inherited_skips_the_channel_token_and_names_env_refuses() {
        let inherit = Inherit::only(["PATH"], ["LC_"]);
        let vars = || {
            [
                "PATH", "LC_ALL", "SECRET", "RUTIS_CHANNEL_TOKEN", "=x", "", "PI_TUI_X",
            ]
            .map(OsString::from)
        };
        let granted = |name: &std::ffi::OsStr| matches("PI_TUI_*".as_ref(), name);
        let kept = |name: &std::ffi::OsStr| inherit.keeps(name) || granted(name);
        // In loopback handover rutis sets a fresh token: never taken out.
        assert_eq!(not_inherited(vars(), kept, true), ["SECRET"]);
        // Otherwise one here is this process's own: taken out.
        assert_eq!(
            not_inherited(vars(), kept, false),
            ["SECRET", "RUTIS_CHANNEL_TOKEN"]
        );
        assert!(matches("A_B".as_ref(), "A_B".as_ref()));
        assert!(!matches("A_B".as_ref(), "A_BC".as_ref()));
        assert!(matches("A_*".as_ref(), "A_BC".as_ref()));
        assert!(!matches("A_*".as_ref(), "B_A".as_ref()));
    }

    /// With nothing to take out, a missing program is still said as
    /// `startable` says it (no early return).
    #[cfg(unix)]
    #[test]
    fn a_missing_program_with_nothing_to_take_out_is_said() {
        let error = env_without(
            &[] as &[&str],
            "/nonexistent/x".into(),
            Vec::new(),
            None,
            None,
        )
        .unwrap_err()
        .to_string();
        assert_eq!(error, "yobuild cannot start /nonexistent/x: no such file");
    }

    /// A program goes through `env` even with nothing to take out, so what
    /// reaches it does not depend on what is set here.
    #[cfg(unix)]
    #[test]
    fn a_program_starts_through_env_with_nothing_to_take_out() {
        let (program, args) = env_without(
            &[] as &[&str],
            "/bin/sh".into(),
            vec!["-c".into()],
            None,
            None,
        )
        .unwrap();
        assert_eq!(program, ENV);
        assert_eq!(args, ["/bin/sh", "-c"].map(OsString::from));
    }

    /// `startable` looks for a program as `execvp` will, from the child's
    /// folder, and says why one cannot start without calling a file that is
    /// there missing.
    #[cfg(unix)]
    #[test]
    fn a_program_is_looked_for_as_the_child_will_and_its_errors_kept_apart() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        let bin = cwd.join("bin");
        std::fs::create_dir(&bin).unwrap();
        let tool = bin.join("tool");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let start = |program: &str, cwd: Option<&Path>, path: Option<&str>| {
            startable(
                std::ffi::OsStr::new(program),
                cwd,
                path.map(std::ffi::OsStr::new),
            )
            .map_err(|e| e.to_string())
        };
        // A relative path and a relative `PATH` entry: from the child's
        // folder, not this process's.
        assert_eq!(start("bin/tool", Some(cwd), None), Ok(()));
        assert_eq!(start("tool", Some(cwd), Some("bin")), Ok(()));
        // An empty entry is the child's folder.
        assert_eq!(start("tool", Some(&bin), Some(":/nowhere")), Ok(()));
        // A later entry that runs wins over an earlier one that does not.
        let early = cwd.join("early");
        std::fs::create_dir(&early).unwrap();
        std::fs::write(early.join("tool"), "").unwrap();
        let both = format!("{}:{}", early.display(), bin.display());
        assert_eq!(start("tool", None, Some(&both)), Ok(()));
        // A symlink loop is the system's error, not "no such file".
        let looped = cwd.join("loop");
        std::os::unix::fs::symlink(&looped, &looped).unwrap();
        let e = start(looped.to_str().unwrap(), None, None).unwrap_err();
        assert!(
            e.starts_with(&format!("yobuild cannot start {}: ", looped.display()))
                && e.contains("symbolic links"),
            "{e}"
        );
        // A folder this user may not search: said, the file not called
        // missing. (Root searches it anyway.)
        // SAFETY: `geteuid` has no preconditions.
        if unsafe { libc::geteuid() } != 0 {
            let shut = cwd.join("shut");
            std::fs::create_dir(&shut).unwrap();
            std::fs::write(shut.join("tool"), "").unwrap();
            std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o000)).unwrap();
            let e = start(shut.join("tool").to_str().unwrap(), None, None).unwrap_err();
            let e2 = start("tool", None, Some(shut.to_str().unwrap())).unwrap_err();
            std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(
                !e.ends_with("no such file") && e.contains("ermission denied"),
                "{e}"
            );
            assert!(
                e2.starts_with(
                    "yobuild cannot start tool: it is not on PATH where it could be looked at ("
                ) && e2.contains("ermission denied"),
                "{e2}"
            );
        }
    }

    #[test]
    fn rows_from_file_reads_ids_names_and_configs_and_names_the_file_on_errors() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("rows.json");
        std::fs::write(
            &file,
            r#"[{"id": "a", "name": "pkg-a", "config": {"x": 1}}, {"id": "b", "name": "pkg-b", "note": "ignored"}]"#,
        )
        .unwrap();
        let rows = rows_from_file(&file, "dsh").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (
                rows[0].id.as_str(),
                rows[0].name.as_str(),
                rows[0].runtime.as_deref()
            ),
            ("a", "pkg-a", Some("dsh"))
        );
        assert_eq!(rows[0].config, json!({"x": 1}));
        assert_eq!(rows[1].config, json!({}));

        std::fs::write(&file, "[{").unwrap();
        let bad = rows_from_file(&file, "dsh").unwrap_err().to_string();
        assert!(bad.contains("rows.json"), "{bad}");
        let missing = rows_from_file(&dir.path().join("none.json"), "dsh")
            .unwrap_err()
            .to_string();
        assert!(missing.contains("none.json"), "{missing}");
    }

    #[test]
    fn the_shared_dsh_host_services_list_parses() {
        let file = yoagent_rutis::adapters::adapters()
            .dsh
            .join("host-services.json");
        let rows = rows_from_file(&file, "dsh").unwrap();
        assert_eq!(rows.len(), 7);
        assert!(rows
            .iter()
            .all(|row| row.name.starts_with("@deepseek-ai/dsh-")));
    }

    #[test]
    fn a_private_dsh_home_is_removed_and_points_skills_inside_it() {
        // Not YOAGENT_DSH_HOME: that is the user's choice, tested by hand.
        if std::env::var_os("YOAGENT_DSH_HOME").is_some() {
            return;
        }
        let home = DshHome::new().unwrap();
        let path = home.path().to_path_buf();
        assert!(path.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "owner-only");
        }
        let env = home.env();
        assert_eq!(env[0], ("DSH_HOME", path.clone()));
        assert_eq!(env[1], ("DSH_AGENTS_HOME", path.join("agents")));
        drop(home);
        assert!(!path.exists());
    }
}
