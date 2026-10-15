//! Judging a command line. Every command goes through the same steps: its
//! substitutions and redirections, then [`invocation`] (what runs), then the
//! program's [`catalogue`] entry (its options and what it does with its
//! arguments). Every effect — writing, appending, deleting, making, running
//! shell text — is judged by one [`Ctx`] rule.

use std::path::{Path, PathBuf};

use super::catalogue::{self, Entry};
use super::invocation::{invocation, Unread};
use super::parse::{parse, Item, Word};
use super::paths::{locate, Here};
use super::Rules;

/// The gate needs the owner's yes.
#[derive(Debug)]
pub(super) struct Ask;

pub(super) type Verdict = Result<(), Ask>;

impl From<Unread> for Ask {
    fn from(_: Unread) -> Self {
        Ask
    }
}

/// Turns a condition into a verdict.
pub(super) fn ok_if(free: bool) -> Verdict {
    match free {
        true => Ok(()),
        false => Err(Ask),
    }
}

/// Where a redirection is harmless.
const DEVICES: [&str; 4] = ["/dev/null", "/dev/stdout", "/dev/stderr", "/dev/tty"];

/// What a command line may do, judged as it is read.
pub(super) struct Ctx<'r> {
    rules: &'r Rules,
    /// Where the shell is; `None` once a `cd` cannot be followed.
    cwd: Option<Here>,
    /// Files it creates: the agent's own once the call has run.
    creates: Vec<PathBuf>,
    /// Names linked earlier in this line: a write through one lands
    /// elsewhere, so they are unknown for the rest of it.
    linked: Vec<PathBuf>,
}

impl Ctx<'_> {
    /// Where `w` lands: only for a literal word in a folder that is known.
    pub fn path(&self, w: &Word) -> Result<PathBuf, Ask> {
        match &self.cwd {
            Some(here) if w.known => Ok(locate(&here.physical, &w.text)),
            _ => Err(Ask),
        }
    }

    /// Never through a link made earlier in the line, never a protected name.
    fn plain(&self, path: &Path) -> bool {
        !self.rules.protected(path) && !self.linked.iter().any(|l| path.starts_with(l))
    }

    /// Inside the project, or the agent's own.
    fn in_reach(&self, path: &Path) -> bool {
        path.starts_with(&self.rules.start.physical) || self.rules.free(path)
    }

    fn created(&mut self, path: PathBuf) {
        if !path.exists() {
            self.creates.push(path);
        }
    }

    /// Writing (replacing) `w`: a new file, the agent's own, or in its scratch.
    pub fn write(&mut self, w: &Word) -> Verdict {
        if DEVICES.contains(&w.text.as_str()) {
            return Ok(());
        }
        let path = self.path(w)?;
        self.write_path(path)
    }

    pub fn write_path(&mut self, path: PathBuf) -> Verdict {
        ok_if(self.plain(&path) && !self.rules.owners(&path))?;
        self.created(path);
        Ok(())
    }

    /// Appending to `w`, or making it (a folder, an empty file) or touching
    /// it: inside the project or the agent's own.
    pub fn add_to(&mut self, w: &Word) -> Verdict {
        if DEVICES.contains(&w.text.as_str()) {
            return Ok(());
        }
        let path = self.path(w)?;
        ok_if(self.plain(&path) && self.in_reach(&path))?;
        self.created(path);
        Ok(())
    }

    /// Changing `w`'s mode or owner: inside the project or the agent's own.
    pub fn change(&self, w: &Word) -> Verdict {
        let path = self.path(w)?;
        ok_if(self.plain(&path) && self.in_reach(&path))
    }

    /// Deleting `w` (or moving it away): only the agent's own.
    pub fn delete(&self, w: &Word) -> Verdict {
        let path = self.path(w)?;
        ok_if(self.plain(&path) && self.rules.free(&path))
    }

    /// A folder something is put into whole (`rsync`, `cp -R`, `git clone`):
    /// one not there yet, or the agent's own.
    pub fn new_or_own(&mut self, w: &Word) -> Verdict {
        let path = self.path(w)?;
        ok_if(self.plain(&path) && (!path.exists() || self.rules.free(&path)))?;
        self.created(path);
        Ok(())
    }

    /// A file of the owner's to run (a script, a test file): inside the
    /// project, there, and not made by the agent.
    pub fn owners_file(&self, w: &Word) -> Verdict {
        let path = self.path(w)?;
        ok_if(
            path.is_file()
                && path.starts_with(&self.rules.start.physical)
                && !self.rules.free(&path)
                && self.plain(&path),
        )
    }

    /// The agent's own (its files, its scratch).
    pub fn free(&self, path: &Path) -> bool {
        self.rules.free(path)
    }

    /// A name `w` is linked to: unknown for the rest of the line.
    pub fn link(&mut self, w: &Word) -> Verdict {
        let path = self.path(w)?;
        self.linked.push(path);
        Ok(())
    }

    /// Shell text run where this command runs (`bash -c`, `eval`, `trap`,
    /// substitutions).
    pub fn run(&mut self, text: &str) -> Verdict {
        let here = self.cwd.clone().ok_or(Ask)?;
        let made = line(self.rules, text, &here)?;
        self.creates.extend(made);
        Ok(())
    }

    /// A command, as its words: what runs, by its catalogue entry. With
    /// `readers_only`, only a program none of whose options can write or
    /// run (`find -exec`).
    pub fn command(&mut self, words: &[Word], readers_only: bool) -> Verdict {
        let Some(run) = invocation(words)? else {
            return Ok(());
        };
        let entry = catalogue::entry(&run.name).ok_or(Ask)?;
        // What `xargs` runs gets arguments the line does not show.
        let readers_only = readers_only || run.via_xargs;
        match entry {
            Entry::Reader => Ok(()),
            _ if readers_only => Err(Ask),
            Entry::Spec(spec, judge) => {
                let parsed = super::args::parse(&spec, run.args).ok_or(Ask)?;
                judge(&parsed, self)
            }
            Entry::Raw(judge) => judge(run.args, self),
        }
    }
}

/// Whether `text` needs the owner's yes (`Err`), and the files it creates.
pub(super) fn line(rules: &Rules, text: &str, start: &Here) -> Result<Vec<PathBuf>, Ask> {
    let items = parse(text).ok_or(Ask)?;
    let mut ctx = Ctx {
        rules,
        cwd: Some(start.clone()),
        creates: Vec::new(),
        linked: Vec::new(),
    };
    // Folders to return to when a `( … )` subshell ends.
    let mut saved: Vec<Option<Here>> = Vec::new();
    let mut before = ";".to_owned();
    for item in items {
        let command = match item {
            Item::Open => {
                saved.push(ctx.cwd.clone());
                continue;
            }
            Item::Close => {
                ctx.cwd = saved.pop().ok_or(Ask)?;
                continue;
            }
            Item::Command(command) => command,
        };
        let piped = before == "|" || command.after == "|";
        before = command.after.clone();
        // What its substitutions run, where it runs.
        for inner in &command.runs {
            ctx.run(inner)?;
        }
        for w in &command.truncates {
            ctx.write(w)?;
        }
        for w in &command.appends {
            ctx.add_to(w)?;
        }
        if let Some(to) = cd(&command.words)? {
            // Followed only when it surely applies to what comes next: a
            // literal folder that exists, not in a pipeline or the
            // background (a subshell), not before `||` (run if it failed).
            let next = match (to, &ctx.cwd) {
                (Some(to), Some(here)) => Some(here.cd(&to.text)),
                _ => None,
            };
            let after = command.after.as_str();
            ctx.cwd =
                next.filter(|d| d.physical.is_dir() && !piped && after != "&" && after != "||");
            continue;
        }
        ctx.command(&command.words, false)?;
    }
    Ok(ctx.creates)
}

/// A `cd`, `pushd` or `popd`: `Some(Some(folder))` for `cd FOLDER` with a
/// literal folder, `Some(None)` for one the gate does not follow.
fn cd(words: &[Word]) -> Result<Option<Option<&Word>>, Ask> {
    let Some(run) = invocation(words)? else {
        return Ok(None);
    };
    Ok(match run.name.as_str() {
        "cd" => Some(match run.args {
            [to] if to.known && !to.text.starts_with('-') => Some(to),
            _ => None,
        }),
        "pushd" | "popd" => Some(None),
        _ => None,
    })
}
