//! `scratch`: the agent's own working folder.

use std::io;
use std::path::{Component, Path, PathBuf};

/// A folder for what the agent makes for itself (downloads, helper scripts,
/// notes to itself), kept out of the owner's. Name it in the system prompt,
/// and to the approval gate, which lets the agent delete and overwrite there
/// freely: [`Scratch::ENV`] in the environment of the plugin runtime the gate
/// runs in (`PluginHostBuilder::env`).
///
/// The folder is left in place when the agent ends, so what it gathered can
/// still be looked at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scratch {
    path: PathBuf,
}

impl Scratch {
    /// The environment variable the approval gate reads.
    pub const ENV: &'static str = "YOBUILD_SCRATCH";

    /// A new, empty folder in the temp dir, named `<name>-<random>`. `name`
    /// is one plain path component (no `/`, `..`).
    pub fn create(name: &str) -> io::Result<Self> {
        let mut parts = Path::new(name).components();
        if !matches!(
            (parts.next(), parts.next()),
            (Some(Component::Normal(_)), None)
        ) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("a scratch name must be one plain path component, not {name:?}"),
            ));
        }
        let dir = tempfile::Builder::new()
            .prefix(&format!("{name}-"))
            .tempdir()
            .map_err(|e| context(e, &std::env::temp_dir().join(format!("{name}-…"))))?;
        Ok(Self { path: dir.keep() })
    }

    /// `path`, created if it is not there yet (an existing folder is used as
    /// it is), made absolute. Refused: a file system root, the home folder,
    /// and the current folder or one holding it — the gate would let the
    /// agent delete there freely.
    pub fn at(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        std::fs::create_dir_all(&path).map_err(|e| context(e, &path))?;
        let path = path.canonicalize().map_err(|e| context(e, &path))?;
        let home = std::env::var_os("HOME").and_then(|h| PathBuf::from(h).canonicalize().ok());
        let cwd = std::env::current_dir()
            .ok()
            .and_then(|d| d.canonicalize().ok());
        let too_wide = path.parent().is_none()
            || home.as_deref() == Some(path.as_path())
            || cwd.is_some_and(|cwd| cwd.starts_with(&path));
        if too_wide {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is too wide for a scratch folder", path.display()),
            ));
        }
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn context(e: io::Error, path: &Path) -> io::Error {
    io::Error::new(
        e.kind(),
        format!("cannot make the scratch folder {}: {e}", path.display()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_makes_a_new_empty_folder_each_time() {
        let a = Scratch::create("yobuild-scratch-test").unwrap();
        let b = Scratch::create("yobuild-scratch-test").unwrap();
        assert_ne!(a, b);
        for s in [&a, &b] {
            assert!(s.path().is_dir());
            assert!(s.path().read_dir().unwrap().next().is_none());
            let name = s.path().file_name().unwrap().to_string_lossy().into_owned();
            assert!(name.starts_with("yobuild-scratch-test-"), "{name}");
            std::fs::remove_dir_all(s.path()).unwrap();
        }
    }

    #[test]
    fn create_refuses_a_name_that_is_a_path() {
        for bad in ["", "a/b", "..", "/abs", "."] {
            assert!(Scratch::create(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn at_creates_missing_parents_accepts_an_existing_folder_and_is_absolute() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/b");
        let scratch = Scratch::at(&nested).unwrap();
        assert!(scratch.path().is_absolute());
        assert_eq!(scratch.path(), nested.canonicalize().unwrap());
        assert!(Scratch::at(&nested).is_ok());
    }

    #[test]
    fn at_refuses_a_folder_too_wide() {
        assert!(Scratch::at("/").is_err());
        assert!(Scratch::at(std::env::current_dir().unwrap()).is_err());
        if let Some(home) = std::env::var_os("HOME") {
            assert!(Scratch::at(home).is_err());
        }
    }
}
