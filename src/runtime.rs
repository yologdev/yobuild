//! yobuild's own runtime files, built into the binary and installed into
//! the cache on first need (design §13 "The runtime sets"), so `yobuild`
//! runs without its repository:
//! - `adapters`: yoagent-rutis's pi and DSH adapters
//!   (`yoagent_rutis::adapters::files()`), `pi/`, `dsh/` and `yoagent.d.ts`
//!   under one root, as its probes want;
//! - `ui`: the terminal UI and yobuild's own plugins (`plugins/`), beside
//!   the page's `web/lib.js` the terminal UI imports.
//!
//! Each at `<home>/runtime/<set>/<hash>/`, the hash over its files: written
//! whole in a `.partial-` folder, `npm ci` in each npm project (their
//! install scripts run: yobuild's own packages, pinned by locks it ships),
//! the stamp last, then renamed into place — a new yobuild with other files
//! gets a new folder, an old one stays for the old binary.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use sha2::{Digest, Sha256};

use crate::install::{self, STAMP};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// One of yobuild's runtime sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RuntimeSet {
    Adapters,
    Ui,
}

/// The `ui` set: the frontend's plugins package, and the page's library
/// the terminal UI imports (`../web/lib.js`, ESM by its `package.json`).
const UI_FILES: &[(&str, &[u8])] = &[
    (
        "plugins/package.json",
        include_bytes!("../frontend/plugins/package.json"),
    ),
    (
        "plugins/package-lock.json",
        include_bytes!("../frontend/plugins/package-lock.json"),
    ),
    (
        "plugins/terminal-ui.ts",
        include_bytes!("../frontend/plugins/terminal-ui.ts"),
    ),
    (
        "plugins/search-links.ts",
        include_bytes!("../frontend/plugins/search-links.ts"),
    ),
    (
        "plugins/todo-plan.ts",
        include_bytes!("../frontend/plugins/todo-plan.ts"),
    ),
    (
        "plugins/pi-extensions/confirm-dangerous.ts",
        include_bytes!("../frontend/plugins/pi-extensions/confirm-dangerous.ts"),
    ),
    ("web/lib.js", include_bytes!("../frontend/web/lib.js")),
    (
        "web/package.json",
        include_bytes!("../frontend/web/package.json"),
    ),
];

impl RuntimeSet {
    pub fn name(self) -> &'static str {
        match self {
            RuntimeSet::Adapters => "adapters",
            RuntimeSet::Ui => "ui",
        }
    }

    /// Its files, by path relative to its folder.
    fn files(self) -> &'static [(&'static str, &'static [u8])] {
        match self {
            RuntimeSet::Adapters => yoagent_rutis::adapters::files(),
            RuntimeSet::Ui => UI_FILES,
        }
    }

    /// Its npm projects, each `npm ci`ed.
    fn projects(self) -> &'static [&'static str] {
        match self {
            RuntimeSet::Adapters => &["pi", "dsh"],
            RuntimeSet::Ui => &["plugins"],
        }
    }

    /// The first 16 hex of a sha256 over its name and, sorted by path, each
    /// file's path and bytes.
    pub(crate) fn hash(self) -> String {
        let mut files: Vec<_> = self.files().to_vec();
        files.sort_by_key(|(path, _)| *path);
        let mut sha = Sha256::new();
        sha.update(self.name().as_bytes());
        for (path, bytes) in files {
            sha.update([0]);
            sha.update(path.as_bytes());
            sha.update([0]);
            sha.update((bytes.len() as u64).to_le_bytes());
            sha.update(bytes);
        }
        let mut hex: String = sha.finalize().iter().map(|b| format!("{b:02x}")).collect();
        hex.truncate(16);
        hex
    }
}

/// The words every failure to install a runtime set begins with (hint
/// `runtime-install`).
pub(crate) const INSTALLING: &str = "installing yobuild's runtime";

/// `set`'s folder under `home`, installed if it is not yet; `say` hears that
/// it is being installed (it takes a while: `npm ci`), and why when a
/// folder there is replaced. Node and npm are checked first.
pub fn installed(home: &Path, set: RuntimeSet, say: &dyn Fn(&str)) -> Result<PathBuf, BoxError> {
    installed_with(home, set, say, &|dir| {
        node_and_npm()?;
        npm_ci(dir)
    })
}

/// [`installed`], with `ci` installing one npm project (the tests' stand-in
/// for `npm ci`).
fn installed_with(
    home: &Path,
    set: RuntimeSet,
    say: &dyn Fn(&str),
    ci: &dyn Fn(&Path) -> Result<(), BoxError>,
) -> Result<PathBuf, BoxError> {
    let hash = set.hash();
    let dir = home.join("runtime").join(set.name()).join(&hash);
    if whole(&dir, set, &hash)? {
        return Ok(dir);
    }
    install_set(home, set, &hash, say, ci)
        .map_err(|e| format!("{INSTALLING} `{}` in {}: {e}", set.name(), dir.display()).into())
}

/// A stamped folder whose projects are all there with their
/// `node_modules`: one deleted from inside (a subfolder removed by hand)
/// is not taken for installed.
fn whole(dir: &Path, set: RuntimeSet, hash: &str) -> Result<bool, BoxError> {
    Ok(install::stamped(dir, hash)?
        && set
            .projects()
            .iter()
            .all(|project| dir.join(project).join("node_modules").is_dir()))
}

fn install_set(
    home: &Path,
    set: RuntimeSet,
    hash: &str,
    say: &dyn Fn(&str),
    ci: &dyn Fn(&Path) -> Result<(), BoxError>,
) -> Result<PathBuf, BoxError> {
    let parent = home.join("runtime").join(set.name());
    let dir = parent.join(hash);
    std::fs::create_dir_all(&parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    install::remove_old_partials(&parent);
    let partial = |what: &str| {
        parent.join(format!(
            "{hash}.partial-{what}{}",
            uuid::Uuid::new_v4().simple()
        ))
    };
    // A folder there that is not whole: changed by hand, or half made by a
    // yobuild that died. Moved aside whole, so nobody runs from half of it
    // — put back if it turns out another yobuild had just finished it.
    if dir.exists() && !whole(&dir, set, hash)? {
        say(&format!(
            "yobuild's runtime `{}` at {} is not whole (changed by hand?): installing it again",
            set.name(),
            dir.display()
        ));
        let aside = partial("old-");
        match std::fs::rename(&dir, &aside) {
            Ok(()) if whole(&aside, set, hash)? => {
                if std::fs::rename(&aside, &dir).is_err() {
                    install::remove_quietly(&aside, "a runtime another yobuild had finished");
                }
            }
            Ok(()) => install::remove_quietly(&aside, "a runtime folder that was not whole"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("moving {} aside: {e}", dir.display()).into()),
        }
    }
    // Another yobuild may have made it meanwhile.
    if whole(&dir, set, hash)? {
        return Ok(dir);
    }
    say(&format!(
        "{INSTALLING} `{}` (once per version; npm ci)",
        set.name()
    ));
    let fresh = partial("");
    if let Err(e) = make(&fresh, set, hash, ci) {
        install::remove_quietly(&fresh, "a runtime install that failed");
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&fresh, &dir) {
        install::remove_quietly(&fresh, "a runtime another yobuild finished first");
        if !whole(&dir, set, hash)? {
            return Err(format!("{}: {e}", dir.display()).into());
        }
    }
    Ok(dir)
}

/// Fills a fresh folder with `set`: its files and each project's `.npmrc`,
/// `ci` in each project, then the stamp.
fn make(
    dir: &Path,
    set: RuntimeSet,
    hash: &str,
    ci: &dyn Fn(&Path) -> Result<(), BoxError>,
) -> Result<(), BoxError> {
    for (path, bytes) in set.files() {
        let file = dir.join(path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        install::write(&file, bytes)?;
    }
    for project in set.projects() {
        let project = dir.join(project);
        install::write(&project.join(".npmrc"), NPMRC)?;
        ci(&project)?;
    }
    install::write(&dir.join(STAMP), format!("{hash}\n"))
}

/// The settings that decide what `npm ci` installs, on its command line
/// (they win over every config file; `install::npm` drops `npm_config_*`
/// variables): a person's `omit=optional` would leave esbuild's platform
/// package out, and a runtime that cannot start would be stamped. Unlike a
/// manifest's sets, install scripts run: yobuild's own, locked.
const NPM_CI: &[&str] = &[
    "ci", "--ignore-scripts=false", "--legacy-peer-deps=false", "--package-lock=true",
    "--install-strategy=hoisted", "--strict-peer-deps=false", "--install-links=false",
    "--include=dev", "--include=optional", "--include=peer", "--no-audit", "--no-fund",
    "--loglevel=error",
];

/// The same, for anyone running npm in the folder by hand.
const NPMRC: &str = "# Written by yobuild: the settings it installs its runtime with.\n\
ignore-scripts=false\n\
legacy-peer-deps=false\n\
package-lock=true\n\
install-strategy=hoisted\n\
strict-peer-deps=false\n\
install-links=false\n\
include[]=dev\n\
include[]=optional\n\
include[]=peer\n";

fn npm_ci(dir: &Path) -> Result<(), BoxError> {
    let args: Vec<String> = NPM_CI.iter().map(|a| a.to_string()).collect();
    install::npm(dir, &args).map(|_| ())
}

/// The oldest Node and npm yobuild runs with (design §13 "Node and npm,
/// checked first"): Node 22.19 for rutis's runtimes, npm 11.19 for the
/// locks a pinned DSH plugin's set needs.
const NODE_FLOOR: (u64, u64) = (22, 19);
const NPM_FLOOR: (u64, u64) = (11, 19);

/// How long `--version` may take (a version manager's shim may stall).
const VERSION_TIME: std::time::Duration = std::time::Duration::from_secs(10);

/// Node and npm are there and new enough. Checked once a process when they
/// are; a failure is checked again next time (one could be passing).
pub(crate) fn node_and_npm() -> Result<(), BoxError> {
    static PASSED: OnceLock<()> = OnceLock::new();
    if PASSED.get().is_some() {
        return Ok(());
    }
    check_versions(&version_of("node"), &version_of("npm"))?;
    let _ = PASSED.set(());
    Ok(())
}

/// What `<program> --version` printed, or why it could not be run: not on
/// PATH, failed (its first line of errors), silent, or too slow.
fn version_of(program: &str) -> Result<String, String> {
    use std::io::Read;
    let mut child = std::process::Command::new(program)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => format!("no {program} on PATH"),
            _ => format!("{program}: {e}"),
        })?;
    let deadline = std::time::Instant::now() + VERSION_TIME;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20))
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "`{program} --version` did not answer within {} s",
                    VERSION_TIME.as_secs()
                ));
            }
            Err(e) => return Err(format!("{program}: {e}")),
        }
    };
    let read = |pipe: Option<&mut dyn Read>| {
        let mut text = String::new();
        if let Some(pipe) = pipe {
            let _ = pipe.read_to_string(&mut text);
        }
        text
    };
    let stdout = read(child.stdout.as_mut().map(|p| p as &mut dyn Read));
    let stderr = read(child.stderr.as_mut().map(|p| p as &mut dyn Read));
    if !status.success() {
        let why = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        return Err(match why.is_empty() {
            true => format!("`{program} --version` failed"),
            false => format!("`{program} --version` failed: {}", why.trim()),
        });
    }
    Ok(stdout.trim().to_owned())
}

/// Whether the versions found meet the floors; the error says what was found.
pub(crate) fn check_versions(
    node: &Result<String, String>,
    npm: &Result<String, String>,
) -> Result<(), String> {
    // What to say of one program below its floor: its version, or why none.
    let below = |program: &str, found: &Result<String, String>, floor: (u64, u64)| {
        let text = match found {
            Err(why) => return Some(why.clone()),
            Ok(text) => text,
        };
        let mut parts = text.trim_start_matches('v').split(['.', '-']);
        let mut number = || parts.next().and_then(|n| n.parse::<u64>().ok());
        match (number(), number()) {
            (Some(major), Some(minor)) => {
                ((major, minor) < floor).then(|| format!("{program} {text}"))
            }
            _ if text.is_empty() => Some(format!("`{program} --version` printed nothing")),
            _ => Some(format!(
                "`{program} --version` printed no version ({text:?})"
            )),
        }
    };
    let mut found = Vec::new();
    found.extend(below("node", node, NODE_FLOOR));
    found.extend(below("npm", npm, NPM_FLOOR));
    match found.is_empty() {
        true => Ok(()),
        false => Err(format!(
            "yobuild needs Node {}.{}+ and npm {}.{}+: found {}",
            NODE_FLOOR.0,
            NODE_FLOOR.1,
            NPM_FLOOR.0,
            NPM_FLOOR.1,
            found.join(", ")
        )),
    }
}

/// A whole `set` under `home` (a stamp, each project's `node_modules`),
/// made without npm: for tests of what uses it.
#[cfg(test)]
pub(crate) fn seed(home: &Path, set: RuntimeSet) -> PathBuf {
    let hash = set.hash();
    let dir = home.join("runtime").join(set.name()).join(&hash);
    for project in set.projects() {
        std::fs::create_dir_all(dir.join(project).join("node_modules")).unwrap();
    }
    std::fs::write(dir.join(STAMP), format!("{hash}\n")).unwrap();
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::RefCell;

    /// A stand-in for `npm ci`: a `node_modules` made, each call counted.
    fn fake_ci(calls: &RefCell<usize>) -> impl Fn(&Path) -> Result<(), BoxError> + '_ {
        move |dir| {
            *calls.borrow_mut() += 1;
            std::fs::create_dir_all(dir.join("node_modules"))?;
            Ok(())
        }
    }

    /// Installed once (said), then used as it is (nothing said, no `ci`);
    /// a folder that is not whole — a project deleted from inside, no
    /// stamp — installed again (said why); a failed install leaves nothing,
    /// prefixed with what was being installed.
    #[test]
    fn a_runtime_set_is_installed_once_and_again_only_when_not_whole() {
        let home = tempfile::tempdir().unwrap();
        let said = RefCell::new(Vec::<String>::new());
        let say = |text: &str| said.borrow_mut().push(text.to_owned());
        let calls = RefCell::new(0);
        let ci = fake_ci(&calls);
        let dir = installed_with(home.path(), RuntimeSet::Ui, &say, &ci).unwrap();
        assert_eq!(
            dir,
            home.path().join("runtime/ui").join(RuntimeSet::Ui.hash())
        );
        assert!(dir.join("plugins/terminal-ui.ts").is_file() && dir.join("web/lib.js").is_file());
        assert!(dir.join("plugins/.npmrc").is_file());
        assert_eq!((*calls.borrow(), said.borrow().len()), (1, 1));
        assert!(said.borrow()[0].starts_with("installing yobuild's runtime `ui`"));
        // Whole: used as it is.
        assert_eq!(
            installed_with(home.path(), RuntimeSet::Ui, &say, &ci).unwrap(),
            dir
        );
        assert_eq!((*calls.borrow(), said.borrow().len()), (1, 1));
        // A project deleted from inside: installed again, said why.
        std::fs::remove_dir_all(dir.join("plugins")).unwrap();
        installed_with(home.path(), RuntimeSet::Ui, &say, &ci).unwrap();
        assert!(dir.join("plugins/node_modules").is_dir());
        assert_eq!(*calls.borrow(), 2);
        assert!(
            said.borrow()[1].contains("is not whole (changed by hand?)"),
            "{:?}",
            said.borrow()
        );
        // No stamp: the same.
        std::fs::remove_file(dir.join(STAMP)).unwrap();
        installed_with(home.path(), RuntimeSet::Ui, &say, &ci).unwrap();
        assert_eq!(*calls.borrow(), 3);
        // A failure: nothing left, not even a partial folder.
        let other = tempfile::tempdir().unwrap();
        let failing = |_: &Path| -> Result<(), BoxError> { Err("npm said no".into()) };
        let e = installed_with(other.path(), RuntimeSet::Ui, &say, &failing)
            .unwrap_err()
            .to_string();
        assert!(
            e.starts_with("installing yobuild's runtime `ui` in "),
            "{e}"
        );
        assert!(e.ends_with(": npm said no"), "{e}");
        let left: Vec<_> = std::fs::read_dir(other.path().join("runtime/ui"))
            .unwrap()
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[test]
    fn node_and_npm_below_their_floors_are_said() {
        let ok = |s: &str| Ok(s.to_owned());
        assert_eq!(check_versions(&ok("v24.7.0"), &ok("11.19.1")), Ok(()));
        assert_eq!(check_versions(&ok("v22.19.0"), &ok("11.19.0")), Ok(()));
        assert_eq!(
            check_versions(&ok("v20.11.0"), &ok("10.2.4")),
            Err("yobuild needs Node 22.19+ and npm 11.19+: found node v20.11.0, npm 10.2.4".into())
        );
        assert_eq!(
            check_versions(&ok("v22.18.0"), &ok("11.20.0")),
            Err("yobuild needs Node 22.19+ and npm 11.19+: found node v22.18.0".into())
        );
        assert_eq!(
            check_versions(&Err("no node on PATH".into()), &ok("11.19.0")),
            Err("yobuild needs Node 22.19+ and npm 11.19+: found no node on PATH".into())
        );
        // Pre-releases read; unreadable or empty output said as such.
        assert_eq!(
            check_versions(&ok("v22.19.0-nightly2026"), &ok("11.19.0-pre")),
            Ok(())
        );
        assert_eq!(
            check_versions(&ok(""), &ok("eleven")),
            Err("yobuild needs Node 22.19+ and npm 11.19+: found `node --version` printed nothing, `npm --version` printed no version (\"eleven\")".into())
        );
    }

    /// The `ui` set holds the files the repository's frontend has, and the
    /// hash changes with any file but not with their order.
    #[test]
    fn the_sets_hold_their_files_and_hash_them() {
        let frontend = Path::new(env!("CARGO_MANIFEST_DIR")).join("frontend");
        for (path, bytes) in UI_FILES {
            assert_eq!(
                std::fs::read(frontend.join(path)).unwrap(),
                *bytes,
                "{path}"
            );
        }
        let adapters = RuntimeSet::Adapters.files();
        for want in [
            "pi/pi-extensions-adapter.ts", "dsh/dsh-tools-adapter.ts", "yoagent.d.ts",
        ] {
            assert!(adapters.iter().any(|(p, _)| *p == want), "{want}");
        }
        assert!(!adapters.iter().any(|(p, _)| p.contains("fixture-")));
        assert_eq!(RuntimeSet::Ui.hash().len(), 16);
        assert_ne!(RuntimeSet::Ui.hash(), RuntimeSet::Adapters.hash());
        assert_eq!(RuntimeSet::Ui.hash(), RuntimeSet::Ui.hash());
    }
}
