//! `scratch`: the agent's own working folder.

use std::path::{Path, PathBuf};

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
    pub const ENV: &'static str = "YOAGENT_SCRATCH";

    /// `<temp dir>/<name>-<pid>`, created.
    pub fn create(name: &str) -> std::io::Result<Self> {
        Self::at(std::env::temp_dir().join(format!("{name}-{}", std::process::id())))
    }

    /// `path`, created if it is not there yet.
    pub fn at(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_makes_a_folder_named_for_the_agent_and_process() {
        let scratch = Scratch::create("yobuild-scratch-test").unwrap();
        assert!(scratch.path().is_dir());
        let name = scratch.path().file_name().unwrap().to_string_lossy();
        assert_eq!(name, format!("yobuild-scratch-test-{}", std::process::id()));
        std::fs::remove_dir_all(scratch.path()).unwrap();
    }

    #[test]
    fn at_creates_missing_parents_and_accepts_an_existing_folder() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/b");
        assert_eq!(Scratch::at(&nested).unwrap().path(), nested);
        assert!(Scratch::at(&nested).is_ok());
    }
}
