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

use self::paths::{identity, locate, protected, real, single, Here};
use super::Scratch;
use crate::frontend::{session::UI_TIMEOUT, Session, UiRequest};

/// Tools that only read, free by default: yoagent's `read_file`,
/// `list_files` and `search`, and this crate's `ask_owner`.
pub const READ_ONLY_TOOLS: [&str; 4] = ["read_file", "list_files", "search", "ask_owner"];

/// An extension that asks the owner before a tool call does anything it
/// does not know to be harmless, and refuses it when nobody answers "Allow"
/// (no frontend, a timeout, "Block" or a dismissed question). The refusal
/// reaches the model as an error tool result with the reason. The question
/// has a short title, and the command or arguments in full, as written.
///
/// **An allow-list.** A shell command (`bash`) is free only when every
/// command in it runs a program the gate knows, with options it knows, and
/// everything it would write, append to, delete or run is allowed:
/// - **writing** (replacing): a new file, one the agent made, or in its
///   [`Scratch`]; **deleting** or moving away: only those of the agent's;
///   **appending**, making a folder or a file, changing a mode: inside the
///   project or the agent's own; never a name starting `.env`, nor `.git`
///   itself, in any case;
/// - **running shell text** (`bash -c`, `eval`, `trap`, substitutions): read
///   as a command line too; **a script** given to an interpreter: one of
///   the owner's, inside the project; **a test file**: inside the project;
/// - **programs**: readers, and writers whose targets are judged as above
///   (`cp`, `mv`, `tee`, `tar -c`, `curl -o`, …); builds and tests, with the
///   line inside the project (`cargo test`, `go test ./...`, `npm test`,
///   `make`, `pytest`); git that reads, or adds, commits and shares without
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
/// `cd` is followed as bash's logical `cd` does it. Everything it cannot
/// read asks: an expansion where an option or a path could be, anything
/// naming a path after a `cd` it cannot follow, a name linked earlier in
/// the same line.
///
/// File tools: `write_file` (or pi's `write`) over a file that exists,
/// `edit_file` (`edit`) of a file outside the project that is not the
/// agent's, either of `.env*` or `.git`, or of a path it cannot read (not a
/// string, `~`, `@`), ask. Other tools: free when named in
/// [`with_free_tools`](Self::with_free_tools) (by default
/// [`READ_ONLY_TOOLS`]), else they ask.
///
/// The agent's own files are those created by calls the gate let through
/// without asking (a call the owner allowed makes nothing the agent's):
/// recorded after the call, only if they exist, as that very file (device and inode;
/// a hard link to the owner's file under that name is the owner's). Where
/// file identity is not known (not Unix), nothing is the agent's own.
///
/// **Its folder must be the shell's:** relative paths are judged against
/// the `cwd` given to [`Gate::new`], so the agent's `bash` tool and file
/// tools must work there too.
///
/// **Not a sandbox.** Builds, tests and the owner's scripts run the
/// project's code as it is — including what the agent has edited, since
/// editing a file is free (a test it wrote, a `build.rs`, a `Makefile`, a
/// `package.json` script). The gate keeps the agent from deleting or
/// replacing the owner's files by command; it does not judge what code
/// does. For a guarantee, run the agent in a container or a disposable copy.
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
}

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
                created: Arc::default(),
            }),
            pending: Arc::default(),
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
    pub fn with_free_tools<S: Into<String>>(self, tools: impl IntoIterator<Item = S>) -> Self {
        self.change(|rules| rules.free_tools.extend(tools.into_iter().map(Into::into)))
    }

    fn change(self, edit: impl FnOnce(&mut Rules)) -> Self {
        let mut rules = Rules {
            start: self.rules.start.clone(),
            scratch: self.rules.scratch.clone(),
            free_tools: self.rules.free_tools.clone(),
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
        self.rules.check(tool, args)
    }
}

/// The gate's reading of one call.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Check {
    /// The question for the owner (a short title), when it needs their yes.
    pub ask: Option<String>,
    /// What the question is about, in full (the command as written, the
    /// arguments), shown under the title.
    pub details: String,
    /// Files it creates (only when it needs no yes): the agent's own once
    /// the call has run and they exist.
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
    fn rechecks_modified_calls(&self) -> bool {
        true
    }
    async fn start_run(&self, _run: &RunContext<'_>) -> Result<Box<dyn RunHooks>, ExtensionError> {
        Ok(Box::new(self.clone()))
    }
}

#[async_trait::async_trait]
impl RunHooks for Gate {
    async fn before_tool(&self, call: &ToolCallRequest<'_>) -> ToolDecision {
        // Each look replaces what an earlier one found (a recheck), and a
        // denied call leaves nothing behind.
        self.pending.lock().unwrap().remove(call.tool_call_id);
        let check = self.rules.check(call.tool_name, call.args);
        if let Some(question) = check.ask {
            let request = UiRequest::Select {
                title: question,
                // The command as written (several lines stay several lines).
                message: check.details,
                options: vec!["Allow".into(), "Block".into()],
                multiple: false,
            };
            let answer = self.session.ask(request, UI_TIMEOUT).await;
            match answer.as_str() {
                Some("Allow") => {}
                Some(_) => return ToolDecision::Deny("the owner blocked it".into()),
                None => {
                    return ToolDecision::Deny(
                        "not allowed: the owner declined, or nobody answered".into(),
                    )
                }
            }
        }
        // The files it makes become the agent's own once it has run.
        if !check.creates.is_empty() {
            self.pending
                .lock()
                .unwrap()
                .insert(call.tool_call_id.to_owned(), check.creates);
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
    /// Files the agent made this session, as the files they were (device
    /// and inode): it may overwrite or delete them while they still are.
    /// Shared by every clone of the gate.
    created: Arc<Mutex<HashMap<PathBuf, (u64, u64)>>>,
}

impl Rules {
    fn check(&self, tool: &str, args: &Json) -> Check {
        let ask = |question: String| Check {
            ask: Some(question),
            details: pretty(args),
            creates: Vec::new(),
        };
        let free = |creates: Vec<PathBuf>| Check {
            ask: None,
            details: String::new(),
            creates,
        };
        match tool {
            "bash" => {
                let Some(command) = args["command"].as_str() else {
                    return ask("Run `bash` with these arguments?".into());
                };
                match judge::line(self, command, &self.start) {
                    Ok(creates) => free(creates),
                    Err(judge::Ask) => Check {
                        ask: Some(match command.lines().count() {
                            1 if command.chars().count() <= 80 => format!("Run `{command}`?"),
                            _ => "Run this command?".into(),
                        }),
                        // The command as written (several lines stay several lines).
                        details: command.to_owned(),
                        creates: Vec::new(),
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
                let path = locate(&self.start.physical, file);
                if self.protected(&path) {
                    ask(format!("Change the protected file {file}?"))
                } else if writes && self.owners(&path) {
                    ask(format!("Overwrite the existing file {file}?"))
                } else if !writes && !path.starts_with(&self.start.physical) && !self.free(&path) {
                    ask(format!("Edit {file}, outside the project?"))
                } else if writes && !path.exists() {
                    free(vec![path])
                } else {
                    free(Vec::new())
                }
            }
            tool if self.free_tools.contains(tool) => free(Vec::new()),
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
        protected(path.strip_prefix(&self.start.physical).unwrap_or(path))
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
