//! `gate`: the owner's approval before a tool call does anything the gate
//! does not know to be harmless. The pieces, in the order a shell command
//! goes through them:
//! - [`parse`]: the command line read as bash reads it;
//! - [`invocation`]: what each command runs, past keywords, assignments
//!   and wrappers;
//! - [`args`]: one option grammar, by each program's [`Spec`](args::Spec);
//! - [`catalogue`]: the programs it lets run and what they do with their
//!   arguments; [`specs`]: the options each may be given; [`grammars`] and
//!   [`git`]: programs read by a grammar of their own;
//! - [`judge`]: each effect (a write, an append, a delete, shell text run)
//!   judged by one rule, against [`paths`].

mod args;
mod catalogue;
mod git;
mod grammars;
mod invocation;
mod judge;
mod parse;
mod paths;
mod specs;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::Value as Json;
use yoagent::extension::{
    Extension, ExtensionError, ExtensionMode, RunContext, RunHooks, ToolOutput,
};
use yoagent::{ToolCallRequest, ToolDecision};

use self::paths::{identity, locate, protected, real, single, Here, Identity};
use super::Scratch;
use crate::frontend::{session::UI_TIMEOUT, Session, UiRequest};

/// Tools that only read, allowed by default: yoagent's `read_file`,
/// `list_files` and `search`. Not [`AskOwner`](super::AskOwner)'s
/// `ask_owner`: whoever adds that tool allows it
/// (`with_allowed_tools([AskOwner::TOOL])`), so a plugin's tool of that name
/// is not allowed by its name alone.
pub const READ_ONLY_TOOLS: [&str; 3] = ["read_file", "list_files", "search"];

/// An extension that asks the owner before a tool call does anything it
/// does not know to be harmless, and refuses it when nobody answers "Allow"
/// (no frontend, a timeout, "Block" or a dismissed question). The refusal
/// reaches the model as an error tool result with the reason. The question
/// has a short title, and the command or arguments in full, as written.
///
/// **An allow-list.** A shell command (`bash`) is free only when every
/// command in it runs a program the gate knows, with options it knows, and
/// everything it would write, append to, delete or run is allowed:
/// - **writing** (replacing): a new file inside the project, one the agent
///   made, or in its [`Scratch`] (a new file elsewhere asks: it could be
///   one a program reads and runs); **deleting** or moving away: only those of the agent's;
///   **touching**: a new file inside the project, or the agent's own;
///   **changing a mode or owner** (`chmod`, `chown`, `chgrp`): only the
///   agent's own; **appending**, making a folder: inside the project or the
///   agent's own; never a name starting `.env`, nor `.git`
///   itself, in any case;
/// - **running shell text** (`bash -c`, `eval`, `trap`, substitutions): read
///   as a command line too; **a script** given to an interpreter: one of
///   the owner's, inside the project; **a test file**: inside the project;
/// - **programs**: readers, and writers whose targets are judged as above
///   (`cp`, `mv`, `tee`, `tar -c`, `curl -o`, …); builds and tests, with the
///   line inside the project (`cargo test`, `go test ./...`, `npm test`,
///   `pytest`; `make` with no target (its default, whatever that is) or only
///   targets that build or check — `test`, `build`, `lint`, … — and `npm
///   run` only for those; checkers through `npx` only when the
///   project has them installed); git that reads, or adds, commits and shares without
///   discarding the working tree (`reset --soft`, `commit --amend` move
///   history; the reflog keeps it). Each
///   with its own list of options: an option not on it asks, as does an
///   abbreviation, or a cluster with a letter not on it;
/// - **the program itself**: named plainly (`cat`), or by its path in a
///   system folder (`/usr/bin/cat`; not `./cat`); given only environment
///   variables that change no program (`RUST_LOG=…`, not `PATH=…`).
///
/// The line is read as bash reads it — quotes, escapes, operators,
/// redirections, heredocs, keywords, comments, line continuations — and
/// `cd` is followed as bash's logical `cd` does it. Where bash would
/// evaluate a value as code (arithmetic on a variable, `${!x}`, a subscript
/// that is not a literal number, `${x@P}`, `(( … ))`), it asks: a value set
/// earlier (`x='a[$(rm f)]'`) would run; so does an expansion that assigns
/// (`${x:=…}`), and a `[[ … ]]` test that is not whole in one command. Everything it cannot
/// read asks: an expansion where an option or a path could be, anything
/// naming a path after a `cd` it cannot follow, a name linked earlier in
/// the same line.
///
/// File tools: `write_file` (or pi's `write`) over a file that exists,
/// either tool on a file outside the project that is not the agent's (a
/// new one too), either of `.env*` or `.git`, or of a path it cannot read (not a
/// string, `~`, `@`), ask. Other tools: free when named in
/// [`with_allowed_tools`](Self::with_allowed_tools) (by default
/// [`READ_ONLY_TOOLS`]), else they ask.
///
/// The agent's own files are those created by calls the gate let through
/// without asking (a call the owner allowed makes nothing the agent's):
/// recorded after the call, only if they exist, as that very file
/// (device, inode and when it was made — a hard link
/// to the owner's file under that name is the owner's, and so is a file
/// the owner wrote in its place), and recorded again after each free call
/// that changes them. Where file identity is not known (not Unix), nothing
/// is the agent's own.
///
/// **Its folder must be the shell's:** relative paths are judged against
/// the `cwd` given to [`Gate::new`], so the agent's `bash` tool and file
/// tools must work there too.
///
/// **Not a sandbox.** Builds, tests and the owner's scripts run the
/// project's code as it is — including what the agent has edited, since
/// editing a file is free (a test it wrote, a `build.rs`, a `Makefile`, a
/// `package.json` script) — and the owner's scripts run with any
/// arguments. The gate keeps the agent from deleting or replacing the
/// owner's files by command; it does not judge what code does, and it does
/// not keep data in: reading is free (`.env` included, as it is to the
/// `read_file` tool), and so is sending what was read (`curl`, `git
/// push`). For a guarantee, run the agent in a container or a disposable
/// copy, without secrets it should not have.
///
/// It is a required extension; it judges a call again when a later
/// extension rewrote it; its state (the files the agent created) lasts
/// across runs and is shared by clones.
#[derive(Clone)]
pub struct Gate {
    session: Session,
    rules: Arc<Rules>,
    /// Files each allowed call would create, until it has run.
    pending: Arc<Mutex<HashMap<String, Vec<PathBuf>>>>,
    /// The run these hooks judge (set at its start): what
    /// [`with_tools_allowed_if`](Self::with_tools_allowed_if) asks about.
    run_id: Option<Arc<str>>,
}

/// Who may offer a tool for it to run without asking: `(run id, tool name)`
/// → whether the tool that would run is one the caller allows.
pub type OfferedCheck = Arc<dyn Fn(&str, &str) -> bool + Send + Sync>;

impl Gate {
    /// Asks through `session`; relative paths are against `cwd` (made
    /// absolute). [`READ_ONLY_TOOLS`] are free.
    pub fn new(session: Session, cwd: impl Into<PathBuf>) -> Self {
        let cwd = cwd.into();
        let cwd = std::path::absolute(&cwd).unwrap_or(cwd);
        Self {
            session,
            rules: Arc::new(Rules {
                // Bash takes the logical folder from `$PWD`.
                start: Here::start(&cwd, std::env::var_os("PWD").map(PathBuf::from).as_deref()),
                scratch: None,
                free_tools: READ_ONLY_TOOLS.iter().map(|t| t.to_string()).collect(),
                allowed_if: None,
                created: Arc::default(),
            }),
            pending: Arc::default(),
            run_id: None,
        }
    }

    /// Lets the agent delete and overwrite freely in its scratch folder.
    pub fn with_scratch(self, scratch: &Scratch) -> Self {
        self.change(|rules| rules.scratch = Some(real(scratch.path())))
    }

    /// Also lets these tools run without asking: tools that only read or
    /// look things up (a plan, a web search). Never one that writes, deletes
    /// or runs code. It cannot free `bash` or the file tools: those are
    /// always judged.
    pub fn with_allowed_tools<S: Into<String>>(self, tools: impl IntoIterator<Item = S>) -> Self {
        self.change(|rules| rules.free_tools.extend(tools.into_iter().map(Into::into)))
    }

    /// Also lets these tools run without asking — when `offered` says, for
    /// the run and the tool, that the tool that would run is one the caller
    /// allows (a plugin's tool allowed only when that plugin offered it).
    /// Outside a run ([`check`](Self::check)) they ask.
    pub fn with_tools_allowed_if<S: Into<String>>(
        self,
        tools: impl IntoIterator<Item = S>,
        offered: OfferedCheck,
    ) -> Self {
        let names: HashSet<String> = tools.into_iter().map(Into::into).collect();
        self.change(|rules| rules.allowed_if = Some((names, offered)))
    }

    fn change(self, edit: impl FnOnce(&mut Rules)) -> Self {
        let mut rules = Rules {
            start: self.rules.start.clone(),
            scratch: self.rules.scratch.clone(),
            free_tools: self.rules.free_tools.clone(),
            allowed_if: self.rules.allowed_if.clone(),
            created: self.rules.created.clone(),
        };
        edit(&mut rules);
        Self {
            rules: Arc::new(rules),
            ..self
        }
    }

    /// What the gate makes of a call: the question to ask (`None`: free),
    /// and the files it would create.
    pub fn check(&self, tool: &str, args: &Json) -> Check {
        self.rules.check_in(tool, args, None)
    }
}

/// The gate's reading of one call: it runs freely, or it needs the
/// owner's yes — never both, so a call the owner allowed makes nothing the
/// agent's.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Check {
    /// It runs without asking. The files it creates become the agent's own
    /// once the call has run, if they are there.
    Free { creates: Vec<PathBuf> },
    /// It needs the owner's yes.
    Ask {
        /// A short title.
        question: String,
        /// What it is about, in full (the command as written, or the
        /// arguments), shown under the title.
        details: String,
    },
}

impl Check {
    /// The question, when it needs the owner's yes.
    pub fn question(&self) -> Option<&str> {
        match self {
            Check::Ask { question, .. } => Some(question),
            Check::Free { .. } => None,
        }
    }

    /// What the question is about, in full.
    pub fn details(&self) -> Option<&str> {
        match self {
            Check::Ask { details, .. } => Some(details),
            Check::Free { .. } => None,
        }
    }
}

#[async_trait::async_trait]
impl Extension for Gate {
    fn name(&self) -> &str {
        "gate"
    }
    fn mode(&self) -> ExtensionMode {
        ExtensionMode::Required
    }
    fn rechecks_modified_calls(&self) -> bool {
        true
    }
    async fn start_run(&self, run: &RunContext<'_>) -> Result<Box<dyn RunHooks>, ExtensionError> {
        Ok(Box::new(Gate {
            run_id: Some(Arc::from(run.run_id)),
            ..self.clone()
        }))
    }
}

#[async_trait::async_trait]
impl RunHooks for Gate {
    async fn before_tool(&self, call: &ToolCallRequest<'_>) -> ToolDecision {
        // Each look replaces what an earlier one found (a recheck), and a
        // denied call leaves nothing behind.
        self.pending.lock().unwrap().remove(call.tool_call_id);
        let creates = match self
            .rules
            .check_in(call.tool_name, call.args, self.run_id.as_deref())
        {
            Check::Free { creates } => creates,
            Check::Ask { question, details } => {
                let request = UiRequest::Select {
                    title: question,
                    // The command as written (several lines stay several lines).
                    message: details,
                    options: vec!["Allow".into(), "Block".into()],
                    multiple: false,
                };
                let answer = self.session.ask(request, UI_TIMEOUT).await;
                return match answer.as_str() {
                    // Allowed by the owner: what it makes stays theirs.
                    Some("Allow") => ToolDecision::Allow,
                    Some(_) => ToolDecision::Deny("the owner blocked it".into()),
                    None => ToolDecision::Deny(
                        "not allowed: the owner declined, or nobody answered".into(),
                    ),
                };
            }
        };
        // The files a free call makes become the agent's own once it has run.
        if !creates.is_empty() {
            self.pending
                .lock()
                .unwrap()
                .insert(call.tool_call_id.to_owned(), creates);
        }
        ToolDecision::Allow
    }

    async fn after_tool(
        &self,
        call: &ToolCallRequest<'_>,
        output: &mut ToolOutput,
    ) -> Result<(), ExtensionError> {
        let creates = self.pending.lock().unwrap().remove(call.tool_call_id);
        if let (Some(creates), false) = (creates, output.is_error) {
            // Only what is there now (a command can fail with no error from
            // the tool), as that very file.
            let mut created = self.rules.created.lock().unwrap();
            for path in creates {
                if let Some(id) = identity(&path) {
                    created.insert(path, id);
                }
            }
        }
        Ok(())
    }
}

struct Rules {
    start: Here,
    scratch: Option<PathBuf>,
    free_tools: HashSet<String>,
    /// Tools free only when the check says the one that would run is
    /// allowed (`with_tools_allowed_if`).
    allowed_if: Option<(HashSet<String>, OfferedCheck)>,
    /// Files the agent made this session, as the files they were
    /// ([`Identity`]): it may overwrite or delete them while they still are.
    /// Shared by every clone of the gate.
    created: Arc<Mutex<HashMap<PathBuf, Identity>>>,
}

impl Rules {
    /// Outside a run: a tool allowed only by its offerer asks.
    #[cfg(test)]
    fn check(&self, tool: &str, args: &Json) -> Check {
        self.check_in(tool, args, None)
    }

    /// The verdict on a call in run `run`.
    fn check_in(&self, tool: &str, args: &Json, run: Option<&str>) -> Check {
        let ask = |question: String| Check::Ask {
            question,
            details: pretty(args),
        };
        let free = |creates: Vec<PathBuf>| Check::Free { creates };
        match tool {
            "bash" => {
                let Some(command) = args["command"].as_str() else {
                    return ask("Run `bash` with these arguments?".into());
                };
                match judge::line(self, command, &self.start) {
                    Ok(creates) => free(creates),
                    Err(judge::Ask) => Check::Ask {
                        question: match command.lines().count() {
                            1 if command.chars().count() <= 80 => format!("Run `{command}`?"),
                            _ => "Run this command?".into(),
                        },
                        // The command as written (several lines stay several lines).
                        details: command.to_owned(),
                    },
                }
            }
            "write_file" | "write" | "edit_file" | "edit" => {
                let writes = matches!(tool, "write_file" | "write");
                let file = match args["path"].as_str() {
                    // `~` and `@` mean something else to some tools (pi's).
                    Some(file) if !file.is_empty() && !file.starts_with(['~', '@']) => file,
                    _ => return ask(format!("Run `{tool}` with these arguments?")),
                };
                let path = locate(self.start.physical(), file);
                if self.protected(&path) {
                    ask(format!("Change the protected file {file}?"))
                } else if writes && self.owners(&path) {
                    ask(format!("Overwrite the existing file {file}?"))
                } else if !path.starts_with(self.start.physical()) && !self.free(&path) {
                    // A new file elsewhere too: one a program reads and runs
                    // (`~/.config/git/config`, a login item).
                    ask(format!("Change {file}, outside the project?"))
                } else if !path.exists() || self.free(&path) {
                    // Made, or the agent's own changed: recorded again (an
                    // edit may alter its identity).
                    free(vec![path])
                } else {
                    free(Vec::new())
                }
            }
            tool if self.free_tools.contains(tool) => free(Vec::new()),
            tool if self.allowed_if.as_ref().is_some_and(|(names, offered)| {
                names.contains(tool) && run.is_some_and(|run| offered(run, tool))
            }) =>
            {
                free(Vec::new())
            }
            tool => ask(format!("Run `{tool}`?")),
        }
    }

    /// The agent's own: a file it made, while it is that file and has no
    /// other name (a hard link to the owner's file is the owner's); or in
    /// its scratch folder.
    fn free(&self, path: &Path) -> bool {
        let own = self
            .created
            .lock()
            .unwrap()
            .get(path)
            .is_some_and(|id| identity(path) == Some(*id) && single(path));
        own || self
            .scratch
            .as_ref()
            .is_some_and(|scratch| path.starts_with(scratch))
    }

    /// A name starting `.env`, or `.git` itself: below the project's folder for a
    /// path inside it (the project may sit in `~/.envs/`), anywhere for
    /// one outside.
    fn protected(&self, path: &Path) -> bool {
        protected(path.strip_prefix(self.start.physical()).unwrap_or(path))
    }

    /// A path the agent may not replace without asking: there, and not its own.
    fn owners(&self, path: &Path) -> bool {
        !self.free(path) && path.exists()
    }
}

/// A call's arguments in full, for a question.
fn pretty(args: &Json) -> String {
    serde_json::to_string_pretty(args).unwrap_or_else(|_| args.to_string())
}

// The tests make symlinks and hard links, and need file identity.
#[cfg(all(test, unix))]
mod tests;
