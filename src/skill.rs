//! `yobuild skill install` (design §13 "New commands"): the yobuild skill,
//! built into the binary, written where a coding agent reads it — the
//! instructions a coding agent follows to build an agent for someone (ask,
//! pick parts, write the manifest, `check`, run scripted, then live).
//!
//! By default `~/.claude/skills/yobuild/` (Claude Code's skills, for every
//! project), `./.claude/skills/yobuild/` with `--project`, or any folder
//! with `--dir` (the skill is plain Markdown: another coding agent can be
//! pointed at it). A stamp (`.yobuild-skill`) lists each file written and
//! its hash, so a later yobuild replaces a copy nobody edited (an upgrade)
//! and refuses, naming them, to overwrite files someone changed — unless
//! `--force`, which writes the folder afresh.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

macro_rules! file {
    ($path:literal) => {
        (
            $path,
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/skill/yobuild/",
                $path
            )) as &[u8],
        )
    };
}

/// The skill's files, by path in its folder.
pub(crate) const FILES: &[(&str, &[u8])] = &[
    file!("SKILL.md"),
    file!("reference/manifest.md"),
    file!("reference/parts.md"),
    file!("reference/scripted.md"),
    file!("reference/failures.md"),
];

/// The stamp: a line per file written, `<sha256> <path>`, after a line
/// naming the yobuild that wrote it.
const STAMP: &str = ".yobuild-skill";

/// Where to write it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// `~/.claude/skills/yobuild/`.
    User,
    /// `./.claude/skills/yobuild/`.
    Project,
    /// A folder of one's own: the skill's folder itself.
    Dir(PathBuf),
}

impl Target {
    /// The skill's folder.
    pub fn folder(&self) -> Result<PathBuf, BoxError> {
        Ok(match self {
            Target::User => {
                let home = std::env::var_os("HOME")
                    .filter(|h| !h.is_empty())
                    .ok_or("HOME is not set: give the folder with --dir")?;
                Path::new(&home).join(".claude/skills/yobuild")
            }
            Target::Project => std::env::current_dir()?.join(".claude/skills/yobuild"),
            Target::Dir(dir) => std::path::absolute(dir)?,
        })
    }
}

/// What `install` did.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Done {
    /// Written where there was none.
    Installed,
    /// A copy nobody edited, replaced; the yobuild that wrote it.
    Updated { from: String },
    /// The same files there already.
    AlreadyThere,
    /// Written afresh over what was there (`--force`).
    Replaced,
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The stamp's text for the files written.
fn stamp_text() -> String {
    let mut text = format!("yobuild {}\n", env!("CARGO_PKG_VERSION"));
    for (path, bytes) in FILES {
        text.push_str(&format!("{} {path}\n", sha(bytes)));
    }
    text
}

/// A stamp's yobuild and its files' hashes; `None` when it is not one.
fn read_stamp(text: &str) -> Option<(String, BTreeMap<String, String>)> {
    let mut lines = text.lines();
    let version = lines.next()?.strip_prefix("yobuild ")?.to_owned();
    let mut files = BTreeMap::new();
    for line in lines {
        let (hash, path) = line.split_once(' ')?;
        files.insert(path.to_owned(), hash.to_owned());
    }
    Some((version, files))
}

/// Every file under `dir`, by path relative to it, but the stamp.
fn files_in(dir: &Path) -> Result<Vec<String>, BoxError> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        for entry in std::fs::read_dir(&at).map_err(|e| format!("{}: {e}", at.display()))? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path.strip_prefix(dir).unwrap_or(&path);
                let rel = rel.to_string_lossy().replace('\\', "/");
                if rel != STAMP {
                    out.push(rel);
                }
            }
        }
    }
    out.sort();
    Ok(out)
}

/// The skill written into `dir` (the skill's own folder): see the module's
/// documentation for what is replaced, refused and `force`d.
pub fn install(dir: &Path, force: bool) -> Result<Done, BoxError> {
    if dir.exists() && !dir.is_dir() {
        return Err(format!(
            "{} is not a folder: give the skill's own folder",
            dir.display()
        )
        .into());
    }
    let mut done = Done::Installed;
    // Files an earlier yobuild wrote that this one does not ship: removed.
    // Nothing else is ever removed: `--dir` names any folder.
    let mut stale: Vec<String> = Vec::new();
    if dir.exists() {
        let stamp = std::fs::read_to_string(dir.join(STAMP)).ok();
        match stamp.as_deref().and_then(read_stamp) {
            None if !dir.join("SKILL.md").is_file() => {
                return Err(format!(
                    "{} holds no yobuild skill (no {STAMP}, no SKILL.md): give the skill's own folder, such as ~/.claude/skills/yobuild",
                    dir.display()
                )
                .into());
            }
            // A copy made by hand (an older yobuild's way): replaced only
            // when asked.
            None if !force => {
                return Err(format!(
                    "{}: a skill yobuild did not install (no {STAMP}): give --force to write its files over it",
                    dir.display()
                )
                .into());
            }
            None => done = Done::Replaced,
            Some((from, written)) => {
                // Changed since written: a file edited, removed, or added.
                let there = files_in(dir)?;
                let mut changed: Vec<String> = there
                    .iter()
                    .filter(|path| match written.get(*path) {
                        None => true,
                        Some(hash) => {
                            std::fs::read(dir.join(path)).map_or(true, |b| sha(&b) != *hash)
                        }
                    })
                    .cloned()
                    .collect();
                changed.extend(written.keys().filter(|p| !there.contains(p)).cloned());
                if !changed.is_empty() && !force {
                    return Err(format!(
                        "{}: changed since yobuild {from} wrote it ({}): give --force to write the skill's files over them (files of your own are left)",
                        dir.display(),
                        changed.join(", ")
                    )
                    .into());
                }
                let same = FILES.len() == written.len()
                    && FILES
                        .iter()
                        .all(|(path, bytes)| written.get(*path) == Some(&sha(bytes)));
                if same && changed.is_empty() {
                    return Ok(Done::AlreadyThere);
                }
                stale = written
                    .keys()
                    .filter(|path| !FILES.iter().any(|(p, _)| p == path))
                    .cloned()
                    .collect();
                done = match changed.is_empty() {
                    true => Done::Updated { from },
                    false => Done::Replaced,
                };
            }
        }
    }
    for path in &stale {
        let file = dir.join(path);
        if let Err(e) = std::fs::remove_file(&file) {
            if e.kind() != std::io::ErrorKind::NotFound {
                return Err(format!("{}: {e}", file.display()).into());
            }
        }
    }
    for (path, bytes) in FILES {
        let file = dir.join(path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(&file, bytes).map_err(|e| format!("{}: {e}", file.display()))?;
    }
    std::fs::write(dir.join(STAMP), stamp_text())
        .map_err(|e| format!("{}: {e}", dir.join(STAMP).display()))?;
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_skill_is_the_repositorys() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("skill/yobuild");
        let listed: Vec<String> = FILES.iter().map(|(p, _)| p.to_string()).collect();
        let mut sorted = listed.clone();
        sorted.sort();
        assert_eq!(sorted, files_in(&dir).unwrap());
        for (path, bytes) in FILES {
            assert_eq!(std::fs::read(dir.join(path)).unwrap(), *bytes, "{path}");
        }
    }

    /// Installed, then there already; an unedited copy of another version
    /// updated; an edited one refused, naming its files, then replaced with
    /// `force`; a folder yobuild did not write refused.
    #[test]
    fn the_skill_is_installed_updated_and_never_overwrites_an_edit() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("yobuild");
        assert_eq!(install(&dir, false).unwrap(), Done::Installed);
        assert!(dir.join("SKILL.md").is_file() && dir.join("reference/parts.md").is_file());
        assert_eq!(install(&dir, false).unwrap(), Done::AlreadyThere);
        // An older yobuild's copy, unedited: its stamp names other content.
        let old = "abc";
        std::fs::write(dir.join("reference/parts.md"), old).unwrap();
        let stamp = stamp_text()
            .replace(
                &format!("yobuild {}", env!("CARGO_PKG_VERSION")),
                "yobuild 0.0.0",
            )
            .replace(
                &format!("{} reference/parts.md", sha(FILES[2].1)),
                &format!("{} reference/parts.md", sha(old.as_bytes())),
            );
        std::fs::write(dir.join(STAMP), stamp).unwrap();
        assert_eq!(
            install(&dir, false).unwrap(),
            Done::Updated {
                from: "0.0.0".into()
            }
        );
        assert_eq!(
            std::fs::read(dir.join("reference/parts.md")).unwrap(),
            FILES[2].1
        );
        // Edited, and a file added: refused, naming both.
        std::fs::write(dir.join("SKILL.md"), "mine").unwrap();
        std::fs::write(dir.join("notes.md"), "mine too").unwrap();
        let e = install(&dir, false).unwrap_err().to_string();
        assert!(e.contains("(SKILL.md, notes.md): give --force"), "{e}");
        assert_eq!(
            std::fs::read_to_string(dir.join("SKILL.md")).unwrap(),
            "mine"
        );
        // --force: the skill's files written over; a file of one's own left.
        assert_eq!(install(&dir, true).unwrap(), Done::Replaced);
        assert_eq!(std::fs::read(dir.join("SKILL.md")).unwrap(), FILES[0].1);
        assert_eq!(
            std::fs::read_to_string(dir.join("notes.md")).unwrap(),
            "mine too"
        );
        std::fs::remove_file(dir.join("notes.md")).unwrap();
        assert_eq!(install(&dir, false).unwrap(), Done::AlreadyThere);
        // A file an older yobuild wrote and this one does not ship: removed.
        std::fs::write(dir.join("reference/old.md"), "old").unwrap();
        let stamp = std::fs::read_to_string(dir.join(STAMP)).unwrap()
            + &format!("{} reference/old.md\n", sha(b"old"));
        std::fs::write(dir.join(STAMP), stamp).unwrap();
        assert!(matches!(
            install(&dir, false).unwrap(),
            Done::Updated { .. }
        ));
        assert!(!dir.join("reference/old.md").exists());
        // A folder holding no skill — a project, a home, the skills' parent —
        // is never written into, `--force` or not.
        let other = parent.path().join("project");
        std::fs::create_dir(&other).unwrap();
        std::fs::write(other.join("main.rs"), "fn main() {}").unwrap();
        for force in [false, true] {
            let e = install(&other, force).unwrap_err().to_string();
            assert!(e.contains("holds no yobuild skill"), "{e}");
        }
        assert_eq!(std::fs::read_dir(&other).unwrap().count(), 1);
        // A copy made by hand: replaced only with --force.
        let copy = parent.path().join("copy");
        std::fs::create_dir(&copy).unwrap();
        std::fs::write(copy.join("SKILL.md"), "an old copy").unwrap();
        let e = install(&copy, false).unwrap_err().to_string();
        assert!(e.contains("a skill yobuild did not install"), "{e}");
        assert_eq!(install(&copy, true).unwrap(), Done::Replaced);
        assert_eq!(std::fs::read(copy.join("SKILL.md")).unwrap(), FILES[0].1);
        // A file removed since written: changed, refused.
        let removed = parent.path().join("removed");
        assert_eq!(install(&removed, false).unwrap(), Done::Installed);
        std::fs::remove_file(removed.join("reference/scripted.md")).unwrap();
        let e = install(&removed, false).unwrap_err().to_string();
        assert!(e.contains("(reference/scripted.md)"), "{e}");
        // A stamp that is not one: a copy yobuild did not install.
        std::fs::write(removed.join(STAMP), "garbage").unwrap();
        let e = install(&removed, false).unwrap_err().to_string();
        assert!(e.contains("a skill yobuild did not install"), "{e}");
        // A file, not a folder.
        let file = parent.path().join("file");
        std::fs::write(&file, "x").unwrap();
        assert!(install(&file, true)
            .unwrap_err()
            .to_string()
            .contains("is not a folder"));
    }
}
