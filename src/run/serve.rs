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

/// The most tasks waiting at once: one more is refused, not queued.
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
    /// started from the page may still be going).
    Sent,
    Running,
    Ended,
    /// Not run: the owner blocked it, nobody answered, or the session
    /// ended first.
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

    /// The session ended: every job not done is refused, and no new one is
    /// taken.
    pub(crate) fn close(&self) {
        self.with(|inner| {
            inner.closed = true;
            for job in inner.jobs.values_mut().filter(|j| !j.state.done()) {
                job.state = State::Refused;
                job.why = Some("the session ended before it ran".into());
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
            let waiting = inner.jobs.values().filter(|j| !j.state.done()).count();
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
        jobs.with(|inner| {
            inner.sent = Some(id.clone());
            if let Some(job) = inner.jobs.get_mut(&id) {
                job.state = State::Sent;
            }
        });
        if !session.send(ClientMessage::Prompt { text: task }) {
            jobs.set(&id, State::Refused, Some("the session has ended".into()));
            continue;
        }
        // Until its run ends (or the session does: `close`).
        while !jobs.state(&id).is_none_or(State::done) {
            if changes.changed().await.is_err() {
                return;
            }
        }
    }
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

/// Where `yobuild serve` writes [`said`] for the agent whose run log is
/// `log`: `<home>/serve/<agent>-<hash>.json`, named as the log's folder.
pub(crate) fn file_of(log: &crate::run_log::RunLog) -> std::path::PathBuf {
    let dir = log.dir();
    let home = dir
        .parent()
        .and_then(std::path::Path::parent)
        .unwrap_or(dir);
    let name = dir.file_name().unwrap_or_default().to_string_lossy();
    home.join("serve").join(format!("{name}.json"))
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
        jobs.close();
        let status = jobs.status("t1").unwrap();
        assert_eq!(
            (&status["state"], &status["why"]),
            (&"refused".into(), &"the session ended before it ran".into())
        );
        assert_eq!(
            jobs.submit("t".into()).unwrap_err(),
            "the session has ended"
        );
    }
}
