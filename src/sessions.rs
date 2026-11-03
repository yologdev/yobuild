//! Sessions that survive a restart (design §15): each session's
//! conversation, open `ask_owner` questions and spend, saved under
//! yobuild's home per agent and folder, and given back with `yobuild run
//! --resume`.
//!
//! One file, `<home>/sessions/<agent>-<16 hex>/session.json` (0600, its
//! folder 0700), written whole (a temp file, then a rename) after each
//! run, after a reset and as the session ends; a `session.lock` beside it
//! held (`flock`) while a session saves there.

use std::collections::HashSet;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use yoagent::{Agent, AgentMessage};

use crate::builtins::{AskOwner, Question};
use crate::frontend::{NoticeLevel, Session, Settled};
use crate::manifest::Manifest;
use crate::run::spend::SessionSpend;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The format this yobuild writes and reads.
pub(crate) const FORMAT: u32 = 1;

/// The saved session's file in its folder.
const FILE: &str = "session.json";

/// Held while a session saves into its folder.
const LOCK: &str = "session.lock";

/// The session a new one replaced, kept once (design §15).
const PREVIOUS: &str = "session.prev.json";

/// What is saved.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Saved {
    pub version: u32,
    /// The canonical paths it is keyed by, for a person reading the folder.
    pub manifest: PathBuf,
    pub cwd: PathBuf,
    /// Of the manifest's text, to say it changed.
    pub manifest_sha256: String,
    /// `provider/id`, as the manifest names it.
    pub model: String,
    /// When, `YYYY-MM-DDTHH:MM:SSZ`.
    pub saved: String,
    /// The agent's history, as yoagent serializes it.
    pub messages: Vec<AgentMessage>,
    /// What the session spent, as its budget counts it (`None`: not priced).
    pub spent_usd: Option<f64>,
    pub questions: Questions,
}

/// The `ask_owner` questions not answered yet, and the last number given.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Questions {
    pub next: u64,
    pub open: Vec<OpenQuestion>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OpenQuestion {
    pub n: u64,
    pub question: String,
    #[serde(default)]
    pub why: String,
    #[serde(default)]
    pub options: Vec<String>,
}

/// The agent's folder under `home`: named after the agent, keyed by the
/// manifest's and the working folder's canonical paths.
pub(crate) fn folder(home: &Path, name: &str, manifest: &Path, cwd: &Path) -> PathBuf {
    let hash = crate::install::sha256_hex(&[
        manifest.as_os_str().as_encoded_bytes(),
        cwd.as_os_str().as_encoded_bytes(),
    ]);
    home.join("sessions")
        .join(format!("{name}-{}", &hash[..16]))
}

/// The manifest's canonical path and its text's sha256.
fn manifest_of(manifest: &Manifest) -> Result<(PathBuf, String), BoxError> {
    let file = manifest.dir.join(crate::install::file_name(&manifest.path));
    let path = std::fs::canonicalize(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let text = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((path, crate::install::sha256_hex(&[&text])))
}

/// `file` read: its version first (a later one said as such, whatever its
/// shape), then strictly.
fn read(file: &Path) -> Result<Option<Saved>, String> {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("not JSON ({e})"))?;
    match value.get("version").and_then(serde_json::Value::as_u64) {
        Some(v) if v > u64::from(FORMAT) => {
            return Err(format!(
                "saved by a later yobuild (session format {v}; this one reads {FORMAT})"
            ))
        }
        Some(_) => {}
        None => return Err("no `version`: not a session yobuild saved".into()),
    }
    serde_json::from_value(value)
        .map(Some)
        .map_err(|e| format!("not a saved session ({e})"))
}

/// Where a session saves, its folder's lock held for as long as it lives.
pub(crate) struct Store {
    dir: PathBuf,
    _lock: File,
    /// A new session's: the saved one is moved to `session.prev.json`
    /// before its first save replaces it.
    keep_previous: bool,
}

impl Store {
    pub(crate) fn file(&self) -> PathBuf {
        self.dir.join(FILE)
    }

    /// Where the session a new one replaced is kept.
    pub(crate) fn previous(&self) -> PathBuf {
        self.dir.join(PREVIOUS)
    }

    /// `saved` written whole: a temp file (0600) in the folder, flushed to
    /// the disk, renamed — a crash leaves the last whole one. A new
    /// session's first save keeps the one it replaces as
    /// `session.prev.json`.
    pub(crate) fn write(&mut self, saved: &Saved) -> Result<(), String> {
        use std::io::Write;
        let file = self.file();
        let failed = |e: &dyn std::fmt::Display| format!("{}: {e}", file.display());
        let text = serde_json::to_vec(saved).map_err(|e| failed(&e))?;
        let mut temp = tempfile::NamedTempFile::new_in(&self.dir).map_err(|e| failed(&e))?;
        private(temp.path(), 0o600).map_err(|e| failed(&e))?;
        temp.write_all(&text).map_err(|e| failed(&e))?;
        temp.as_file().sync_all().map_err(|e| failed(&e))?;
        if self.keep_previous {
            match std::fs::rename(&file, self.previous()) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    return Err(format!("{}: {e}", self.previous().display()))
                }
                _ => self.keep_previous = false,
            }
        }
        temp.persist(&file).map_err(|e| failed(&e.error))?;
        Ok(())
    }
}

/// `path`'s mode (unix; elsewhere nothing).
fn private(path: &Path, mode: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

/// `file` locked for this process, or `false` when another holds it.
fn lock(file: &File) -> std::io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: the descriptor is `file`'s, open for the call.
        match unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } {
            0 => Ok(true),
            _ => match std::io::Error::last_os_error() {
                e if e.raw_os_error() == Some(libc::EWOULDBLOCK) => Ok(false),
                e => Err(e),
            },
        }
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        Ok(true)
    }
}

/// What a session opens with: where it saves (`None`: unsaved), the saved
/// session it resumes, and what to say as it starts.
pub(crate) struct Opened {
    pub store: Option<Store>,
    pub resumed: Option<Saved>,
    pub notes: Vec<(NoticeLevel, String)>,
    /// The manifest's canonical path and sha256, the working folder's.
    pub manifest: PathBuf,
    pub manifest_sha256: String,
    pub cwd: PathBuf,
}

/// The session's store for `manifest` in `cwd` under `home` (else
/// yobuild's home), and the saved session when `resume`. Refused: `resume`
/// with `[session] save = false`, a saved file that does not read, or
/// another yobuild holding the folder.
pub(crate) fn open(
    manifest: &Manifest,
    cwd: &Path,
    home: Option<&Path>,
    resume: bool,
) -> Result<Opened, BoxError> {
    let file_name = crate::install::file_name(&manifest.path);
    if !manifest.session.save {
        if resume {
            return Err(format!(
                "{file_name}'s `[session] save = false`: no session is saved to resume"
            )
            .into());
        }
        return Ok(Opened {
            store: None,
            resumed: None,
            notes: Vec::new(),
            manifest: PathBuf::new(),
            manifest_sha256: String::new(),
            cwd: cwd.to_owned(),
        });
    }
    let (path, sha) = manifest_of(manifest)?;
    let cwd = std::fs::canonicalize(cwd).map_err(|e| format!("{}: {e}", cwd.display()))?;
    let mut opened = Opened {
        store: None,
        resumed: None,
        notes: Vec::new(),
        manifest: path,
        manifest_sha256: sha,
        cwd,
    };
    // No home, or a folder that cannot be made: unsaved (said), unless the
    // session was to resume.
    let store = match home
        .map(|h| Ok(h.to_owned()))
        .unwrap_or_else(crate::install::home)
        .map_err(|e| e.to_string())
        .and_then(|home| {
            let dir = folder(&home, &manifest.agent.name, &opened.manifest, &opened.cwd);
            make(&dir).map(|lock| (dir, lock))
        }) {
        Ok(store) => store,
        Err(e) if resume => return Err(format!("cannot resume: {e}").into()),
        Err(e) => {
            opened.notes.push((
                NoticeLevel::Warning,
                format!("This session is not saved: {e}."),
            ));
            return Ok(opened);
        }
    };
    let (dir, lock) = store;
    let Some(lock) = lock else {
        if resume {
            return Err("another yobuild runs this agent in this folder: stop it first, or run without --resume (this session is then not saved)".into());
        }
        opened.notes.push((
            NoticeLevel::Warning,
            "Another yobuild runs this agent in this folder: this session is not saved (that one is).".into(),
        ));
        return Ok(opened);
    };
    let file = dir.join(FILE);
    match (read(&file), resume) {
        (Ok(Some(saved)), true) => opened.resumed = Some(saved),
        (Ok(None), true) => opened.notes.push((
            NoticeLevel::Info,
            "No session of this agent in this folder was saved: a new one starts.".into(),
        )),
        (Err(e), true) => {
            return Err(format!(
                "cannot resume {}: {e}: move it aside, or run without --resume (a new session then replaces it when its first run ends)",
                file.display()
            )
            .into())
        }
        (Ok(Some(saved)), false) => opened.notes.push((
            NoticeLevel::Info,
            format!(
                "A session of this agent in this folder was saved {} ({} messages): `--resume` continues it instead. This new session replaces it when its first run ends, keeping it as {} (renamed to session.json, `--resume` continues it).",
                saved.saved,
                saved.messages.len(),
                dir.join(PREVIOUS).display()
            ),
        )),
        (Err(e), false) => opened.notes.push((
            NoticeLevel::Info,
            format!(
                "A session of this agent in this folder was saved, and does not read ({e}): this new session replaces it when its first run ends, keeping it as {}.",
                dir.join(PREVIOUS).display()
            ),
        )),
        (Ok(None), false) => {}
    }
    let keep_previous = !resume && file.exists();
    opened.store = Some(Store {
        dir,
        _lock: lock,
        keep_previous,
    });
    Ok(opened)
}

/// The folder made (0700) and its lock taken: `None` when another holds it.
fn make(dir: &Path) -> Result<Option<File>, String> {
    let failed = |e: &dyn std::fmt::Display| format!("{}: {e}", dir.display());
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir).map_err(|e| failed(&e))?;
    // Made before (by an older yobuild, or by hand): private all the same.
    private(dir, 0o700).map_err(|e| failed(&e))?;
    let path = dir.join(LOCK);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    private(&path, 0o600).map_err(|e| format!("{}: {e}", path.display()))?;
    match lock(&file) {
        Ok(true) => Ok(Some(file)),
        Ok(false) => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// What to say as a saved session resumes: when it was saved and what does
/// not come back; another model, a changed manifest; the spend so far.
pub(crate) fn resumed_notes(
    saved: &Saved,
    manifest: &Manifest,
    sha: &str,
    budget: Option<f64>,
) -> Vec<(NoticeLevel, String)> {
    let mut lost = vec!["what the page showed before".to_owned()];
    if manifest.extend.is_some() {
        lost.push("the plugins the agent loaded (it may load them again)".into());
    }
    let remote = manifest
        .extend
        .as_ref()
        .is_some_and(|e| e.kinds.contains(&crate::manifest::ExtendKind::Remote));
    if remote || manifest.mcp.iter().any(|m| m.oauth()) {
        lost.push("the sign-ins (each signs in again)".into());
    }
    if manifest.builtins.facts {
        lost.push("facts' snapshot (taken again at this session's first run)".into());
    }
    if !manifest.plugins.is_empty() {
        lost.push("the plugins' own state".into());
    }
    let mut notes = vec![(
        NoticeLevel::Info,
        format!(
            "Resumed the session saved {} ({} messages). Not resumed: {}.",
            saved.saved,
            saved.messages.len(),
            lost.join("; ")
        ),
    )];
    let model = format!(
        "{}/{}",
        manifest.agent.model.provider(),
        manifest.agent.model.id()
    );
    if saved.model != model {
        notes.push((
            NoticeLevel::Warning,
            format!(
                "This session was saved with {}; it goes on with {model}.",
                saved.model
            ),
        ));
    }
    if saved.manifest_sha256 != sha {
        let name = crate::install::file_name(&manifest.path);
        notes.push((
            NoticeLevel::Warning,
            format!("{name} changed since this session was saved: it goes on under {name} as it is now."),
        ));
    }
    if let Some(spent) = saved.spent_usd.filter(|s| *s > 0.0) {
        let of = budget
            .map(|b| format!(" (its budget: ${b:.2})"))
            .unwrap_or_default();
        notes.push((
            NoticeLevel::Info,
            format!("This session has spent ${spent:.4} so far{of}."),
        ));
    }
    notes
}

/// Saves the session as it goes: after each run and a reset (the driver's
/// [`Settled`]), and as it ends. A new session saves only once a run of
/// its own ended, so opening one and closing it again keeps the saved one.
pub(crate) struct Saver {
    store: Store,
    session: Session,
    manifest: PathBuf,
    cwd: PathBuf,
    manifest_sha256: String,
    model: String,
    ask: Option<AskOwner>,
    spend: Option<SessionSpend>,
    /// A resumed session's spend, when this one has no price to count with.
    spent_before: Option<f64>,
    /// A resumed session's last question number, when this one has no
    /// `ask_owner` to go on numbering: never given twice.
    next_before: u64,
    /// Resumed, or a run of this session ended.
    live: bool,
    /// The messages in the last save (a resumed session's, until then):
    /// what a session ended mid-run is saved with.
    last: Vec<AgentMessage>,
    /// Whether the last save was written.
    written: bool,
    /// Failures said, once each.
    said: HashSet<String>,
}

impl Saver {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        store: Store,
        session: Session,
        manifest: &Path,
        manifest_sha256: &str,
        cwd: &Path,
        model: String,
        ask: Option<AskOwner>,
        spend: Option<SessionSpend>,
        resumed: Option<&Saved>,
    ) -> Self {
        Self {
            store,
            session,
            manifest: manifest.to_owned(),
            cwd: cwd.to_owned(),
            manifest_sha256: manifest_sha256.to_owned(),
            model,
            ask,
            spend,
            spent_before: resumed.and_then(|s| s.spent_usd),
            next_before: resumed.map_or(0, |s| s.questions.next),
            live: resumed.is_some(),
            last: resumed.map(|s| s.messages.clone()).unwrap_or_default(),
            written: false,
            said: HashSet::new(),
        }
    }

    /// The saved session's file.
    pub(crate) fn file(&self) -> PathBuf {
        self.store.file()
    }

    /// The saved file, when this session's last save was written with a
    /// conversation in it.
    pub(crate) fn saved(&self) -> Option<PathBuf> {
        (self.written && !self.last.is_empty()).then(|| self.file())
    }

    /// The driver's callback: saved once this session is live. A reset
    /// (idle, or ending a run) withdraws the open questions: they are
    /// taken off here, not left to each question's own task.
    pub(crate) fn settled(&mut self, agent: &Agent, why: Settled) {
        match why {
            Settled::Run => self.live = true,
            Settled::Reset => {
                if let Some(ask) = &self.ask {
                    ask.forget_open();
                }
            }
        }
        if self.live {
            self.save(agent.messages().to_vec());
        }
    }

    /// As the session ends, with the agent the driver returned. None (Ctrl+C,
    /// SIGHUP or SIGTERM mid-run, a failed task): the run in progress is
    /// lost, but what it spent and the questions open now are saved over
    /// the last save's conversation.
    pub(crate) fn ending(&mut self, agent: Option<&Agent>) {
        if !self.live {
            return;
        }
        let messages = match agent {
            Some(agent) => agent.messages().to_vec(),
            None => self.last.clone(),
        };
        self.save(messages);
    }

    fn save(&mut self, messages: Vec<AgentMessage>) {
        let questions = match &self.ask {
            Some(ask) => Questions {
                next: ask.last_number().max(self.next_before),
                open: ask
                    .open()
                    .into_iter()
                    .map(|(n, q)| OpenQuestion {
                        n,
                        question: q.question().to_owned(),
                        why: q.why().to_owned(),
                        options: q.options().to_vec(),
                    })
                    .collect(),
            },
            None => Questions {
                next: self.next_before,
                open: Vec::new(),
            },
        };
        let saved = Saved {
            version: FORMAT,
            manifest: self.manifest.clone(),
            cwd: self.cwd.clone(),
            manifest_sha256: self.manifest_sha256.clone(),
            model: self.model.clone(),
            saved: now(),
            messages,
            spent_usd: self
                .spend
                .as_ref()
                .map(SessionSpend::spent)
                .or(self.spent_before),
            questions,
        };
        let store = &mut self.store;
        let written = blocking(|| store.write(&saved));
        self.written = written.is_ok();
        self.last = saved.messages;
        if let Err(e) = written {
            tracing::warn!("saving the session: {e}");
            if self.said.insert(e.clone()) {
                self.session.notice(
                    NoticeLevel::Warning,
                    format!("This session could not be saved: {e}."),
                );
            }
        }
    }
}

/// `write` off the runtime's async threads where it has others to run
/// them (`block_in_place` on a multi-thread runtime: `yobuild run`'s),
/// else in place (a current-thread runtime, as some tests have: the write
/// then holds it, which only a test sees). The driver calls back on its
/// own task, so saves keep its order either way.
fn blocking<T>(write: impl FnOnce() -> T) -> T {
    use tokio::runtime::{Handle, RuntimeFlavor};
    match Handle::try_current().map(|h| h.runtime_flavor()) {
        Ok(RuntimeFlavor::MultiThread) => tokio::task::block_in_place(write),
        _ => write(),
    }
}

/// A [`Saver`] shared by the driver's callback and the session's end.
pub(crate) type Shared = Arc<Mutex<Saver>>;

/// The driver's callback for `saver`.
pub(crate) fn on_settled(saver: &Shared) -> impl FnMut(&Agent, Settled) + Send + 'static {
    let saver = saver.clone();
    move |agent: &Agent, why: Settled| {
        saver
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .settled(agent, why)
    }
}

/// The saved open questions asked again (their numbers kept), each said;
/// with no `ask_owner` now, said as not asked.
pub(crate) fn ask_again(saved: &Saved, ask: Option<&AskOwner>, session: &Session) {
    let open = &saved.questions.open;
    let Some(ask) = ask else {
        if !open.is_empty() {
            session.notice(
                NoticeLevel::Warning,
                format!(
                    "{} open question(s) of the saved session were not asked again: `ask_owner` is off now.",
                    open.len()
                ),
            );
        }
        return;
    };
    // The numbering goes on after the saved session's.
    ask.continue_after(saved.questions.next);
    for q in open {
        match Question::new(q.question.clone(), q.why.clone(), q.options.clone()) {
            Ok(question) => {
                ask.ask_again(q.n, question);
                session.notice(
                    NoticeLevel::Info,
                    format!("Asked again: #{} {}", q.n, q.question.trim()),
                );
            }
            Err(e) => tracing::warn!("a saved question #{} not asked again: {e}", q.n),
        }
    }
}

/// Now, `YYYY-MM-DDTHH:MM:SSZ` (the clock before 1970: its start).
fn now() -> String {
    crate::builtins::find_plugin::iso(crate::builtins::find_plugin::now().unwrap_or(0))
}

#[cfg(test)]
mod tests;
