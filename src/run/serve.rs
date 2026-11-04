//! `yobuild serve` (design §18): one agent's runs offered to a program on
//! this machine. Tasks wait in a queue and run one at a time, each put to
//! the owner first when `[serve] approval = "ask"`; each ends as the run
//! log's entry (design §16). The API's routes are the page's
//! (`frontend::web`), behind its token.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{json, Value as Json};
use tokio::sync::{mpsc, watch};

use crate::frontend::protocol::ClientMessage;
use crate::frontend::session::UI_TIMEOUT;
use crate::frontend::{Session, UiRequest};
use crate::manifest::Approval;

/// The most tasks queued at once (not yet put to the owner or sent): one
/// more is refused, not queued.
pub(crate) const QUEUED_AT_MOST: usize = 100;

/// The most characters a task may have.
pub(crate) const TASK_CHARS: usize = 64 * 1024;

/// Where a task is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// In the queue.
    Queued,
    /// Put to the owner (`approval = "ask"`).
    Asking,
    /// Sent to the session; its run has not started yet (a run the owner
    /// started from the page may still be going). A reset meanwhile drops
    /// it: refused, as the worker sees the session's generation move on.
    Sent,
    Running,
    Ended,
    /// Not run, or not seen to end: the owner blocked it, nobody answered,
    /// a reset dropped it, or the session ended first.
    Refused,
}

impl State {
    fn word(self) -> &'static str {
        match self {
            State::Queued => "queued",
            State::Asking => "asking",
            State::Sent => "sent",
            State::Running => "running",
            State::Ended => "ended",
            State::Refused => "refused",
        }
    }

    fn done(self) -> bool {
        matches!(self, State::Ended | State::Refused)
    }
}

struct Job {
    task: String,
    state: State,
    /// The session's run, once it started.
    run: Option<u64>,
    /// The run log's entry, once it ended.
    entry: Option<Json>,
    /// Why it was refused.
    why: Option<String>,
}

#[derive(Default)]
struct Inner {
    jobs: HashMap<String, Job>,
    /// The job sent and waiting for its run to start.
    sent: Option<String>,
    next: u64,
    closed: bool,
}

/// The tasks a program sent, shared by the API, the worker and the
/// session's watch (clones share them).
#[derive(Clone)]
pub(crate) struct Jobs {
    inner: Arc<Mutex<Inner>>,
    queue: mpsc::UnboundedSender<String>,
    changed: Arc<watch::Sender<u64>>,
}

impl Jobs {
    /// The jobs, and the queue the worker reads.
    pub(crate) fn new() -> (Jobs, mpsc::UnboundedReceiver<String>) {
        let (queue, rx) = mpsc::unbounded_channel();
        let (changed, _) = watch::channel(0);
        let jobs = Jobs {
            inner: Arc::default(),
            queue,
            changed: Arc::new(changed),
        };
        (jobs, rx)
    }

    fn with<T>(&self, f: impl FnOnce(&mut Inner) -> T) -> T {
        let out = f(&mut self.inner.lock().unwrap_or_else(PoisonError::into_inner));
        self.changed.send_modify(|n| *n += 1);
        out
    }

    fn set(&self, id: &str, state: State, why: Option<String>) {
        self.with(|inner| {
            if let Some(job) = inner.jobs.get_mut(id) {
                job.state = state;
                if why.is_some() {
                    job.why = why;
                }
            }
        });
    }

    fn state(&self, id: &str) -> Option<State> {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.jobs.get(id).map(|j| j.state)
    }

    /// The session's watch: a run started with `prompt`. The job sent with
    /// that text is now that run.
    pub(crate) fn started(&self, run: u64, prompt: &str) {
        self.with(|inner| {
            let Some(id) = inner.sent.clone() else { return };
            let Some(job) = inner.jobs.get_mut(&id) else {
                return;
            };
            if job.task == prompt {
                job.state = State::Running;
                job.run = Some(run);
                inner.sent = None;
            }
        });
    }

    /// The session's watch: `run` ended, as `entry` logs it.
    pub(crate) fn ended(&self, run: u64, entry: Json) {
        self.with(|inner| {
            let job = inner.jobs.values_mut().find(|j| j.run == Some(run));
            if let Some(job) = job {
                job.state = State::Ended;
                job.entry = Some(entry);
            }
        });
    }

    /// The session is ending: no new job is taken (those there are left to
    /// the watch, which may still see a run end: [`Jobs::close`] after).
    pub(crate) fn closing(&self) {
        self.with(|inner| inner.closed = true);
    }

    /// The session ended and its watch with it: every job not done is
    /// refused, said by where it was; no new one is taken.
    pub(crate) fn close(&self) {
        self.close_with("the session ended");
    }

    /// [`Jobs::close`], saying `why` the session took no more.
    pub(crate) fn close_with(&self, why: &str) {
        self.with(|inner| {
            inner.closed = true;
            inner.sent = None;
            for job in inner.jobs.values_mut().filter(|j| !j.state.done()) {
                let when = match job.state {
                    State::Running => "while it ran (its end was not seen)",
                    _ => "before it ran",
                };
                job.state = State::Refused;
                job.why = Some(format!("{why} {when}"));
            }
        });
    }

    /// The job `id`, still sent and not seen to start, refused for `why`.
    fn refuse_sent(&self, id: &str, why: &str) {
        self.with(|inner| {
            let Some(job) = inner.jobs.get_mut(id) else {
                return;
            };
            if job.state == State::Sent {
                job.state = State::Refused;
                job.why = Some(why.into());
                inner.sent = None;
            }
        });
    }
}

impl crate::frontend::web::Api for Jobs {
    fn submit(&self, task: String) -> Result<Json, String> {
        let task = task.trim().to_owned();
        if task.is_empty() {
            return Err("the task is blank".into());
        }
        if task.chars().count() > TASK_CHARS {
            return Err(format!("the task is longer than {TASK_CHARS} characters"));
        }
        let id = self.with(|inner| {
            if inner.closed {
                return Err("the session has ended".to_owned());
            }
            let waiting = inner
                .jobs
                .values()
                .filter(|j| j.state == State::Queued)
                .count();
            if waiting >= QUEUED_AT_MOST {
                return Err(format!(
                    "{waiting} tasks are waiting already: send it once one has ended"
                ));
            }
            inner.next += 1;
            let id = format!("t{}", inner.next);
            inner.jobs.insert(
                id.clone(),
                Job {
                    task,
                    state: State::Queued,
                    run: None,
                    entry: None,
                    why: None,
                },
            );
            Ok(id)
        })?;
        if self.queue.send(id.clone()).is_err() {
            self.set(&id, State::Refused, Some("the session has ended".into()));
        }
        Ok(self.status(&id).expect("the job just added"))
    }

    fn status(&self, id: &str) -> Option<Json> {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let job = inner.jobs.get(id)?;
        Some(json!({
            "id": id,
            "state": job.state.word(),
            "done": job.state.done(),
            "run": job.run,
            "why": job.why,
            "entry": job.entry,
        }))
    }

    fn run_of(&self, id: &str) -> Option<Option<u64>> {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.jobs.get(id).map(|j| j.run)
    }

    fn changes(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }
}

/// Runs each job the queue gives, one at a time, until the session ends:
/// put to the owner first with `approval = "ask"` (Allow runs it; Block,
/// no answer or nobody there refuses it), then sent to the session as a
/// prompt, and waited for until its run ends.
pub(crate) async fn work(
    jobs: Jobs,
    session: Session,
    approval: Approval,
    mut queue: mpsc::UnboundedReceiver<String>,
) {
    let mut changes = jobs.changed.subscribe();
    while let Some(id) = queue.recv().await {
        let task = {
            let inner = jobs.inner.lock().unwrap_or_else(PoisonError::into_inner);
            match inner.jobs.get(&id) {
                Some(job) if job.state == State::Queued => job.task.clone(),
                _ => continue,
            }
        };
        if approval == Approval::Ask {
            jobs.set(&id, State::Asking, None);
            let request = UiRequest::Select {
                title: "Run this task a program on this machine sent? (yobuild serve)".into(),
                message: task.clone(),
                options: vec!["Allow".into(), "Block".into()],
                multiple: false,
            };
            let answer = match session.frontends() {
                0 => Json::Null,
                _ => session.ask(request, UI_TIMEOUT).await,
            };
            let why = match answer.as_str() {
                Some("Allow") => None,
                Some(_) => Some("the owner blocked it"),
                None if session.is_closed() => Some("the session ended before it ran"),
                None if session.frontends() == 0 => {
                    Some("nobody was there to allow it (`approval = \"ask\"`: the owner allows each task on the page)")
                }
                None => Some("nobody answered"),
            };
            if let Some(why) = why {
                jobs.set(&id, State::Refused, Some(why.into()));
                continue;
            }
        }
        // Watched from here: a reset drops a prompt the driver queued
        // behind a run, and only its `RunStart` (broadcast before the
        // driver reads a later reset) tells it was not dropped. The history
        // replay is read first, so an earlier run of the same text is not
        // taken for it.
        let mut watching = session.observe();
        while watching.messages.try_recv().is_ok() {}
        let generation = session.generation();
        jobs.with(|inner| {
            inner.sent = Some(id.clone());
            if let Some(job) = inner.jobs.get_mut(&id) {
                job.state = State::Sent;
            }
        });
        if !session.send(ClientMessage::Prompt { text: task.clone() }) {
            jobs.set(&id, State::Refused, Some("the session has ended".into()));
            continue;
        }
        let mut seen = false;
        // Until its run ends (or the session does: `close`).
        loop {
            match jobs.state(&id) {
                None => break,
                Some(state) if state.done() => break,
                Some(State::Sent) if !seen => {
                    seen = started_in(&mut watching, &task);
                    if !seen && session.generation() != generation {
                        jobs.refuse_sent(&id, "the session was reset before it ran");
                        continue;
                    }
                }
                Some(_) => {}
            }
            tokio::select! {
                changed = changes.changed() => if changed.is_err() {
                    return;
                },
                _ = tokio::time::sleep(SENT_LOOK), if !seen => {}
            }
        }
    }
}

/// How often a sent job is looked at for a reset that dropped it.
const SENT_LOOK: std::time::Duration = std::time::Duration::from_millis(250);

/// Whether a run with `task` as its prompt started among what `watching`
/// got since it was last read (read to the end, so it stays small).
fn started_in(watching: &mut crate::frontend::Connection, task: &str) -> bool {
    let mut started = false;
    while let Ok(message) = watching.messages.try_recv() {
        if let crate::frontend::ServerMessage::RunStart { prompt, .. } = message {
            started |= prompt == task;
        }
    }
    started
}

/// What a program needs to find the API, written where `yobuild serve`
/// says (0600): the URL, the token and this process.
pub(crate) fn said(url: &str, token: &str) -> Json {
    json!({
        "runs": format!("{url}/api/runs"),
        "token": token,
        "pid": std::process::id(),
    })
}

/// Where `yobuild serve` writes [`said`] for `manifest` served from `cwd`:
/// `<home>/serve/<agent>-<hash>.json`, the hash over the manifest's path
/// (links resolved) and the working folder, as a session's folder is
/// named — so serving one agent from two folders writes two files.
pub(crate) fn file_of(
    manifest: &crate::manifest::Manifest,
    cwd: &std::path::Path,
    home: Option<&std::path::Path>,
) -> Result<std::path::PathBuf, super::BoxError> {
    let home = match home {
        Some(home) => home.to_owned(),
        None => crate::install::home()?,
    };
    let file = manifest.dir.join(crate::install::file_name(&manifest.path));
    let file = std::fs::canonicalize(&file).unwrap_or(file);
    let cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_owned());
    let hash = crate::install::sha256_hex(&[
        file.as_os_str().as_encoded_bytes(),
        cwd.as_os_str().as_encoded_bytes(),
    ]);
    let name = format!("{}-{}.json", manifest.agent.name, &hash[..16]);
    Ok(home.join("serve").join(name))
}

/// Removes `file` when it is still this process's (another `yobuild serve`
/// of the same agent and folder may have written its own over it since).
pub(crate) fn remove_own(file: &std::path::Path) -> std::io::Result<()> {
    let text = std::fs::read_to_string(file)?;
    let pid = serde_json::from_str::<Json>(&text)
        .ok()
        .and_then(|said| said["pid"].as_u64());
    match pid == Some(u64::from(std::process::id())) {
        true => std::fs::remove_file(file),
        false => Ok(()),
    }
}

/// Writes `bytes` to `file` whole, readable by this user only: its folder
/// made (0700), a temp file (0600) renamed into place.
pub(crate) fn write(file: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let dir = file.parent().unwrap_or(std::path::Path::new("."));
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)?;
    // tempfile makes its file 0600.
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(file).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::web::Api;

    #[test]
    fn a_task_is_its_run_once_the_run_it_was_sent_as_starts() {
        let (jobs, mut queue) = Jobs::new();
        let status = jobs.submit("  sum it  ".into()).unwrap();
        assert_eq!(
            (&status["id"], &status["state"]),
            (&"t1".into(), &"queued".into())
        );
        assert_eq!(queue.try_recv().unwrap(), "t1");
        assert_eq!(jobs.status("t1").unwrap()["state"], "queued");
        // Sent, as the worker does: a run the owner started meanwhile is
        // not it.
        jobs.with(|inner| {
            inner.sent = Some("t1".into());
            inner.jobs.get_mut("t1").unwrap().state = State::Sent;
        });
        jobs.started(4, "the owner's own");
        assert_eq!(jobs.run_of("t1"), Some(None));
        jobs.started(5, "sum it");
        assert_eq!(jobs.run_of("t1"), Some(Some(5)));
        assert_eq!(jobs.status("t1").unwrap()["state"], "running");
        jobs.ended(4, json!({ "run": 4 }));
        assert_eq!(jobs.status("t1").unwrap()["done"], false);
        jobs.ended(5, json!({ "run": 5 }));
        let done = jobs.status("t1").unwrap();
        assert_eq!(
            (&done["state"], &done["entry"]["run"]),
            (&"ended".into(), &5.into())
        );
        assert_eq!(jobs.run_of("t9"), None);
    }

    #[test]
    fn tasks_are_refused_blank_too_long_too_many_or_once_closed() {
        let (jobs, _queue) = Jobs::new();
        assert_eq!(jobs.submit(" \n".into()).unwrap_err(), "the task is blank");
        let long = "x".repeat(TASK_CHARS + 1);
        assert!(jobs.submit(long).unwrap_err().contains("longer than"));
        for _ in 0..QUEUED_AT_MOST {
            jobs.submit("t".into()).unwrap();
        }
        assert!(jobs
            .submit("t".into())
            .unwrap_err()
            .contains("waiting already"));
        // One sent and running: no longer counted as waiting.
        jobs.with(|inner| {
            inner.jobs.get_mut("t1").unwrap().state = State::Running;
            inner.jobs.get_mut("t1").unwrap().run = Some(1);
        });
        jobs.submit("t".into()).unwrap();
        jobs.closing();
        assert_eq!(
            jobs.submit("t".into()).unwrap_err(),
            "the session has ended"
        );
        // Closing leaves a running one to the watch.
        jobs.ended(1, json!({ "run": 1 }));
        jobs.close();
        let why = |id: &str| jobs.status(id).unwrap()["why"].clone();
        assert_eq!(jobs.status("t1").unwrap()["state"], "ended");
        assert_eq!(why("t2"), "the session ended before it ran");
        assert_eq!(
            jobs.submit("t".into()).unwrap_err(),
            "the session has ended"
        );
    }

    #[test]
    fn a_job_not_seen_to_end_is_said_so_and_only_a_sent_one_refused_as_sent() {
        let (jobs, _queue) = Jobs::new();
        jobs.submit("a".into()).unwrap();
        jobs.submit("b".into()).unwrap();
        jobs.with(|inner| {
            inner.sent = Some("t1".into());
            inner.jobs.get_mut("t1").unwrap().state = State::Sent;
        });
        jobs.refuse_sent("t2", "reset");
        assert_eq!(jobs.status("t2").unwrap()["state"], "queued");
        jobs.refuse_sent("t1", "the session was reset before it ran");
        assert_eq!(
            jobs.status("t1").unwrap()["why"],
            "the session was reset before it ran"
        );
        jobs.started(3, "a");
        assert_eq!(jobs.run_of("t1"), Some(None), "a refused job is not bound");
        jobs.with(|inner| {
            inner.jobs.get_mut("t2").unwrap().state = State::Running;
        });
        jobs.close_with("the worker stopped");
        assert_eq!(
            jobs.status("t2").unwrap()["why"],
            "the worker stopped while it ran (its end was not seen)"
        );
    }
}
