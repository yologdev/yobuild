//! `facts`: what the host counted, after each run.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use tokio::sync::Mutex as AsyncMutex;
use yoagent::extension::{Extension, ExtensionError, RunContext, RunEnd, RunHooks, RunOutcome};
use yoagent::{AgentEvent, AgentMessage, Message};

use super::OpenQuestions;
use crate::frontend::{NoticeLevel, Session};

/// How long git may take for the snapshot, and for each run's facts
/// (yoagent gives an extension's `finish` 5 s in all).
const GIT_TIME: Duration = Duration::from_secs(4);

/// How many names a list shows before it only counts.
const NAMES: usize = 3;

/// An extension that, when each run ends, tells every frontend (a
/// [`Notice`](crate::frontend::ServerMessage::Notice)) what the model cannot
/// make up:
///
/// `Facts: since the start: <git diff --shortstat>, N new commit(s), new: a,
/// b; T tokens in, U out; K tool call(s); Ss; Q question(s) waiting on you.`
///
/// - The start is a snapshot of the repository taken when the first run
///   starts: uncommitted edits already there (stored with `git stash
///   create`), every branch and tag tip, and the untracked files with their
///   size and time. Edits, commits and files already there are not counted;
///   untracked files of the owner's that the agent changed or removed are
///   named. Files git ignores are not counted.
/// - Commits are those new to HEAD since the start (not on any branch or tag
///   then: a branch switch or a pull is not the agent's work); commits HEAD
///   no longer has (a reset, an amend) are said.
/// - What git cannot tell is said, never guessed: "not a git repository"
///   only when git says so; "changes unknown (git: …)" when git is missing,
///   fails or takes too long; a note when the edits already there could not
///   be told apart.
/// - git never writes the owner's index: every command works on a private
///   copy of it (`GIT_INDEX_FILE`), in the repository's top folder, in the C
///   locale.
/// - Tokens (input, cache reads and writes; output) and tool calls are this
///   run's; the questions are those still open when it ends
///   ([`Facts::with_open_questions`]).
///
/// A run a limit stopped also gets a warning notice saying so, and how to
/// go on.
pub struct Facts {
    session: Session,
    cwd: PathBuf,
    start: Arc<AsyncMutex<Option<Arc<Snapshot>>>>,
    open: Option<OpenQuestions>,
}

impl Facts {
    pub fn new(session: Session, cwd: impl Into<PathBuf>) -> Self {
        Self {
            session,
            cwd: cwd.into(),
            start: Arc::default(),
            open: None,
        }
    }

    /// Also count the questions still waiting on the owner.
    pub fn with_open_questions(mut self, open: OpenQuestions) -> Self {
        self.open = Some(open);
        self
    }
}

#[async_trait::async_trait]
impl Extension for Facts {
    fn name(&self) -> &str {
        "facts"
    }
    async fn start_run(&self, _run: &RunContext<'_>) -> Result<Box<dyn RunHooks>, ExtensionError> {
        let start = {
            let mut start = self.start.lock().await;
            match start.as_ref() {
                Some(snapshot) => snapshot.clone(),
                None => {
                    let snapshot = Arc::new(Snapshot::take(&self.cwd).await);
                    *start = Some(snapshot.clone());
                    snapshot
                }
            }
        };
        Ok(Box::new(FactsRun {
            session: self.session.clone(),
            cwd: self.cwd.clone(),
            start,
            open: self.open.clone(),
            started: Instant::now(),
            counts: Mutex::default(),
        }))
    }
}

struct FactsRun {
    session: Session,
    cwd: PathBuf,
    start: Arc<Snapshot>,
    open: Option<OpenQuestions>,
    started: Instant,
    /// Tokens in and out, tool calls.
    counts: Mutex<(u64, u64, usize)>,
}

#[async_trait::async_trait]
impl RunHooks for FactsRun {
    fn on_event(&self, event: &AgentEvent) {
        let mut counts = self.counts.lock().unwrap();
        match event {
            AgentEvent::MessageEnd {
                message: AgentMessage::Llm(Message::Assistant { usage, .. }),
            } => {
                counts.0 += usage.input + usage.cache_read + usage.cache_write;
                counts.1 += usage.output;
            }
            AgentEvent::ToolExecutionEnd { .. } => counts.2 += 1,
            _ => {}
        }
    }

    async fn finish(&mut self, outcome: &RunOutcome) {
        // A run a limit stopped has no report: say so, and how to go on.
        if let RunEnd::Stopped { reason } = outcome.end() {
            self.session.notice(
                NoticeLevel::Warning,
                format!("Stopped early: {reason}. Send \"continue\" to pick up where it left off."),
            );
        }
        let (tokens_in, tokens_out, tools) = *self.counts.lock().unwrap();
        let changed = match tokio::time::timeout(GIT_TIME, self.start.changes(&self.cwd)).await {
            Ok(changed) => changed,
            Err(_) => "changes since the start unknown (git: timed out)".into(),
        };
        let open = match &self.open {
            Some(open) => format!("; {} question(s) waiting on you", open.count()),
            None => String::new(),
        };
        self.session.notice(
            NoticeLevel::Info,
            format!(
                "Facts: {changed}; {tokens_in} tokens in, {tokens_out} out; {tools} tool \
                 call(s); {:.0}s{open}.",
                self.started.elapsed().as_secs_f64(),
            ),
        );
    }
}

/// An untracked file's size and time, to tell whether it changed.
type Stamp = (u64, Option<SystemTime>);

/// The repository as it was when the first run started.
enum Snapshot {
    NotRepo,
    /// git is missing, failed or took too long.
    Unknown(String),
    Repo(Start),
}

struct Start {
    /// The repository's top folder: every command runs there.
    root: PathBuf,
    /// HEAD's commit; `None` when it had none yet.
    head: Option<String>,
    /// The working tree with uncommitted edits (`git stash create`), the
    /// staged files when there was no commit (`git write-tree`), else `head`.
    tree: String,
    /// False when the edits already there could not be stored.
    edits_known: bool,
    /// Every branch and tag tip: commits reachable from them are not new.
    tips: Vec<String>,
    /// The untracked files then (paths from `root`); `None` when git could
    /// not list them.
    untracked: Option<HashMap<String, Stamp>>,
    taken: SystemTime,
}

impl Snapshot {
    async fn take(cwd: &Path) -> Self {
        match tokio::time::timeout(GIT_TIME, Self::take_now(cwd)).await {
            Ok(Ok(Some(start))) => Snapshot::Repo(start),
            Ok(Ok(None)) => Snapshot::NotRepo,
            Ok(Err(why)) => Snapshot::Unknown(why.to_string()),
            Err(_) => Snapshot::Unknown("timed out".into()),
        }
    }

    async fn take_now(cwd: &Path) -> Result<Option<Start>, GitError> {
        let root = match Git::at(cwd).run(&["rev-parse", "--show-toplevel"]).await {
            Ok(root) => PathBuf::from(root.trim()),
            Err(GitError::NotRepo) => return Ok(None),
            Err(why) => return Err(why),
        };
        let git = Git::at(&root).with_index_copy().await?;
        let head = git.head().await?;
        let (tree, edits_known) = match &head {
            // `git stash create` needs a commit: store what is staged as a
            // tree instead. Edits not staged on top of it cannot be told apart.
            None => {
                let tree = git.run(&["write-tree"]).await?.trim().to_owned();
                let unstaged = git.run(&["diff", "--quiet"]).await.is_err();
                (tree, !unstaged)
            }
            // Empty output: nothing uncommitted, `head` is the tree.
            Some(head) => match git.run(&["stash", "create"]).await {
                Ok(stash) if !stash.trim().is_empty() => (stash.trim().to_owned(), true),
                Ok(_) => (head.clone(), true),
                Err(why) => {
                    tracing::warn!(error = %why, "facts: no snapshot of the uncommitted edits");
                    (head.clone(), false)
                }
            },
        };
        let mut tips: Vec<String> = git
            .run(&["for-each-ref", "--format=%(objectname)"])
            .await?
            .lines()
            .map(str::to_owned)
            .collect();
        tips.extend(head.clone());
        let untracked = git.untracked().await.ok();
        Ok(Some(Start {
            root,
            head,
            tree,
            edits_known,
            tips,
            untracked,
            taken: SystemTime::now(),
        }))
    }

    async fn changes(&self, cwd: &Path) -> String {
        match self {
            Snapshot::NotRepo => match Git::at(cwd).run(&["rev-parse", "--git-dir"]).await {
                Ok(_) => "not a git repository at the start (one was made since)".into(),
                Err(_) => "not a git repository".into(),
            },
            Snapshot::Unknown(why) => {
                format!("changes unknown: no snapshot at the start (git: {why})")
            }
            Snapshot::Repo(start) => match start.changes().await {
                Ok(changed) => changed,
                Err(why) => format!("changes since the start unknown (git: {why})"),
            },
        }
    }
}

impl Start {
    async fn changes(&self) -> Result<String, GitError> {
        let git = Git::at(&self.root).with_index_copy().await?;
        let diff = git
            .run(&[
                "diff",
                "--no-relative",
                "--no-ext-diff",
                "--shortstat",
                &self.tree,
            ])
            .await?;
        let diff = match diff.trim() {
            "" => "no changes".to_owned(),
            diff => diff.to_owned(),
        };
        let mut changed = match git.head().await? {
            Some(_) => {
                let mut args = vec!["rev-list", "--count", "HEAD"];
                if !self.tips.is_empty() {
                    args.push("--not");
                    args.extend(self.tips.iter().map(String::as_str));
                }
                let new = git.count(&args).await?;
                let mut changed = format!("since the start: {diff}, {new} new commit(s)");
                if let Some(head) = &self.head {
                    let range = format!("HEAD..{head}");
                    let dropped = git.count(&["rev-list", "--count", &range]).await?;
                    if dropped > 0 {
                        changed.push_str(&format!(", {dropped} commit(s) no longer in HEAD"));
                    }
                }
                changed
            }
            None if self.head.is_some() => {
                format!("since the start: {diff}, HEAD has no commit now")
            }
            None => format!("since the start: {diff}, 0 new commit(s)"),
        };
        if !self.edits_known {
            changed.push_str(" (the edits already there could not be told apart)");
        }
        let (Some(before), Ok(now)) = (&self.untracked, git.untracked().await) else {
            changed.push_str(", new files unknown");
            return Ok(changed);
        };
        // New since the start, named (a test run's bytecode shows as what it
        // is); a file made before the start that only now shows (an ignore
        // rule removed) is not new.
        let new: Vec<&String> = now
            .iter()
            .filter(|(f, stamp)| {
                !before.contains_key(*f) && stamp.1.is_none_or(|time| time >= self.taken)
            })
            .map(|(f, _)| f)
            .collect();
        let edited: Vec<&String> = before
            .iter()
            .filter(|(f, stamp)| now.get(*f).is_some_and(|now| now != *stamp))
            .map(|(f, _)| f)
            .collect();
        let gone: Vec<&String> = before
            .keys()
            .filter(|f| !now.contains_key(*f) && !self.root.join(f).exists())
            .collect();
        changed.push_str(&names("new", new));
        changed.push_str(&names("changed untracked", edited));
        changed.push_str(&names("removed untracked", gone));
        Ok(changed)
    }
}

/// `, <what>: a, b` — sorted, at most [`NAMES`] named.
fn names(what: &str, files: Vec<&String>) -> String {
    let mut files: Vec<&str> = files.into_iter().map(String::as_str).collect();
    files.sort_unstable();
    match files.len() {
        0 => String::new(),
        n if n <= NAMES => format!(", {what}: {}", files.join(", ")),
        n => format!(", {n} {what}: {}, …", files[..NAMES].join(", ")),
    }
}

/// Runs git for the facts: in `dir`, in the C locale, with no repository
/// chosen by the environment, and — once [`with_index_copy`](Git::with_index_copy)
/// — on a private copy of the index, so a git killed at a time limit leaves
/// no lock in the owner's repository and their index is never written.
struct Git {
    dir: PathBuf,
    index: Option<tempfile::TempDir>,
}

enum GitError {
    NotRepo,
    Failed(String),
}

impl Git {
    fn at(dir: &Path) -> Self {
        Self {
            dir: dir.to_owned(),
            index: None,
        }
    }

    async fn with_index_copy(mut self) -> Result<Self, GitError> {
        let dir =
            tempfile::tempdir().map_err(|e| GitError::Failed(format!("no temp dir ({e})")))?;
        let real = self.run(&["rev-parse", "--git-path", "index"]).await?;
        let real = self.dir.join(real.trim());
        let copy = dir.path().join("index");
        // No index yet (nothing ever staged): git reads a missing one as empty.
        if real.exists() {
            tokio::fs::copy(&real, &copy)
                .await
                .map_err(|e| GitError::Failed(format!("cannot copy the index ({e})")))?;
        }
        self.index = Some(dir);
        Ok(self)
    }

    fn command(&self, args: &[&str]) -> tokio::process::Command {
        let mut command = tokio::process::Command::new("git");
        command
            .args(args)
            .current_dir(&self.dir)
            .env("LC_ALL", "C")
            .env("LANGUAGE", "C")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .kill_on_drop(true);
        if let Some(index) = &self.index {
            command.env("GIT_INDEX_FILE", index.path().join("index"));
        }
        command
    }

    /// Its output, untouched.
    async fn run(&self, args: &[&str]) -> Result<String, GitError> {
        let out = self.output(args).await?;
        if out.status.success() {
            return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
        }
        Err(failure(args, &out))
    }

    async fn output(&self, args: &[&str]) -> Result<std::process::Output, GitError> {
        self.command(args)
            .output()
            .await
            .map_err(|e| GitError::Failed(format!("cannot run git ({e})")))
    }

    async fn count(&self, args: &[&str]) -> Result<usize, GitError> {
        let out = self.run(args).await?;
        out.trim()
            .parse()
            .map_err(|_| GitError::Failed(format!("`git {}` gave {out:?}", args[0])))
    }

    /// HEAD's commit; `None` only when git says there is none yet (`-q`:
    /// exit 1, nothing on stderr). Any other failure is an error.
    async fn head(&self) -> Result<Option<String>, GitError> {
        let args = ["rev-parse", "--verify", "-q", "HEAD"];
        let out = self.output(&args).await?;
        if out.status.success() {
            return Ok(Some(String::from_utf8_lossy(&out.stdout).trim().to_owned()));
        }
        if out.status.code() == Some(1) && out.stderr.is_empty() {
            return Ok(None);
        }
        Err(failure(&args, &out))
    }

    /// The untracked files (git's ignore rules apply), from the top folder,
    /// names as they are, with their size and time.
    async fn untracked(&self) -> Result<HashMap<String, Stamp>, GitError> {
        let out = self
            .run(&[
                "ls-files",
                "-z",
                "--others",
                "--exclude-standard",
                "--full-name",
            ])
            .await?;
        Ok(out
            .split('\0')
            .filter(|f| !f.is_empty())
            .map(|f| {
                let meta = std::fs::symlink_metadata(self.dir.join(f)).ok();
                let stamp = (
                    meta.as_ref().map_or(0, |m| m.len()),
                    meta.and_then(|m| m.modified().ok()),
                );
                (f.to_owned(), stamp)
            })
            .collect::<HashMap<_, _>>())
    }
}

fn failure(args: &[&str], out: &std::process::Output) -> GitError {
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains("not a git repository") {
        return GitError::NotRepo;
    }
    let first = stderr.lines().next().unwrap_or("").trim();
    GitError::Failed(format!(
        "`git {}` failed: {}",
        args.first().unwrap_or(&""),
        if first.is_empty() {
            "no message"
        } else {
            first
        }
    ))
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GitError::NotRepo => f.write_str("not a git repository"),
            GitError::Failed(why) => f.write_str(why),
        }
    }
}
