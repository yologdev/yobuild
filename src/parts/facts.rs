//! `facts`: what the host counted, after each run.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::OnceCell;
use yoagent::extension::{Extension, ExtensionError, RunContext, RunEnd, RunHooks, RunOutcome};
use yoagent::{AgentEvent, AgentMessage, Message};

use super::OpenQuestions;
use crate::frontend::{NoticeLevel, Session};

/// How long git may take for each snapshot, and for the facts after a run
/// (yoagent gives an extension's `finish` 5 s in all).
const GIT_TIME: Duration = Duration::from_secs(4);

/// An extension that, when each run ends, tells every frontend (a
/// [`Notice`](crate::frontend::ServerMessage::Notice)) what the model cannot
/// make up:
///
/// `Facts: since the start: <git diff --shortstat>, N commit(s), new: a, b;
/// T tokens in, U out; K tool call(s); Ss; Q question(s) waiting on you.`
///
/// - The start is a snapshot of the folder taken when the first run starts:
///   uncommitted edits already there (stored with `git stash create`, which
///   touches neither the files, the index nor the stash list) and untracked
///   files already there are not counted.
/// - What git cannot tell is said, never guessed: "not a git repository"
///   only when git says so; "changes unknown (git: …)" when git is missing,
///   fails or takes too long; a note when the edits already there could not
///   be told apart (no snapshot).
/// - Tokens (input, cache reads and writes; output) and tool calls are this
///   run's; the questions are those still open when it ends
///   ([`Facts::with_open_questions`]).
///
/// A run a limit stopped also gets a warning notice saying so, and how to
/// go on.
pub struct Facts {
    session: Session,
    cwd: PathBuf,
    start: Arc<OnceCell<Snapshot>>,
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
        let start = self
            .start
            .get_or_init(|| Snapshot::take(&self.cwd))
            .await
            .clone();
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
    start: Snapshot,
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
            Err(_) => "changes unknown (git: timed out)".into(),
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

/// The folder as it was when the first run started.
#[derive(Clone)]
enum Snapshot {
    NotRepo,
    /// git is missing, failed or took too long.
    Unknown(String),
    Repo {
        /// The commit (git's empty tree when the repository has none yet):
        /// commits after it are the agent's.
        head: String,
        /// The working tree, uncommitted edits included, else `head` (and
        /// `edits_known` false when the snapshot failed).
        tree: String,
        edits_known: bool,
        /// The untracked files then; `None` when git could not list them.
        untracked: Option<HashSet<String>>,
    },
}

/// Git's empty tree: the base of a repository with no commit yet.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

impl Snapshot {
    async fn take(cwd: &Path) -> Self {
        match tokio::time::timeout(GIT_TIME, Self::take_now(cwd)).await {
            Ok(snapshot) => snapshot,
            Err(_) => Snapshot::Unknown("timed out".into()),
        }
    }

    async fn take_now(cwd: &Path) -> Self {
        match git(cwd, &["rev-parse", "--git-dir"]).await {
            Ok(_) => {}
            Err(Git::NotRepo) => return Snapshot::NotRepo,
            Err(Git::Failed(why)) => return Snapshot::Unknown(why),
        }
        // No HEAD yet: a repository with no commit.
        let head = git(cwd, &["rev-parse", "--verify", "-q", "HEAD"])
            .await
            .unwrap_or_else(|_| EMPTY_TREE.to_owned());
        let (tree, edits_known) = if head == EMPTY_TREE {
            // `git stash create` needs a commit: store what is staged as a
            // tree (`write-tree` leaves the files and the index as they are).
            // Edits not staged on top of that cannot be told apart.
            match git(cwd, &["write-tree"]).await {
                Ok(tree) => {
                    let unstaged = git(cwd, &["diff", "--quiet"]).await.is_err();
                    (tree, !unstaged)
                }
                Err(why) => {
                    tracing::warn!(error = %why, "facts: no snapshot of the staged files");
                    (head.clone(), false)
                }
            }
        } else {
            // Empty output: nothing uncommitted, `head` is the tree.
            match git(cwd, &["stash", "create"]).await {
                Ok(snapshot) if !snapshot.is_empty() => (snapshot, true),
                Ok(_) => (head.clone(), true),
                Err(why) => {
                    tracing::warn!(error = %why, "facts: no snapshot of the uncommitted edits");
                    (head.clone(), false)
                }
            }
        };
        Snapshot::Repo {
            head,
            tree,
            edits_known,
            untracked: untracked(cwd).await.ok(),
        }
    }

    async fn changes(&self, cwd: &Path) -> String {
        let (head, tree, edits_known, before) = match self {
            Snapshot::NotRepo => return "not a git repository".into(),
            Snapshot::Unknown(why) => return format!("changes unknown (git: {why})"),
            Snapshot::Repo {
                head,
                tree,
                edits_known,
                untracked,
            } => (head, tree, *edits_known, untracked),
        };
        let range = if head == EMPTY_TREE {
            "HEAD".to_owned()
        } else {
            format!("{head}..HEAD")
        };
        let diff = git(cwd, &["diff", "--shortstat", tree]).await;
        let commits = match git(cwd, &["rev-parse", "--verify", "-q", "HEAD"]).await {
            Ok(_) => git(cwd, &["rev-list", "--count", &range])
                .await
                .and_then(|n| n.parse::<usize>().map_err(|e| Git::Failed(e.to_string()))),
            // Still no commit in a repository that had none: none by the agent.
            Err(Git::Failed(_)) if head == EMPTY_TREE => Ok(0),
            Err(why) => Err(why),
        };
        let mut changed = match (diff, commits) {
            (Ok(diff), Ok(commits)) => {
                let diff = if diff.is_empty() {
                    "no changes".into()
                } else {
                    diff
                };
                format!("since the start: {diff}, {commits} commit(s)")
            }
            (Err(why), _) | (_, Err(why)) => {
                return format!("changes since the start unknown (git: {why})")
            }
        };
        if !edits_known {
            changed.push_str(" (the edits already there could not be told apart)");
        }
        // New since the start, named (a test run's bytecode shows as what it is).
        match (before, untracked(cwd).await) {
            (Some(before), Ok(now)) => {
                let mut new: Vec<String> =
                    now.into_iter().filter(|f| !before.contains(f)).collect();
                new.sort();
                match new.len() {
                    0 => {}
                    n if n <= 3 => changed.push_str(&format!(", new: {}", new.join(", "))),
                    n => changed.push_str(&format!(", {n} new files: {}, …", new[..3].join(", "))),
                }
            }
            _ => changed.push_str(", new files unknown"),
        }
        changed
    }
}

/// The untracked files (git's ignore rules apply), names unquoted.
async fn untracked(cwd: &Path) -> Result<HashSet<String>, Git> {
    let out = git(cwd, &["ls-files", "-z", "--others", "--exclude-standard"]).await?;
    Ok(out
        .split('\0')
        .filter(|f| !f.is_empty())
        .map(str::to_owned)
        .collect())
}

enum Git {
    NotRepo,
    Failed(String),
}

impl std::fmt::Display for Git {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Git::NotRepo => f.write_str("not a git repository"),
            Git::Failed(why) => f.write_str(why),
        }
    }
}

/// Runs git in `cwd`; its output trimmed of surrounding whitespace (but not
/// of `\0`s). The process is killed if the caller stops waiting.
async fn git(cwd: &Path, args: &[&str]) -> Result<String, Git> {
    let out = tokio::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| Git::Failed(format!("cannot run git ({e})")))?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout)
            .trim_matches(|c: char| c != '\0' && c.is_whitespace())
            .to_owned());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains("not a git repository") {
        return Err(Git::NotRepo);
    }
    let first = stderr.lines().next().unwrap_or("").trim();
    Err(Git::Failed(format!(
        "`git {}` failed: {}",
        args.first().unwrap_or(&""),
        if first.is_empty() {
            "no message"
        } else {
            first
        }
    )))
}
