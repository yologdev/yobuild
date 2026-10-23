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

/// `set`'s folder under `home`, installed if it is not yet; `say` hears that
/// it is being installed (it takes a while: `npm ci`).
pub fn installed(home: &Path, set: RuntimeSet, say: &dyn Fn(&str)) -> Result<PathBuf, BoxError> {
    let hash = set.hash();
    let parent = home.join("runtime").join(set.name());
    let dir = parent.join(&hash);
    if install::stamped(&dir, &hash)? {
        return Ok(dir);
    }
    node_and_npm()?;
    std::fs::create_dir_all(&parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    install::remove_old_partials(&parent);
    let partial = |what: &str| {
        parent.join(format!(
            "{hash}.partial-{what}{}",
            uuid::Uuid::new_v4().simple()
        ))
    };
    // A folder there without its stamp: changed by hand, or half made by a
    // yobuild that died. Moved aside whole, so nobody runs from half of it.
    if dir.exists() && !install::stamped(&dir, &hash)? {
        let aside = partial("old-");
        match std::fs::rename(&dir, &aside) {
            Ok(()) => install::remove_quietly(&aside, "a runtime folder changed by hand"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", dir.display()).into()),
        }
    }
    say(&format!(
        "installing yobuild's runtime `{}` (once per version; npm ci)",
        set.name()
    ));
    let fresh = partial("");
    if let Err(e) = make(&fresh, set, &hash) {
        install::remove_quietly(&fresh, "a runtime install that failed");
        return Err(format!("installing yobuild's runtime `{}`: {e}", set.name()).into());
    }
    if let Err(e) = std::fs::rename(&fresh, &dir) {
        install::remove_quietly(&fresh, "a runtime another yobuild finished first");
        if !install::stamped(&dir, &hash)? {
            return Err(format!("{}: {e}", dir.display()).into());
        }
    }
    Ok(dir)
}

/// Fills a fresh folder with `set`: its files, `npm ci` in each project,
/// then the stamp.
fn make(dir: &Path, set: RuntimeSet, hash: &str) -> Result<(), BoxError> {
    for (path, bytes) in set.files() {
        let file = dir.join(path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        install::write(&file, bytes)?;
    }
    let args: Vec<String> = ["ci", "--no-audit", "--no-fund", "--loglevel=error"]
        .map(String::from)
        .to_vec();
    for project in set.projects() {
        install::npm(&dir.join(project), &args)?;
    }
    install::write(&dir.join(STAMP), format!("{hash}\n"))
}

/// The oldest Node and npm yobuild runs with (design §13 "Node and npm,
/// checked first"): Node 22.19 for rutis's runtimes, npm 11.19 for the
/// locks a pinned DSH plugin's set needs.
const NODE_FLOOR: (u64, u64) = (22, 19);
const NPM_FLOOR: (u64, u64) = (11, 19);

/// Node and npm are there and new enough; checked once a process.
pub(crate) fn node_and_npm() -> Result<(), BoxError> {
    static CHECKED: OnceLock<Result<(), String>> = OnceLock::new();
    CHECKED
        .get_or_init(|| check_versions(&version_of("node"), &version_of("npm")))
        .clone()
        .map_err(Into::into)
}

/// What `<program> --version` printed, or why it could not be run.
fn version_of(program: &str) -> Result<String, String> {
    let output = std::process::Command::new(program)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => format!("no {program} on PATH"),
            _ => format!("{program}: {e}"),
        })?;
    match output.status.success() {
        true => Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned()),
        false => Err(format!("`{program} --version` failed")),
    }
}

/// Whether the versions found meet the floors; the error says what was found.
pub(crate) fn check_versions(
    node: &Result<String, String>,
    npm: &Result<String, String>,
) -> Result<(), String> {
    // What to say of one program below its floor: its version, or why none.
    let below = |program: &str, found: &Result<String, String>, floor: (u64, u64)| match found {
        Err(why) => Some(why.clone()),
        Ok(text) => {
            let numbers: Vec<u64> = text
                .trim_start_matches('v')
                .split('.')
                .take(2)
                .map(|n| n.parse().unwrap_or(0))
                .collect();
            let have = (
                numbers.first().copied().unwrap_or(0),
                numbers.get(1).copied().unwrap_or(0),
            );
            (have < floor).then(|| format!("{program} {text}"))
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

#[cfg(test)]
mod tests {
    use super::*;

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
