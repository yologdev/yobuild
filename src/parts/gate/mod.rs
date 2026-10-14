//! `gate`: approval before deleting, discarding or overwriting what the
//! owner already has. A port of `confirm-dangerous.ts` (the pi extension,
//! kept as an example) to a Rust `before_tool`: no Node runtime on the
//! safety path.

mod shell;

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::Value as Json;
use yoagent::extension::{Extension, ExtensionError, ExtensionMode, RunContext, RunHooks};
use yoagent::{ToolCallRequest, ToolDecision};

use self::shell::{basename, parse, unwrap, Item, Word};
use super::Scratch;
use crate::frontend::{session::UI_TIMEOUT, Session, UiRequest};

/// Where a redirection is harmless.
const DEVICES: [&str; 4] = ["/dev/null", "/dev/stdout", "/dev/stderr", "/dev/tty"];
const ALWAYS: [&str; 6] = ["sudo", "doas", "truncate", "shred", "mkfs", "wipefs"];
const DELETERS: [&str; 4] = ["rm", "unlink", "rmdir", "shred"];
const SHELLS: [&str; 5] = ["sh", "bash", "zsh", "dash", "ksh"];

/// An extension that asks the owner before a tool call deletes, discards
/// or overwrites what they already have, and refuses it when nobody answers
/// "Allow" (no frontend, a timeout, or "Block"). The refusal reaches the
/// model as an error tool result with the reason.
///
/// Shell commands (`bash`) are read command by command — quotes, escapes,
/// operators, redirections, heredocs, substitutions, `bash -c` / `eval` of
/// literal text — following `cd` only where it surely applies. What asks:
/// - `rm`, `unlink`, `rmdir`, `git rm` (not `--cached`), `xargs` running
///   one of them, `find … -delete` or `-exec rm …`;
/// - git: `reset --hard`/`--merge`/`--keep`, `clean` (not `-n`),
///   `checkout` / `restore` of paths (not `restore --staged`),
///   `checkout -f`, `switch -f` / `--discard-changes`, `branch -D`, a forced
///   push (`-f`, `--force*`, `+ref`, `:ref`, `--mirror`, `--delete`),
///   `stash drop` / `clear`;
/// - `mv`, `cp`, `tee` (without `-a`), `dd of=` and truncating redirections
///   (`>`, `>|`, `2>`, `&>`; not `>>`, not to `/dev/null`) onto a file that
///   exists;
/// - `sudo`, `truncate`, `shred`, `mkfs`, recursive `chmod` / `chown`;
/// - any path it cannot know (`$VAR`, `~`, globs, a `cd` it cannot follow).
///
/// File tools: `write_file` (or pi's `write`) over a file that exists, and
/// `write_file` / `edit_file` (`edit`) of `.env` or anything in `.git/`.
///
/// Free: files the agent created in this session (by a call that was let
/// through), anything in its [`Scratch`], and targeted edits of other files.
/// Paths are compared with symlinks resolved.
///
/// **Not a sandbox:** a script or program can still change files in ways a
/// command line does not show (`sed -i`, `python3 -c …`, a Makefile), and a
/// plugin's own tools are judged only by these names. For a guarantee, run
/// the agent in a container or a disposable copy.
///
/// It is a required extension, and its state (the files the agent created)
/// lasts across runs.
#[derive(Clone)]
pub struct Gate {
    session: Session,
    rules: Arc<Rules>,
}

impl Gate {
    /// Asks through `session`; relative paths are against `cwd`.
    pub fn new(session: Session, cwd: impl Into<PathBuf>) -> Self {
        Self {
            session,
            rules: Arc::new(Rules {
                cwd: cwd.into(),
                scratch: None,
                created: Mutex::default(),
            }),
        }
    }

    /// Lets the agent delete and overwrite freely in its scratch folder.
    pub fn with_scratch(self, scratch: &Scratch) -> Self {
        Self {
            rules: Arc::new(Rules {
                cwd: self.rules.cwd.clone(),
                scratch: Some(real(scratch.path())),
                created: Mutex::default(),
            }),
            ..self
        }
    }

    /// What the gate makes of a call: the question to ask (`None`: free),
    /// and the files it would create.
    pub fn check(&self, tool: &str, args: &Json) -> Check {
        self.rules.check(tool, args)
    }
}

/// The gate's reading of one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// The question for the owner, when it needs their yes.
    pub ask: Option<String>,
    /// Files it creates: the agent's own once the call is let through.
    pub creates: Vec<PathBuf>,
}

#[async_trait::async_trait]
impl Extension for Gate {
    fn name(&self) -> &str {
        "gate"
    }
    fn mode(&self) -> ExtensionMode {
        ExtensionMode::Required
    }
    async fn start_run(&self, _run: &RunContext<'_>) -> Result<Box<dyn RunHooks>, ExtensionError> {
        Ok(Box::new(self.clone()))
    }
}

#[async_trait::async_trait]
impl RunHooks for Gate {
    async fn before_tool(&self, call: &ToolCallRequest<'_>) -> ToolDecision {
        let check = self.rules.check(call.tool_name, call.args);
        if let Some(question) = check.ask {
            let request = UiRequest::Select {
                title: question,
                message: String::new(),
                options: vec!["Allow".into(), "Block".into()],
                multiple: false,
            };
            let answer = self.session.ask(request, UI_TIMEOUT).await;
            match answer.as_str() {
                Some("Allow") => {}
                Some(_) => return ToolDecision::Deny("the owner blocked it".into()),
                None => {
                    return ToolDecision::Deny(
                        "needs the owner's approval, and nobody answered".into(),
                    )
                }
            }
        }
        // Only once it may run: the files it makes are then the agent's own.
        self.rules.created.lock().unwrap().extend(check.creates);
        ToolDecision::Allow
    }
}

struct Rules {
    cwd: PathBuf,
    scratch: Option<PathBuf>,
    /// Files the agent made this session: it may overwrite or delete them.
    created: Mutex<HashSet<PathBuf>>,
}

impl Rules {
    fn check(&self, tool: &str, args: &Json) -> Check {
        let mut check = Check {
            ask: None,
            creates: Vec::new(),
        };
        match tool {
            "bash" => {
                let command = args["command"].as_str().unwrap_or_default();
                let (ask, creates) = self.line(command, &self.cwd);
                check.creates = creates;
                if ask {
                    check.ask = Some(format!("Run `{command}`?"));
                }
            }
            "write_file" | "write" | "edit_file" | "edit" => {
                let file = args["path"].as_str().unwrap_or_default();
                let path = locate(&self.cwd, file);
                let writes = matches!(tool, "write_file" | "write");
                if protected(file) {
                    check.ask = Some(format!("Change the protected file {file}?"));
                } else if writes && self.owners(&path) {
                    check.ask = Some(format!("Overwrite the existing file {file}?"));
                } else if writes && !path.exists() {
                    check.creates.push(path);
                }
            }
            _ => {}
        }
        check
    }

    fn free(&self, path: &Path) -> bool {
        self.created.lock().unwrap().contains(path)
            || self
                .scratch
                .as_ref()
                .is_some_and(|scratch| path.starts_with(scratch))
    }

    /// A path the agent may not touch without asking: one that exists and is not its own.
    fn owners(&self, path: &Path) -> bool {
        !self.free(path) && path.exists()
    }

    /// Whether `line` needs the owner's yes, and the files it creates.
    fn line(&self, line: &str, start: &Path) -> (bool, Vec<PathBuf>) {
        const ASK: (bool, Vec<PathBuf>) = (true, Vec::new());
        let Some(items) = parse(line) else {
            return ASK;
        };
        let mut creates: Vec<PathBuf> = Vec::new();
        // `None` once a `cd` cannot be followed: every path is then unknown.
        let mut cwd: Option<PathBuf> = Some(start.to_owned());
        // Directories to return to when a `( … )` subshell ends.
        let mut saved: Vec<Option<PathBuf>> = Vec::new();
        let mut before = ";".to_owned();

        for item in items {
            let command = match item {
                Item::Open => {
                    saved.push(cwd.clone());
                    continue;
                }
                Item::Close => {
                    let Some(back) = saved.pop() else { return ASK };
                    cwd = back;
                    continue;
                }
                Item::Command(command) => command,
            };
            let piped = before == "|" || command.after == "|";
            before = command.after.clone();

            // A target this command would write: asks if it is the owner's, or unknown.
            let target = |w: &Word, creates: &mut Vec<PathBuf>| -> bool {
                if DEVICES.contains(&w.text.as_str()) {
                    return false;
                }
                let Some(dir) = cwd.as_deref().filter(|_| w.known) else {
                    return true;
                };
                let path = locate(dir, &w.text);
                if self.owners(&path) {
                    return true;
                }
                if !path.exists() {
                    creates.push(path);
                }
                false
            };
            // A path this command would delete: asks unless it is the agent's own.
            let deletes = |w: &Word| -> bool {
                match cwd.as_deref().filter(|_| w.known) {
                    Some(dir) => !self.free(&locate(dir, &w.text)),
                    None => true,
                }
            };

            // What its substitutions run, where it runs (a subshell of this directory).
            for inner in &command.runs {
                let Some(dir) = cwd.as_deref() else {
                    return ASK;
                };
                let (ask, made) = self.line(inner, dir);
                if ask {
                    return ASK;
                }
                creates.extend(made);
            }
            for w in &command.truncates {
                if target(w, &mut creates) {
                    return ASK;
                }
            }
            if command.words.is_empty() {
                continue;
            }
            let program = unwrap(&command.words);
            let (name, args) = (program.name.as_str(), program.args);
            let flags: Vec<&str> = args
                .iter()
                .filter(|a| a.text.starts_with('-'))
                .map(|a| a.text.as_str())
                .collect();
            let operands: Vec<&Word> = args.iter().filter(|a| !a.text.starts_with('-')).collect();

            if matches!(name, "cd" | "pushd" | "popd") {
                // Followed only when it surely applies to what comes next: a
                // literal directory that exists, not in a pipeline or the
                // background (a subshell), not before `||` (that runs if it failed).
                let dir = match (operands.first(), cwd.as_deref()) {
                    (Some(to), Some(dir)) if to.known && name == "cd" => {
                        Some(locate(dir, &to.text))
                    }
                    _ => None,
                };
                let after = command.after.as_str();
                cwd = dir.filter(|d| d.is_dir() && !piped && after != "&" && after != "||");
                continue;
            }
            if ALWAYS.contains(&name) || name.starts_with("mkfs.") {
                return ASK;
            }
            if name == "eval" {
                // Read like any command line, when its text is known.
                if args.is_empty() {
                    continue;
                }
                let Some(dir) = cwd.as_deref() else {
                    return ASK;
                };
                if args.iter().any(|a| !a.known) {
                    return ASK;
                }
                let text: Vec<&str> = args.iter().map(|a| a.text.as_str()).collect();
                let (ask, made) = self.line(&text.join(" "), dir);
                if ask {
                    return ASK;
                }
                creates.extend(made);
                continue;
            }
            if DELETERS.contains(&name) {
                if program.via_xargs || operands.is_empty() || operands.iter().any(|w| deletes(w)) {
                    return ASK;
                }
                continue;
            }
            if program.via_xargs && ["mv", "cp", "dd", "tee", "git"].contains(&name) {
                return ASK;
            }
            if SHELLS.contains(&name) {
                let script = args.iter().position(|a| {
                    a.text == "-c"
                        || (a.text.starts_with('-')
                            && a.text.ends_with('c')
                            && a.text[1..]
                                .chars()
                                .all(|c| c.is_ascii_alphanumeric() || c == '_'))
                });
                if let Some(at) = script {
                    let body = args.get(at + 1);
                    let (Some(body), Some(dir)) = (body.filter(|b| b.known), cwd.as_deref()) else {
                        return ASK;
                    };
                    let (ask, made) = self.line(&body.text, dir);
                    if ask {
                        return ASK;
                    }
                    creates.extend(made);
                }
                continue;
            }
            if name == "find" {
                let texts: Vec<&str> = args.iter().map(|a| a.text.as_str()).collect();
                if texts.contains(&"-delete") {
                    return ASK;
                }
                if let Some(exec) = texts
                    .iter()
                    .position(|t| ["-exec", "-execdir", "-ok", "-okdir"].contains(t))
                {
                    let runs = basename(texts.get(exec + 1).copied().unwrap_or(""));
                    if DELETERS.contains(&runs.as_str()) {
                        return ASK;
                    }
                }
                continue;
            }
            if matches!(name, "chmod" | "chown")
                && flags.iter().any(|f| {
                    *f == "--recursive"
                        || (!f.starts_with("--")
                            && f[1..]
                                .chars()
                                .all(|c| c.is_ascii_alphanumeric() || c == '_')
                            && f.contains('R'))
                })
            {
                return ASK;
            }
            if name == "dd" {
                if let Some(of) = args.iter().find(|a| a.text.starts_with("of=")) {
                    let to = Word {
                        text: of.text[3..].to_owned(),
                        known: of.known,
                    };
                    if target(&to, &mut creates) {
                        return ASK;
                    }
                }
                continue;
            }
            if name == "tee" {
                let appends = flags.iter().any(|f| *f == "-a" || *f == "--append");
                if !appends {
                    for w in &operands {
                        if target(w, &mut creates) {
                            return ASK;
                        }
                    }
                }
                continue;
            }
            if matches!(name, "mv" | "cp") {
                let into = args
                    .iter()
                    .position(|a| a.text == "-t" || a.text == "--target-directory");
                let dir = into.and_then(|at| args.get(at + 1));
                let sources: Vec<&Word> = match dir {
                    Some(dir) => operands.iter().copied().filter(|a| *a != dir).collect(),
                    None => operands[..operands.len().saturating_sub(1)].to_vec(),
                };
                let Some(dest) = dir.or(operands.last().copied()) else {
                    continue;
                };
                if sources.is_empty() {
                    continue;
                }
                let Some(here) = cwd.as_deref().filter(|_| dest.known) else {
                    return ASK;
                };
                let dest_path = locate(here, &dest.text);
                let landings: Vec<Word> = if dir.is_some() || dest_path.is_dir() {
                    sources
                        .iter()
                        .map(|s| Word {
                            text: dest_path
                                .join(basename(&s.text))
                                .to_string_lossy()
                                .into_owned(),
                            known: s.known,
                        })
                        .collect()
                } else {
                    vec![Word {
                        text: dest_path.to_string_lossy().into_owned(),
                        known: true,
                    }]
                };
                for w in &landings {
                    if target(w, &mut creates) {
                        return ASK;
                    }
                }
                continue;
            }
            if name == "git" && self.git_risky(args, cwd.as_deref()) {
                return ASK;
            }
        }
        (false, creates)
    }

    fn git_risky(&self, args: &[Word], cwd: Option<&Path>) -> bool {
        let mut i = 0;
        while i < args.len() && args[i].text.starts_with('-') {
            if ["-C", "-c", "--git-dir", "--work-tree", "--namespace"]
                .contains(&args[i].text.as_str())
            {
                i += 1;
            }
            i += 1;
        }
        let sub = args.get(i).map(|a| a.text.as_str()).unwrap_or("");
        let rest: Vec<&str> = args
            .get(i + 1..)
            .unwrap_or(&[])
            .iter()
            .map(|a| a.text.as_str())
            .collect();
        let flags: HashSet<&str> = rest
            .iter()
            .copied()
            .filter(|a| a.starts_with('-'))
            .collect();
        let paths: Vec<&str> = rest
            .iter()
            .copied()
            .filter(|a| !a.starts_with('-'))
            .collect();
        let has = |f: &str| flags.contains(f);
        match sub {
            "reset" => has("--hard") || has("--merge") || has("--keep"),
            "clean" => !has("-n") && !has("--dry-run"),
            "rm" => !has("--cached"),
            // Unstaging only touches the index; anything else rewrites files.
            "restore" => !(has("--staged") && !has("--worktree") && !has("-W")),
            // A branch switch is fine; checking out paths discards their changes.
            "checkout" => {
                has("-f")
                    || has("--force")
                    || rest.contains(&"--")
                    || paths.contains(&".")
                    || cwd.is_some_and(|dir| paths.iter().any(|p| self.owners(&locate(dir, p))))
            }
            "switch" => has("-f") || has("--force") || has("--discard-changes"),
            "branch" => has("-D") || (has("-d") && has("--force")),
            "push" => {
                flags.iter().any(|f| {
                    *f == "-f"
                        || f.starts_with("--force")
                        || *f == "--mirror"
                        || *f == "--delete"
                        || *f == "-d"
                }) || paths
                    .iter()
                    .any(|p| p.starts_with('+') || p.starts_with(':'))
            }
            "stash" => matches!(paths.first(), Some(&"drop") | Some(&"clear")),
            _ => false,
        }
    }
}

/// A path with a component starting `.env` (`.env`, `.env.local`, …) or
/// inside `.git/`.
fn protected(file: &str) -> bool {
    let starts = |s: &str| s.starts_with(".env") || s.starts_with(".git/");
    starts(file)
        || file
            .match_indices('/')
            .any(|(at, _)| starts(&file[at + 1..]))
}

/// `file` against `dir`, with `.` and `..` applied and symlinks resolved.
fn locate(dir: &Path, file: &str) -> PathBuf {
    let path = Path::new(file);
    let joined = if path.is_absolute() {
        path.to_owned()
    } else {
        dir.join(path)
    };
    real(&normalize(&joined))
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// `path` with symlinks resolved, also for a file not created yet.
fn real(path: &Path) -> PathBuf {
    if let Ok(real) = path.canonicalize() {
        return real;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) if parent != path => real(parent).join(name),
        _ => path.to_owned(),
    }
}

#[cfg(test)]
mod tests;
