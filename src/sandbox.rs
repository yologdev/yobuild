//! The sandbox (design §17): with `[sandbox]` in a manifest, every plugin
//! runtime but the terminal UI's, every stdio MCP server (a manifest's or
//! one `load_plugin` starts) and the helper scripts that import a package's
//! code start confined — they write only to the agent's folder, temp and
//! what `[sandbox] write` adds, and read everything but the owner's secrets
//! ([`HIDDEN`] and `[sandbox] hide`). The network stays open. On Linux
//! through bubblewrap (`bwrap`), on macOS through `sandbox-exec`; elsewhere
//! refused. Not confined: the terminal UI's runtime (yobuild's own code),
//! the agent's own tools (the gate judges them) and npm (it runs no
//! package's scripts); `check` probes no pi or DSH plugin alone with
//! `[sandbox]`, since yoagent-rutis starts those probes.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::manifest;

/// What the sandbox hides beside `[sandbox] hide`, under the home folder:
/// where keys, tokens and passwords are kept by the tools that keep them —
/// SSH, GnuPG, the clouds' command lines, GitHub's and npm's logins, git's
/// stored credentials, the password stores, the keychains — and the shell
/// profiles, where keys are often exported. Each one that is there.
#[rustfmt::skip]
pub const HIDDEN: &[&str] = &[
    ".ssh", ".gnupg", ".aws", ".azure", ".config/gcloud", ".kube", ".docker",
    ".config/gh", ".netrc", ".git-credentials", ".npmrc", ".yarnrc.yml", ".pypirc",
    ".cargo/credentials", ".cargo/credentials.toml", ".password-store", ".config/op",
    ".local/share/keyrings", "Library/Keychains",
    ".bashrc", ".bash_profile", ".bash_login", ".profile", ".zshrc", ".zprofile",
    ".zshenv", ".zlogin", ".config/fish",
];

/// How long [`Sandbox::new`]'s trial may take.
const TRIAL: Duration = Duration::from_secs(10);

/// What confines a process.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Tool {
    /// bubblewrap, at this path.
    Bwrap(PathBuf),
    /// macOS's `/usr/bin/sandbox-exec`.
    SandboxExec,
}

impl Tool {
    /// This system's, or why there is none.
    fn here() -> Result<Tool, String> {
        if cfg!(target_os = "macos") {
            let path = Path::new(SANDBOX_EXEC);
            return match path.is_file() {
                true => Ok(Tool::SandboxExec),
                false => Err(format!(
                    "[sandbox] needs {SANDBOX_EXEC} on macOS, and it is not there"
                )),
            };
        }
        if cfg!(target_os = "linux") {
            return on_path("bwrap")
                .map(Tool::Bwrap)
                .ok_or_else(|| NO_BWRAP.to_owned());
        }
        Err("[sandbox] confines plugins on Linux (bubblewrap) and macOS (sandbox-exec) only".into())
    }

    fn name(&self) -> &'static str {
        match self {
            Tool::Bwrap(_) => "bwrap",
            Tool::SandboxExec => "sandbox-exec",
        }
    }
}

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Linux without bubblewrap.
pub(crate) const NO_BWRAP: &str = "[sandbox] needs bubblewrap (`bwrap`) on PATH, and it is not there: install it (`apt install bubblewrap`, `dnf install bubblewrap`), or leave out [sandbox]";

/// What [`Sandbox::new`]'s trial says when the tool is there but cannot
/// confine a process here, before why.
pub(crate) const CANNOT_START: &str = "the sandbox cannot start here: ";

/// `program` in a folder of `PATH`, executable.
fn on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(program))
        .find(|candidate| executable(candidate))
}

#[cfg(unix)]
fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn executable(path: &Path) -> bool {
    path.is_file()
}

/// A path the sandbox hides, and whether it is a folder (bubblewrap covers
/// a folder with an empty one, a file with `/dev/null`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Hidden {
    path: PathBuf,
    folder: bool,
}

/// Where confined processes may write, and what they may not read: every
/// path canonical and UTF-8 (a stdio server's command is text).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Rules {
    write: Vec<PathBuf>,
    hide: Vec<Hidden>,
    /// `[sandbox] hide` entries not there: nothing hidden for them.
    notes: Vec<String>,
}

impl Rules {
    /// The rules for an agent working in `cwd`, `home` the home folder:
    /// writable `cwd`, `temp` and `settings.write` (each there, a folder,
    /// not holding `home`); hidden [`HIDDEN`] and `settings.hide`, each
    /// that is there, none holding a writable folder. All canonical.
    fn of(
        settings: &manifest::Sandbox,
        cwd: &Path,
        home: &Path,
        temp: &[PathBuf],
    ) -> Result<Rules, String> {
        let home = canonical(home).map_err(|e| format!("the sandbox: the home folder {e}"))?;
        let cwd = canonical(cwd).map_err(|e| format!("the sandbox: the agent's folder {e}"))?;
        if home.starts_with(&cwd) {
            return Err(format!(
                "the sandbox: the agent's folder {} holds your home folder, which plugins could then write: run the agent in a folder of its own",
                cwd.display()
            ));
        }
        let mut write = vec![cwd.clone()];
        for folder in temp {
            if let Ok(folder) = canonical(folder) {
                write.push(folder);
            }
        }
        for place in &settings.write {
            let given = place.under(&home);
            let path = canonical(&given).map_err(|e| format!("[sandbox] write {e}"))?;
            if !path.is_dir() {
                return Err(format!("[sandbox] write {}: not a folder", given.display()));
            }
            if home.starts_with(&path) {
                return Err(format!(
                    "[sandbox] write {}: it holds your home folder: name the folders plugins need",
                    given.display()
                ));
            }
            write.push(path);
        }
        // Each once; one inside another is kept, so each is said.
        write.sort();
        write.dedup();
        let mut hide: Vec<Hidden> = Vec::new();
        let mut notes = Vec::new();
        let defaults = HIDDEN.iter().map(|rest| (home.join(rest), false));
        let given = settings.hide.iter().map(|place| (place.under(&home), true));
        for (path, named) in defaults.chain(given) {
            let Ok(real) = canonical(&path) else {
                if named {
                    notes.push(format!(
                        "[sandbox] hide {}: not there, so nothing is hidden for it",
                        path.display()
                    ));
                }
                continue;
            };
            if let Some(w) = write.iter().find(|w| w.starts_with(&real)) {
                let what = match *w == cwd {
                    true => "the agent's folder".to_owned(),
                    false => format!("{}, which plugins may write", w.display()),
                };
                return Err(format!(
                    "[sandbox] hide {}: it holds {what}: hide what is inside it instead",
                    path.display()
                ));
            }
            if !hide.iter().any(|h| h.path == real) {
                hide.push(Hidden {
                    folder: real.is_dir(),
                    path: real,
                });
            }
        }
        Ok(Rules { write, hide, notes })
    }
}

/// `path` canonical and UTF-8, or why not ("<path>: not there", …).
fn canonical(path: &Path) -> Result<PathBuf, String> {
    let real = std::fs::canonicalize(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!("{}: not there", path.display()),
        _ => format!("{}: {e}", path.display()),
    })?;
    match real.to_str() {
        Some(_) => Ok(real),
        None => Err(format!(
            "{}: its path is not UTF-8, which the sandbox cannot name",
            path.display()
        )),
    }
}

/// The temp folders a confined process may write: `TMPDIR` (Rust's
/// `temp_dir`), `/tmp` and `/var/tmp`, and on macOS the per-user folder
/// holding `TMPDIR` (`/var/folders/…/T`: its `C` sibling is the user's
/// cache folder there).
fn temp_folders() -> Vec<PathBuf> {
    let temp = std::env::temp_dir();
    let mut folders = vec![temp.clone(), "/tmp".into(), "/var/tmp".into()];
    if cfg!(target_os = "macos") {
        if let Ok(real) = std::fs::canonicalize(&temp) {
            if real.starts_with("/private/var/folders") && real.ends_with("T") {
                folders.extend(real.parent().map(Path::to_path_buf));
            }
        }
    }
    folders
}

/// The sandbox an agent's plugins and stdio servers start in: made once,
/// as the agent starts ([`Sandbox::new`]), then each command wrapped
/// ([`Sandbox::wrap`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sandbox {
    tool: Tool,
    rules: Rules,
}

impl Sandbox {
    /// The sandbox `[sandbox]` (`settings`) asks for, for an agent working
    /// in `cwd`: its folders found, this system's tool found and tried (a
    /// shell that does nothing, started through it), or why not — said
    /// before anything starts.
    pub fn new(settings: &manifest::Sandbox, cwd: &Path) -> Result<Sandbox, String> {
        let tool = Tool::here()?;
        let home = std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .ok_or("the sandbox needs the home folder (HOME), and HOME is not set")?;
        let mut rules = Rules::of(settings, cwd, &home, &temp_folders())?;
        // DSH's own home, when the owner named one (and it is there): the
        // DSH host's session log is written there.
        let dsh = std::env::var_os("YOAGENT_DSH_HOME").filter(|h| !h.is_empty());
        if let Some(folder) = dsh.and_then(|d| canonical(Path::new(&d)).ok()) {
            if !rules.write.contains(&folder) {
                rules.write.push(folder);
            }
        }
        let sandbox = Sandbox { tool, rules };
        sandbox.tried()?;
        Ok(sandbox)
    }

    /// `program args` confined: bubblewrap's or `sandbox-exec`'s command
    /// line, which execs it (`sandbox-exec` in the same process; bubblewrap
    /// as its child, in a pid namespace of its own that ends with it, and
    /// itself ending with its parent).
    pub fn wrap(&self, program: OsString, args: Vec<OsString>) -> (OsString, Vec<OsString>) {
        let mut wrapped: Vec<OsString> = match &self.tool {
            Tool::Bwrap(_) => bwrap_args(&self.rules),
            Tool::SandboxExec => vec!["-p".into(), profile(&self.rules).into()],
        };
        if matches!(self.tool, Tool::Bwrap(_)) {
            wrapped.push("--".into());
        }
        wrapped.push(program);
        wrapped.extend(args);
        let tool = match &self.tool {
            Tool::Bwrap(path) => path.clone().into_os_string(),
            Tool::SandboxExec => SANDBOX_EXEC.into(),
        };
        (tool, wrapped)
    }

    /// [`wrap`](Self::wrap) for a command given as text (a stdio server's:
    /// every path here is UTF-8).
    pub fn wrap_text(&self, program: String, args: Vec<String>) -> (String, Vec<String>) {
        let text = |word: OsString| word.to_string_lossy().into_owned();
        let (program, args) = self.wrap(program.into(), args.into_iter().map(Into::into).collect());
        (text(program), args.into_iter().map(text).collect())
    }

    /// `bwrap` or `sandbox-exec`.
    pub fn tool(&self) -> &'static str {
        self.tool.name()
    }

    /// The folders a confined process may write.
    pub fn writable(&self) -> &[PathBuf] {
        &self.rules.write
    }

    /// The folders and files it may not read.
    pub fn hidden(&self) -> Vec<&Path> {
        self.rules.hide.iter().map(|h| h.path.as_path()).collect()
    }

    /// What the agent is told as it starts (`run`'s Info notice, `check`'s
    /// agent note); `[sandbox] hide` entries not there are [`notes`].
    ///
    /// [`notes`]: Self::notes
    pub fn said(&self) -> String {
        let home = std::env::var_os("HOME").and_then(|h| std::fs::canonicalize(h).ok());
        let shown = |path: &Path| match home.as_deref().and_then(|h| path.strip_prefix(h).ok()) {
            Some(rest) => format!("~/{}", rest.display()),
            None => path.display().to_string(),
        };
        let write: Vec<String> = self.writable().iter().map(|p| shown(p)).collect();
        let hide: Vec<String> = self.hidden().into_iter().map(shown).collect();
        let hide = match hide.is_empty() {
            true => "nothing".to_owned(),
            false => hide.join(", "),
        };
        format!(
            "the sandbox ({}) confines every plugin runtime and stdio MCP server: they write only to {}; they read everything else but {hide}; the network is open to them",
            self.tool(),
            write.join(", ")
        )
    }

    /// `[sandbox] hide` entries not there.
    pub fn notes(&self) -> &[String] {
        &self.rules.notes
    }

    /// A shell that does nothing, started confined within [`TRIAL`]: the
    /// sandbox works here (an unprivileged user namespace may be refused, a
    /// sandbox may not nest), or why not.
    fn tried(&self) -> Result<(), String> {
        let (program, args) = self.wrap("/bin/sh".into(), vec!["-c".into(), ":".into()]);
        let cannot = |why: String| format!("{CANNOT_START}{why}");
        let mut child = std::process::Command::new(&program)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| cannot(format!("{}: {e}", self.tool())))?;
        let deadline = Instant::now() + TRIAL;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(cannot(format!(
                        "{} did not start a shell within {} s",
                        self.tool(),
                        TRIAL.as_secs()
                    )));
                }
                Err(e) => return Err(cannot(format!("{}: {e}", self.tool()))),
            }
        };
        if status.success() {
            return Ok(());
        }
        let mut stderr = String::new();
        if let Some(mut out) = child.stderr.take() {
            use std::io::Read;
            let _ = out.read_to_string(&mut stderr);
        }
        let first = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        Err(cannot(format!(
            "{} {status}: {}",
            self.tool(),
            match first.is_empty() {
                true => "it said nothing",
                false => first.trim(),
            }
        )))
    }
}

/// bubblewrap's options for `rules`: the whole file system read-only, a
/// device and a process folder of its own, each writable folder bound
/// writable over it, then each hidden folder covered by an empty read-only
/// one and each hidden file by `/dev/null` (after the writable folders, so
/// a hidden one inside them stays hidden). It ends with its parent, and in
/// a pid namespace of its own: what the confined process started ends with
/// it.
fn bwrap_args(rules: &Rules) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "--die-with-parent", "--unshare-pid", "--ro-bind", "/", "/", "--dev", "/dev", "--proc",
        "/proc",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    for folder in &rules.write {
        args.extend(["--bind".into(), folder.into(), folder.into()]);
    }
    for hidden in &rules.hide {
        let path: OsString = hidden.path.clone().into();
        match hidden.folder {
            true => args.extend(["--tmpfs".into(), path.clone(), "--remount-ro".into(), path]),
            false => args.extend(["--ro-bind".into(), "/dev/null".into(), path]),
        }
    }
    args
}

/// `sandbox-exec`'s profile for `rules`: everything allowed but writing,
/// which only the writable folders and the terminal's and null devices
/// take; then reading and writing the hidden paths denied (the last rule
/// that matches decides).
fn profile(rules: &Rules) -> String {
    let quoted = |path: &Path| {
        let text = path.to_string_lossy();
        format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
    };
    let mut profile =
        String::from("(version 1)\n(allow default)\n(deny file-write*)\n(allow file-write*");
    for folder in &rules.write {
        profile.push_str(&format!("\n  (subpath {})", quoted(folder)));
    }
    for device in [
        "/dev/null", "/dev/zero", "/dev/random", "/dev/urandom", "/dev/tty", "/dev/dtracehelper",
    ] {
        profile.push_str(&format!("\n  (literal \"{device}\")"));
    }
    profile.push_str("\n  (regex #\"^/dev/ttys[0-9]+$\")\n  (regex #\"^/dev/fd/[0-9]+$\"))\n");
    if !rules.hide.is_empty() {
        profile.push_str("(deny file-read* file-write*");
        for hidden in &rules.hide {
            profile.push_str(&format!("\n  (subpath {})", quoted(&hidden.path)));
        }
        profile.push_str(")\n");
    }
    profile
}

#[cfg(test)]
mod tests;
