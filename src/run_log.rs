//! The run log (design §16): one JSON line per run, appended when the run
//! ends, under yobuild's home — what a run was asked, how it ended, what it
//! cost, the tools it called, the facts, the questions left waiting on the
//! owner, and its report (the last answer). Read by `yobuild log` and the
//! web page's past runs.
//!
//! `<home>/logs/<agent>-<16 hex>/runs.jsonl` (the hex: a sha256 of the
//! manifest's canonical path, so two agents of one name keep their own),
//! the folder 0700 and the file 0600 (unix). Each text is cut to a bound
//! ([`TASK_CHARS`], [`REPORT_CHARS`], …), and the file is rotated to
//! `runs.1.jsonl` past [`ROTATE_AT`] bytes (one kept), under an advisory
//! lock ([`LOCK`]) so two yobuilds on one agent rotate it once. Never
//! logged: a tool's arguments and results, notices but the facts, the
//! prompt; every text written has the credentials' values shown as
//! `${NAME}` ([`Hide`]) before it is cut.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read as _, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::frontend::RunOutcome;
use crate::manifest::Manifest;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The entries' version: a line of another is skipped by [`RunLog::read`].
pub const VERSION: u32 = 1;
/// The log's file, and the one it is rotated to.
pub const FILE: &str = "runs.jsonl";
pub const ROTATED: &str = "runs.1.jsonl";
/// The advisory lock held around a rotation and an append (unix).
pub const LOCK: &str = "runs.lock";
/// The size past which the file is rotated before an append: with the one
/// rotated, at least 2 × this of history (a run's line is at most some
/// 40 KB, its texts cut as below).
pub const ROTATE_AT: u64 = 2 * 1024 * 1024;
/// The bounds, in characters: the task, the report (the last answer), an
/// error and the facts, each question waiting; how many questions and
/// tools are kept.
pub const TASK_CHARS: usize = 2_000;
pub const REPORT_CHARS: usize = 16_000;
pub const LINE_CHARS: usize = 2_000;
pub const QUESTION_CHARS: usize = 500;
pub const QUESTIONS: usize = 20;
pub const TOOLS: usize = 100;
/// The shortest credential value hidden: a shorter one (a region, a port)
/// is no secret, and replacing it would garble the text around it.
const HIDDEN_AT_LEAST: usize = 8;

/// One run, as logged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Entry {
    /// [`VERSION`].
    pub v: u32,
    /// When it started and ended, `YYYY-MM-DDTHH:MM:SSZ` (UTC), and how
    /// long it took.
    pub started: String,
    pub ended: String,
    pub seconds: f64,
    /// The agent's name and its manifest's path.
    pub agent: String,
    pub manifest: String,
    /// The folder it worked in: with `manifest`, the session `--resume`
    /// continues there (design §15).
    #[serde(default)]
    pub folder: String,
    /// The session's id and the run's number in it.
    pub session: String,
    pub run: u64,
    /// `once`, `web` or `terminal`; whether the model's replies came from
    /// a file (`--scripted`).
    pub mode: String,
    pub scripted: bool,
    /// What started it: the task, the owner's prompt, or a steer (an
    /// answer, a plugin's wake).
    pub task: String,
    pub outcome: RunOutcome,
    pub error: Option<String>,
    /// yoagent's `total_cost_usd` (`None`: not priced, or no turn); the
    /// tokens (in: input + cache reads + cache writes) and turns.
    pub cost_usd: Option<f64>,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub turns: u32,
    /// Each tool called, by name: how many calls succeeded and failed.
    pub tools: Vec<Tools>,
    /// The `facts` built-in's line, without its `Facts: `.
    pub facts: Option<String>,
    /// `ask_owner`'s questions open as the run ended (`#N question`).
    pub waiting: Vec<String>,
    /// The run's last answer.
    pub report: Option<String>,
}

/// A tool's calls in a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Tools {
    pub name: String,
    pub ok: u32,
    pub failed: u32,
}

impl Entry {
    /// How it ended, as `--once` says it: `Completed`, `Stopped: …`.
    pub fn outcome_line(&self) -> String {
        match &self.error {
            Some(error) => format!("{:?}: {error}", self.outcome),
            None => format!("{:?}", self.outcome),
        }
    }

    /// Its cost: `$0.0021`, `<$0.0001`, `not priced`; with no turn,
    /// `nothing spent`, or `unknown` for a run aborted or failed (its
    /// figures were lost with it).
    pub fn cost_line(&self) -> String {
        if self.turns == 0 {
            return match self.outcome {
                RunOutcome::Aborted | RunOutcome::Error => "unknown".to_owned(),
                _ => "nothing spent".to_owned(),
            };
        }
        match self.cost_usd {
            Some(usd) if usd > 0.0 && usd < 0.00005 => "<$0.0001".to_owned(),
            Some(usd) => format!("${usd:.4}"),
            None => "not priced".to_owned(),
        }
    }
}

/// A run as it is being seen, made an [`Entry`] when it ends.
#[derive(Debug, Default)]
pub(crate) struct Seeing {
    pub started: String,
    pub since: Option<std::time::Instant>,
    pub run: u64,
    pub task: String,
    pub tools: BTreeMap<String, (u32, u32)>,
    pub facts: Option<String>,
    pub answer: Option<String>,
}

impl Seeing {
    /// A tool's call ended.
    pub fn tool(&mut self, name: &str, failed: bool) {
        if !self.tools.contains_key(name) && self.tools.len() >= TOOLS {
            return;
        }
        let counts = self.tools.entry(name.to_owned()).or_default();
        match failed {
            true => counts.1 += 1,
            false => counts.0 += 1,
        }
    }
}

/// What every entry of a session shares.
#[derive(Debug, Clone)]
pub(crate) struct Base {
    pub agent: String,
    pub manifest: String,
    pub folder: String,
    pub session: String,
    pub mode: &'static str,
    pub scripted: bool,
}

/// How a run ended, from its `RunEnd`.
pub(crate) struct Ending<'a> {
    pub outcome: RunOutcome,
    pub error: Option<&'a str>,
    pub stats: &'a yoagent::SessionStats,
    pub total_cost_usd: Option<f64>,
    pub waiting: Vec<String>,
}

/// `seeing`, ended, as an entry; each text with the credentials hidden
/// (`hide`), then cut to its bound — hidden first, so a credential the cut
/// would split is never left in part.
pub(crate) fn entry(base: &Base, seeing: Seeing, ending: Ending, hide: &Hide) -> Entry {
    let usage = ending.stats.total_usage();
    let text = |text: &str, max: usize| cut(&hide.hidden(text), max);
    Entry {
        v: VERSION,
        started: seeing.started,
        ended: now(),
        seconds: seeing
            .since
            .map(|s| (s.elapsed().as_secs_f64() * 10.0).round() / 10.0)
            .unwrap_or_default(),
        agent: base.agent.clone(),
        manifest: base.manifest.clone(),
        folder: base.folder.clone(),
        session: base.session.clone(),
        run: seeing.run,
        mode: base.mode.to_owned(),
        scripted: base.scripted,
        task: text(&seeing.task, TASK_CHARS),
        outcome: ending.outcome,
        error: ending.error.map(|e| text(e, LINE_CHARS)),
        cost_usd: ending.total_cost_usd,
        tokens_in: usage.input + usage.cache_read + usage.cache_write,
        tokens_out: usage.output,
        turns: ending.stats.turns,
        tools: seeing
            .tools
            .into_iter()
            .map(|(name, (ok, failed))| Tools { name, ok, failed })
            .collect(),
        facts: seeing.facts.map(|f| text(&f, LINE_CHARS)),
        waiting: ending
            .waiting
            .iter()
            .take(QUESTIONS)
            .map(|q| text(q, QUESTION_CHARS))
            .collect(),
        report: seeing.answer.map(|a| text(&a, REPORT_CHARS)),
    }
}

/// Now, `YYYY-MM-DDTHH:MM:SSZ`; a clock before 1970 reads as 1970.
pub(crate) fn now() -> String {
    crate::builtins::find_plugin::iso(crate::builtins::find_plugin::now().unwrap_or_default())
}

/// `text` cut to `max` characters, its lines kept: what is cut is said on
/// a line of its own.
pub(crate) fn cut(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_owned();
    }
    let kept: String = text.chars().take(max).collect();
    format!(
        "{}\n… (cut: {} more characters)",
        kept.trim_end(),
        count - max
    )
}

/// The credentials `load_plugin` takes during a session, `(name, value)`:
/// a remote server's header filled from the variable its owner named.
/// Shared with the log's [`Hide`], which hides each from then on.
pub(crate) type Credentials = Arc<Mutex<Vec<(String, String)>>>;

/// The credentials' values a log line must not hold, each shown as its
/// `${NAME}` instead; longest first, so a value inside another is not
/// replaced first.
#[derive(Debug, Clone, Default)]
pub struct Hide {
    fixed: Vec<(String, String)>,
    taken: Credentials,
}

impl Hide {
    /// From this process's environment, each of 8 characters or more: the
    /// model's key variables (`Manifest::key_variables`); every variable
    /// the manifest hands out, whatever its name — a `${NAME}` its plugins'
    /// `config` or its servers' URLs, headers, `env` or `args` refer to,
    /// a name or prefix (`AWS_*`) of an `env` grant; and every variable
    /// whose name reads as a secret's (`KEY`, `TOKEN`, `SECRET`,
    /// `PASSWORD`, …).
    pub fn from_env(manifest: &Manifest) -> Self {
        let (names, prefixes) = handed_out(manifest);
        Self::of(std::env::vars_os().filter_map(|(name, value)| {
            let name = name.into_string().ok()?;
            let value = value.into_string().ok()?;
            let hidden = names.contains(&name)
                || prefixes.iter().any(|p| name.starts_with(p.as_str()))
                || crate::manifest::secret(&name);
            hidden.then_some((name, value))
        }))
    }

    /// From `(name, value)` pairs (each one a credential's), each of 8
    /// characters or more.
    pub fn of(vars: impl IntoIterator<Item = (String, String)>) -> Self {
        let mut fixed: Vec<(String, String)> = vars
            .into_iter()
            .filter(|(_, value)| value.chars().count() >= HIDDEN_AT_LEAST)
            .map(|(name, value)| (value, format!("${{{name}}}")))
            .collect();
        fixed.sort_by_key(|(value, _)| std::cmp::Reverse(value.len()));
        fixed.dedup_by(|a, b| a.0 == b.0);
        Self {
            fixed,
            taken: Credentials::default(),
        }
    }

    /// With the credentials `load_plugin` takes this session, each hidden
    /// whatever its length (as `load_plugin` hides them in what a server
    /// says).
    pub(crate) fn with_taken(mut self, taken: Credentials) -> Self {
        self.taken = taken;
        self
    }

    /// `text` with each value replaced by its `${NAME}`.
    pub fn hidden(&self, text: &str) -> String {
        let mut all = self.fixed.clone();
        all.extend(
            self.taken
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .filter(|(_, value)| !value.is_empty())
                .map(|(name, value)| (value.clone(), format!("${{{name}}}"))),
        );
        all.sort_by_key(|(value, _)| std::cmp::Reverse(value.len()));
        all.iter().fold(text.to_owned(), |text, (value, shown)| {
            text.replace(value.as_str(), shown)
        })
    }
}

/// The variables `manifest` hands out, by name and by prefix (an `env`
/// grant's `AWS_*` as `AWS_`): the model's keys, its plugins' `config`
/// references and `env` grants, its servers' references (URL, headers,
/// `env`, `args`).
fn handed_out(manifest: &Manifest) -> (BTreeSet<String>, Vec<String>) {
    use crate::manifest::Transport;
    let mut names: BTreeSet<String> = manifest.key_variables().into_iter().collect();
    let mut prefixes = Vec::new();
    for plugin in &manifest.plugins {
        names.extend(plugin.config_references());
        for granted in &plugin.grants.env {
            match granted.strip_suffix('*') {
                Some(prefix) => prefixes.push(prefix.to_owned()),
                None => {
                    names.insert(granted.clone());
                }
            }
        }
    }
    for server in &manifest.mcp {
        let values: Vec<&crate::manifest::Value> = match &server.transport {
            Transport::Stdio { args, env, .. } => args.iter().chain(env.values()).collect(),
            Transport::Http { url, headers, .. } => {
                std::iter::once(url).chain(headers.values()).collect()
            }
        };
        names.extend(
            values
                .into_iter()
                .flat_map(|v| v.references())
                .map(str::to_owned),
        );
    }
    (names, prefixes)
}

/// An agent's run log: its folder under yobuild's home.
#[derive(Debug, Clone)]
pub struct RunLog {
    dir: PathBuf,
}

impl RunLog {
    /// The log of the agent at `manifest`, under `home` (`None`:
    /// `YOBUILD_HOME`, else `~/.yobuild`). Nothing is made yet.
    pub fn of(manifest: &Manifest, home: Option<&Path>) -> Result<Self, BoxError> {
        let home = match home {
            Some(home) => home.to_owned(),
            None => crate::install::home()?,
        };
        // The manifest's own path, links resolved: one agent, one log,
        // however its path is spelt. A file gone since it was read keeps
        // the path as read.
        let path = std::fs::canonicalize(&manifest.path).unwrap_or_else(|_| manifest.path.clone());
        let hash = Sha256::digest(path.as_os_str().as_encoded_bytes());
        let hex: String = hash.iter().take(8).map(|b| format!("{b:02x}")).collect();
        Ok(Self {
            dir: home
                .join("logs")
                .join(format!("{}-{hex}", manifest.agent.name)),
        })
    }

    /// The log's folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file appended to.
    pub fn file(&self) -> PathBuf {
        self.dir.join(FILE)
    }

    /// Appends `entry` as one line: the folder made (0700) if missing;
    /// under the folder's lock ([`LOCK`]), the file (0600) rotated first
    /// when past [`ROTATE_AT`], and a line break written first when the
    /// file does not end with one (a line a crash tore keeps to itself).
    /// Errors name the path.
    pub fn append(&self, entry: &Entry) -> Result<(), BoxError> {
        let failed = |path: &Path, e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
        let mut line = serde_json::to_string(entry)?;
        line.push('\n');
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder
            .create(&self.dir)
            .map_err(|e| failed(&self.dir, &e))?;
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true).read(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        // Held until this returns: another yobuild's rotation and append
        // wait for this one's, so the file is rotated once.
        let lock = self.dir.join(LOCK);
        let held = options.open(&lock).map_err(|e| failed(&lock, &e))?;
        hold(&held).map_err(|e| failed(&lock, &e))?;
        let file = self.file();
        match std::fs::metadata(&file) {
            Ok(meta) if meta.len() >= ROTATE_AT => {
                let rotated = self.dir.join(ROTATED);
                match std::fs::rename(&file, &rotated) {
                    // Rotated already, by a yobuild that held no lock.
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    other => other.map_err(|e| failed(&rotated, &e))?,
                }
            }
            _ => {}
        }
        let mut out = options.open(&file).map_err(|e| failed(&file, &e))?;
        if !ends_a_line(&mut out).map_err(|e| failed(&file, &e))? {
            line.insert(0, '\n');
        }
        // One write: lines of runs that end together do not interleave.
        out.write_all(line.as_bytes())
            .map_err(|e| failed(&file, &e))?;
        Ok(())
    }

    /// Its last `n` entries, oldest first (the rotated file's, then the
    /// file's), and how many lines could not be read (another version, or
    /// broken). No log yet: none.
    pub fn read(&self, n: usize) -> Result<Read, BoxError> {
        let mut read = Read::default();
        for name in [ROTATED, FILE] {
            let path = self.dir.join(name);
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(format!("{}: {e}", path.display()).into()),
            };
            // Line by line, as bytes: a line torn mid-character (a crash
            // during a write) is one unread line, not an unreadable log.
            for line in bytes.split(|&b| b == b'\n') {
                if line.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                match serde_json::from_slice::<Entry>(line) {
                    Ok(entry) if entry.v == VERSION => read.entries.push(entry),
                    _ => read.unread += 1,
                }
            }
        }
        let extra = read.entries.len().saturating_sub(n);
        read.entries.drain(..extra);
        Ok(read)
    }
}

/// Whether `file` is empty or ends with a line break.
fn ends_a_line(file: &mut std::fs::File) -> std::io::Result<bool> {
    if file.metadata()?.len() == 0 {
        return Ok(true);
    }
    file.seek(std::io::SeekFrom::End(-1))?;
    let mut last = [0u8];
    file.read_exact(&mut last)?;
    Ok(last[0] == b'\n')
}

/// Takes `file`'s advisory lock, waiting for it; it goes when the file is
/// closed. None off unix.
#[cfg(unix)]
fn hold(file: &std::fs::File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    loop {
        // SAFETY: a valid descriptor, owned by `file` for the call.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

#[cfg(not(unix))]
fn hold(_: &std::fs::File) -> std::io::Result<()> {
    Ok(())
}

/// What [`RunLog::read`] found.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct Read {
    pub entries: Vec<Entry>,
    /// Lines skipped: written by another version, or not an entry.
    pub unread: usize,
}

/// `entries` as `yobuild log` prints them: one block a run, oldest first —
/// when, how it ended, its cost, then its task, tools, the questions
/// waiting, the facts, and the report's first `lines` lines.
pub fn text(entries: &[Entry], lines: usize) -> String {
    let mut out = String::new();
    for e in entries {
        let when = e.started.replace('T', " ").replace('Z', " UTC");
        out.push_str(&format!(
            "{when}  run {} ({})  {}  {}  {:.0}s\n",
            e.run,
            e.mode,
            e.outcome_line(),
            e.cost_line(),
            e.seconds
        ));
        out.push_str(&format!("  task: {}\n", one_line(&e.task, 100)));
        if !e.tools.is_empty() {
            let tools: Vec<String> = e
                .tools
                .iter()
                .map(|t| match (t.ok, t.failed) {
                    (ok, 0) => format!("{} {ok}✓", t.name),
                    (0, failed) => format!("{} {failed}✗", t.name),
                    (ok, failed) => format!("{} {ok}✓ {failed}✗", t.name),
                })
                .collect();
            out.push_str(&format!("  tools: {}\n", tools.join(", ")));
        }
        for question in &e.waiting {
            out.push_str(&format!("  waiting on you: {}\n", one_line(question, 100)));
        }
        if let Some(facts) = &e.facts {
            out.push_str(&format!("  facts: {}\n", one_line(facts, 200)));
        }
        if let Some(report) = &e.report {
            let mut shown = report.lines().filter(|l| !l.trim().is_empty());
            out.push_str("  report:\n");
            for line in shown.by_ref().take(lines) {
                out.push_str(&format!("    {}\n", one_line(line, 100)));
            }
            if shown.next().is_some() {
                out.push_str("    …\n");
            }
        }
    }
    out
}

/// `text` on one line, cut to `max` characters with an ellipsis.
fn one_line(text: &str, max: usize) -> String {
    crate::run::host::clip(text, max)
}

#[cfg(test)]
mod tests;
