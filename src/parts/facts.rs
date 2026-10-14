//! `facts`: what the host counted, after each run.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use yoagent::extension::{Extension, ExtensionError, RunContext, RunEnd, RunHooks, RunOutcome};
use yoagent::{AgentEvent, AgentMessage, Message};

use super::OpenQuestions;
use crate::frontend::{NoticeLevel, Session};

/// An extension that, when each run ends, tells every frontend (a
/// [`Notice`](crate::frontend::ServerMessage::Notice)) what the model cannot
/// make up:
///
/// `Facts: since the start: <git diff --shortstat>, N commit(s), new: a, b;
/// T tokens in, U out; K tool call(s); Ss; Q question(s) waiting on you.`
///
/// - Changes and commits are against the folder as it was when the `Facts`
///   was made: uncommitted edits already there (snapshotted with
///   `git stash create`, which touches neither the files, the index nor the
///   stash list) and untracked files already there are not counted. Outside
///   a git repository: "not a git repository".
/// - Tokens and tool calls are this run's; the questions are those still
///   open when it ends ([`Facts::with_open_questions`]).
///
/// A run a limit stopped also gets a warning notice saying so, and how to
/// go on.
pub struct Facts {
    session: Session,
    cwd: PathBuf,
    start: Option<Start>,
    untracked: Arc<HashSet<String>>,
    open: Option<OpenQuestions>,
}

impl Facts {
    /// Takes the snapshot of `cwd` now: make it before the agent starts.
    pub fn new(session: Session, cwd: impl Into<PathBuf>) -> Self {
        let cwd = cwd.into();
        Self {
            session,
            start: Start::of(&cwd),
            untracked: Arc::new(untracked(&cwd)),
            cwd,
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
        Ok(Box::new(FactsRun {
            session: self.session.clone(),
            cwd: self.cwd.clone(),
            start: self.start.clone(),
            untracked: self.untracked.clone(),
            open: self.open.clone(),
            started: Instant::now(),
            counts: Mutex::default(),
        }))
    }
}

struct FactsRun {
    session: Session,
    cwd: PathBuf,
    start: Option<Start>,
    untracked: Arc<HashSet<String>>,
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
                counts.0 += usage.input + usage.cache_read;
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
        let changed = match &self.start {
            Some(start) => start.changes(&self.cwd),
            None => "not a git repository".into(),
        };
        // New since the start, named (a test run's bytecode shows as what it is).
        let mut new: Vec<String> = untracked(&self.cwd)
            .into_iter()
            .filter(|f| !self.untracked.contains(f))
            .collect();
        new.sort();
        let new = match new.len() {
            0 => String::new(),
            n if n <= 3 => format!(", new: {}", new.join(", ")),
            n => format!(", {n} new files: {}, …", new[..3].join(", ")),
        };
        let open = match &self.open {
            Some(open) => format!("; {} question(s) waiting on you", open.count()),
            None => String::new(),
        };
        self.session.notice(
            NoticeLevel::Info,
            format!(
                "Facts: {changed}{new}; {tokens_in} tokens in, {tokens_out} out; {tools} tool \
                 call(s); {:.0}s{open}.",
                self.started.elapsed().as_secs_f64(),
            ),
        );
    }
}

/// Where the agent started, in git.
#[derive(Clone)]
struct Start {
    /// The commit (git's empty tree when the repository has none yet):
    /// commits after it are the agent's.
    head: String,
    /// The working tree, uncommitted edits included (`git stash create`
    /// stores them as a commit without touching the files, the index or
    /// the stash list), else `head`: changes since it are the agent's.
    tree: String,
}

/// Git's empty tree: the base of a repository with no commit yet.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

impl Start {
    /// `None` outside a git repository.
    fn of(cwd: &Path) -> Option<Self> {
        git(cwd, &["rev-parse", "--git-dir"])?;
        let head = git(cwd, &["rev-parse", "HEAD"]).unwrap_or_else(|| EMPTY_TREE.to_owned());
        let tree = git(cwd, &["stash", "create"])
            .filter(|snapshot| !snapshot.is_empty())
            .unwrap_or_else(|| head.clone());
        Some(Start { head, tree })
    }

    /// Committed and uncommitted changes alike, against the working tree as
    /// it was: edits the owner had not committed are not the agent's.
    fn changes(&self, cwd: &Path) -> String {
        let diff = git(cwd, &["diff", "--shortstat", &self.tree]).unwrap_or_default();
        let range = if self.head == EMPTY_TREE {
            "HEAD".to_owned()
        } else {
            format!("{}..HEAD", self.head)
        };
        let commits = git(cwd, &["rev-list", "--count", &range])
            .and_then(|n| n.parse::<usize>().ok())
            .unwrap_or(0);
        let diff = if diff.is_empty() {
            "no changes".into()
        } else {
            diff
        };
        format!("since the start: {diff}, {commits} commit(s)")
    }
}

/// The untracked files (git's ignore rules apply); empty outside a repository.
fn untracked(cwd: &Path) -> HashSet<String> {
    git(cwd, &["ls-files", "--others", "--exclude-standard"])
        .map(|s| s.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

fn git(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}
