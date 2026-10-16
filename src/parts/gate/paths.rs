//! Where paths land: the shell's folder (logical and real), resolution as
//! the kernel does it, the names the gate protects, a file's identity.

use std::path::{Component, Path, PathBuf};

/// Where a shell is: its logical path (what `$PWD` says, what `cd ..`
/// works on) and the real folder (where relative paths land).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Here {
    logical: PathBuf,
    physical: PathBuf,
}

impl Here {
    /// The logical path (what `$PWD` says).
    #[cfg(test)]
    pub fn logical(&self) -> &Path {
        &self.logical
    }

    /// The real folder, where relative paths land.
    pub fn physical(&self) -> &Path {
        &self.physical
    }

    /// `cwd`; logically `pwd` (the inherited `$PWD`) when it names the same
    /// folder, as bash takes it; otherwise the real folder (`getcwd`).
    pub fn start(cwd: &Path, pwd: Option<&Path>) -> Self {
        let physical = real(cwd);
        let logical = pwd
            .filter(|pwd| pwd.is_absolute() && real(pwd) == physical)
            .map(Path::to_owned)
            .unwrap_or_else(|| physical.clone());
        Self { logical, physical }
    }

    /// Where `cd to` lands, as bash's (logical) `cd` does it: `..` on the
    /// logical path (`link/..` is where `link` sits), then the real folder.
    pub fn cd(&self, to: &str) -> Self {
        let logical = lexical(&self.logical, to);
        Self {
            physical: real(&logical),
            logical,
        }
    }
}

/// `file` against `dir` as the kernel resolves it: a `..` applies to the
/// path so far with its symlinks followed.
pub(super) fn locate(dir: &Path, file: &str) -> PathBuf {
    let mut out = dir.to_owned();
    for part in Path::new(file).components() {
        match part {
            Component::Prefix(_) | Component::RootDir => out = PathBuf::from(part.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                out = real(&out);
                out.pop();
            }
            Component::Normal(name) => out.push(name),
        }
    }
    real(&out)
}

/// `path` with symlinks resolved, also for a file not created yet — and a
/// dangling symlink resolved to where it points (a write through it lands
/// there).
pub(super) fn real(path: &Path) -> PathBuf {
    real_within(path, 40)
}

fn real_within(path: &Path, hops: usize) -> PathBuf {
    if let Ok(real) = path.canonicalize() {
        return real;
    }
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return path.to_owned();
    };
    if parent == path {
        return path.to_owned();
    }
    let here = real_within(parent, hops).join(name);
    match std::fs::read_link(&here) {
        Ok(target) if hops > 0 => {
            real_within(&here.parent().unwrap_or(&here).join(target), hops - 1)
        }
        _ => here,
    }
}

/// `file` against `dir` as text: `..` drops the component before it,
/// whatever it is (bash's logical `cd`).
fn lexical(dir: &Path, file: &str) -> PathBuf {
    let mut out = dir.to_owned();
    for part in Path::new(file).components() {
        match part {
            Component::Prefix(_) | Component::RootDir => out = PathBuf::from(part.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(name) => out.push(name),
        }
    }
    out
}

/// A path with a component starting `.env` (`.env`, `.env.local`, …) or
/// named `.git`, in any case (macOS and Windows file systems ignore it).
pub(super) fn protected(path: &Path) -> bool {
    path.components().any(|c| match c {
        Component::Normal(name) => {
            let name = name.to_string_lossy().to_lowercase();
            name.starts_with(".env") || name == ".git"
        }
        _ => false,
    })
}

/// A program named by its path runs freely only from a system folder: a
/// file the agent wrote (`./cat`, `tmp/ls`) is not the program it is named
/// after.
pub(super) fn system_binary(path: &str) -> bool {
    const SYSTEM: [&str; 7] = [
        "/bin/", "/usr/bin/", "/usr/local/bin/", "/opt/homebrew/bin/", "/usr/sbin/", "/sbin/",
        "/usr/local/sbin/",
    ];
    let resolved = real(Path::new(path));
    let resolved = resolved.to_string_lossy();
    Path::new(path).is_absolute() && SYSTEM.iter().any(|dir| resolved.starts_with(dir))
}

/// Which file a path names: device and inode, and when that file was made.
/// A filesystem reuses a deleted file's inode (Linux's do at once), so the
/// inode alone could make a file the owner wrote in place of the agent's
/// pass for the agent's. Its birth time tells them apart; where the
/// filesystem keeps none, its change time does — which an edit of the
/// agent's own file changes too, so that file then asks: more, never less.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Identity {
    dev: u64,
    ino: u64,
    made: (i64, i64),
}

/// A file's identity (not following a symlink at the end).
#[cfg(unix)]
pub(super) fn identity(path: &Path) -> Option<Identity> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::symlink_metadata(path).ok()?;
    let made = match m.created() {
        Ok(born) => match born.duration_since(std::time::UNIX_EPOCH) {
            Ok(after) => (after.as_secs() as i64, i64::from(after.subsec_nanos())),
            Err(before) => (-(before.duration().as_secs() as i64), 0),
        },
        Err(_) => (m.ctime(), m.ctime_nsec()),
    };
    Some(Identity {
        dev: m.dev(),
        ino: m.ino(),
        made,
    })
}

/// Elsewhere no identity is known: nothing is ever the agent's own, so the
/// gate asks more, never less.
#[cfg(not(unix))]
pub(super) fn identity(_path: &Path) -> Option<Identity> {
    None
}

/// A folder, or a file with one name only.
#[cfg(unix)]
pub(super) fn single(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir() || m.nlink() == 1)
}

#[cfg(not(unix))]
pub(super) fn single(_path: &Path) -> bool {
    false
}
