//! `gate`: approval before deleting, discarding or overwriting what the
//! owner already has. A port of `confirm-dangerous.ts` (the pi extension,
//! kept as an example) to a Rust `before_tool`: no Node runtime on the
//! safety path.

mod shell;

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::Value as Json;
use yoagent::extension::{
    Extension, ExtensionError, ExtensionMode, RunContext, RunHooks, ToolOutput,
};
use yoagent::{ToolCallRequest, ToolDecision};

use self::shell::{basename, parse, unwrap, Item, Word};
use super::Scratch;
use crate::frontend::{session::UI_TIMEOUT, Session, UiRequest};

/// Where a redirection is harmless.
const DEVICES: [&str; 4] = ["/dev/null", "/dev/stdout", "/dev/stderr", "/dev/tty"];
const ALWAYS: [&str; 6] = ["sudo", "doas", "truncate", "shred", "mkfs", "wipefs"];
const DELETERS: [&str; 4] = ["rm", "unlink", "rmdir", "shred"];
const SHELLS: [&str; 5] = ["sh", "bash", "zsh", "dash", "ksh"];
/// Programs `find -exec` may run without asking: they only read.
const READS: [&str; 24] = [
    "grep",
    "egrep",
    "fgrep",
    "rg",
    "wc",
    "cat",
    "head",
    "tail",
    "ls",
    "stat",
    "file",
    "echo",
    "printf",
    "basename",
    "dirname",
    "md5sum",
    "sha1sum",
    "sha256sum",
    "shasum",
    "du",
    "test",
    "[",
    "diff",
    "cmp",
];

/// Tools that only read, free by default: yoagent's `read_file`,
/// `list_files`, `search`, and `ask_owner`.
pub const READ_ONLY_TOOLS: [&str; 4] = ["read_file", "list_files", "search", "ask_owner"];

/// An extension that asks the owner before a tool call does anything it
/// does not know to be harmless, and refuses it when nobody answers "Allow"
/// (no frontend, a timeout, "Block" or a dismissed question). The refusal
/// reaches the model as an error tool result with the reason. The question
/// has a short title, and the command or arguments in full, as written.
///
/// **An allow-list.** A shell command (`bash`) is free only when every
/// command in it is known to do no harm here, and everything it writes is
/// new, the agent's own, or in its [`Scratch`]. The line is read as bash
/// reads it — quotes, escapes, operators, redirections, heredocs,
/// substitutions (each read as a command line too), `bash -c` / `eval` /
/// `trap` text, keywords (`if … then`, `for … do`: the command after them is
/// judged), comments, line continuations — and `cd` is followed as bash's
/// logical `cd` does it. Free:
/// - readers and shell builtins (`ls`, `cat`, `grep`, `rg`, `jq`, `diff`,
///   `sed` without `-i`, `awk` without output or `system`, `[`, `read`, …);
///   making folders and empty files; non-recursive `chmod` / `chown`;
/// - building and testing (the owner's code): `cargo build|test|check|…`,
///   `go build|test|…`, `npm test` / `npm run`, `make` (not `install` or
///   `clean`), `pytest`, `python -m pytest`; the owner's own scripts (`bash
///   build.sh`, `python3 tool.py` when the file is theirs);
/// - git that reads or adds, commits and shares without discarding:
///   `status`, `diff`, `log`, `show`, `add`, `commit`, `push` (not forced),
///   `fetch`, `checkout -b`, `restore --staged`, `reset --soft`, `clean -n`, …,
///   each with the options that keep it so;
/// - writers whose targets are judged: redirections, `cp`, `mv` (the source
///   too: moving away is deleting), `tee`, `dd of=`, `ln`, `install`,
///   `curl -o` / `wget -O`, `rsync` into a new folder, `rm` of the agent's
///   own files, `tar -c`, `uniq IN OUT`.
///
/// Everything else asks: a program not on the list (an interpreter running
/// code it was given, `sudo`, `kill`, `source`, an unknown wrapper), a git
/// subcommand or option not listed (abbreviated, clustered, one that runs a
/// command), a path it cannot know (`$VAR`, `~`, globs, a `cd` it cannot
/// follow), a write onto the owner's file, anything written or appended to
/// `.env*` or `.git` in any case, a name linked earlier in the same line.
///
/// File tools: `write_file` (or pi's `write`) over a file that exists, and
/// `write_file` / `edit_file` (`edit`) of `.env*` or `.git`, or of a path it
/// cannot read (not a string, `~`, `@`), ask; targeted edits are free.
/// Other tools: free when named in [`with_free_tools`](Self::with_free_tools)
/// (by default [`READ_ONLY_TOOLS`]), else they ask.
///
/// The agent's own files are those created by calls that ran: recorded
/// after the call, only if they exist, as that very file (device and inode;
/// a hard link to the owner's file under that name is the owner's).
///
/// **Its folder must be the shell's:** relative paths are judged against
/// the `cwd` given to [`Gate::new`], so the agent's `bash` tool and file
/// tools must work there too.
///
/// **Not a sandbox:** the owner's own scripts and builds run as they are,
/// and a program can change files in ways its command line does not show.
/// For a guarantee, run the agent in a container or a disposable copy.
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
                start: Here::start(&cwd),
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
    /// or runs code.
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

/// Where a shell is: its logical path (what `$PWD` says, what `cd ..`
/// works on) and the real folder (where relative paths land).
#[derive(Clone)]
struct Here {
    logical: PathBuf,
    physical: PathBuf,
}

impl Here {
    /// `cwd`; logically `$PWD` when that names the same folder (bash
    /// inherits it), else `cwd` itself.
    fn start(cwd: &Path) -> Self {
        let physical = real(cwd);
        // Otherwise bash starts from the real folder (`getcwd`).
        let logical = std::env::var_os("PWD")
            .map(PathBuf::from)
            .filter(|pwd| pwd.is_absolute() && real(pwd) == physical)
            .unwrap_or_else(|| physical.clone());
        Self { logical, physical }
    }

    /// Where `cd to` lands, as bash's (logical) `cd` does it.
    fn cd(&self, to: &str) -> Self {
        let logical = lexical(&self.logical, to);
        Self {
            physical: real(&logical),
            logical,
        }
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
        let mut check = Check {
            ask: None,
            details: String::new(),
            creates: Vec::new(),
        };
        match tool {
            "bash" => {
                let Some(command) = args["command"].as_str() else {
                    check.ask = Some("Run `bash` with these arguments?".into());
                    check.details = pretty(args);
                    return check;
                };
                let (ask, creates) = self.line(command, &self.start);
                if ask {
                    check.ask = Some(match command.lines().count() {
                        1 if command.chars().count() <= 80 => format!("Run `{command}`?"),
                        _ => "Run this command?".into(),
                    });
                    check.details = command.to_owned();
                } else {
                    check.creates = creates;
                }
            }
            "write_file" | "write" | "edit_file" | "edit" => {
                let writes = matches!(tool, "write_file" | "write");
                let file = match args["path"].as_str() {
                    // `~` and `@` mean something else to some tools (pi's).
                    Some(file) if !file.is_empty() && !file.starts_with(['~', '@']) => file,
                    _ => {
                        check.ask = Some(format!("Run `{tool}` with these arguments?"));
                        check.details = pretty(args);
                        return check;
                    }
                };
                let path = locate(&self.start.physical, file);
                if protected(Path::new(file)) || protected(&path) {
                    check.ask = Some(format!("Change the protected file {file}?"));
                } else if writes && self.owners(&path) {
                    check.ask = Some(format!("Overwrite the existing file {file}?"));
                } else if writes && !path.exists() {
                    check.creates.push(path);
                }
            }
            tool if self.free_tools.contains(tool) => {}
            tool => {
                check.ask = Some(format!("Run `{tool}`?"));
                check.details = pretty(args);
            }
        }
        check
    }

    fn free(&self, path: &Path) -> bool {
        // Its own file only while it is the same file, and not linked to
        // another name (a hard link to the owner's file is the owner's).
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

    /// A path the agent may not touch without asking: one that exists and is not its own.
    fn owners(&self, path: &Path) -> bool {
        !self.free(path) && path.exists()
    }

    /// Whether `line` needs the owner's yes, and the files it creates.
    fn line(&self, line: &str, start: &Here) -> (bool, Vec<PathBuf>) {
        const ASK: (bool, Vec<PathBuf>) = (true, Vec::new());
        let Some(items) = parse(line) else {
            return ASK;
        };
        let mut creates: Vec<PathBuf> = Vec::new();
        // `None` once a `cd` cannot be followed: every path is then unknown.
        let mut cwd: Option<Here> = Some(start.clone());
        // Directories to return to when a `( … )` subshell ends.
        let mut saved: Vec<Option<Here>> = Vec::new();
        let mut before = ";".to_owned();
        // Names linked earlier in this line (`ln`): a write through one lands
        // elsewhere, so they are unknown for the rest of it.
        let linked: std::cell::RefCell<Vec<PathBuf>> = Default::default();
        let through_link = |path: &Path| linked.borrow().iter().any(|l| path.starts_with(l));

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
                let Some(dir) = cwd
                    .as_ref()
                    .map(|h| h.physical.as_path())
                    .filter(|_| w.known)
                else {
                    return true;
                };
                let path = locate(dir, &w.text);
                if protected(&path) || self.owners(&path) || through_link(&path) {
                    return true;
                }
                if !path.exists() {
                    creates.push(path);
                }
                false
            };
            // A path this command would delete: asks unless it is the agent's own.
            let deletes = |w: &Word| -> bool {
                match cwd
                    .as_ref()
                    .map(|h| h.physical.as_path())
                    .filter(|_| w.known)
                {
                    Some(dir) => {
                        let path = locate(dir, &w.text);
                        !self.free(&path) || through_link(&path)
                    }
                    None => true,
                }
            };

            // What its substitutions run, where it runs (a subshell of this directory).
            for inner in &command.runs {
                let Some(here) = cwd.as_ref() else {
                    return ASK;
                };
                let (ask, made) = self.line(inner, here);
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
            // Appending is free, except to `.env*` or inside `.git/` (a hook).
            for w in &command.appends {
                if DEVICES.contains(&w.text.as_str()) {
                    continue;
                }
                match cwd
                    .as_ref()
                    .map(|h| h.physical.as_path())
                    .filter(|_| w.known)
                {
                    Some(dir) if !protected(&locate(dir, &w.text)) => {}
                    _ => return ASK,
                }
            }
            if command.words.is_empty() {
                continue;
            }
            let program = unwrap(&command.words);
            // A program named by an expansion (`$(echo rm)`, `$X`): unknown.
            if !program.known {
                return ASK;
            }
            // `GIT_CONFIG_*`, `GIT_DIR`, …: configuration or a repository not judged.
            if program.git_env {
                return ASK;
            }
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
                // As bash's (logical) `cd` does it: `..` on the logical path
                // (`link/..` is where `link` sits), then the real folder.
                let dir = match (operands.first(), cwd.as_ref()) {
                    (Some(to), Some(here)) if to.known && name == "cd" => Some(here.cd(&to.text)),
                    _ => None,
                };
                let after = command.after.as_str();
                cwd =
                    dir.filter(|d| d.physical.is_dir() && !piped && after != "&" && after != "||");
                continue;
            }
            // Exporting a `GIT_*` variable sets it for every later git.
            let exports = name == "export"
                || (matches!(name, "declare" | "typeset") && flags.iter().any(|f| f.contains('x')));
            if exports && operands.iter().any(|w| shell::git_variable(w)) {
                return ASK;
            }
            if ALWAYS.contains(&name) || name.starts_with("mkfs.") {
                return ASK;
            }
            if name == "eval" {
                // Read like any command line, when its text is known.
                if args.is_empty() {
                    continue;
                }
                let Some(here) = cwd.as_ref() else {
                    return ASK;
                };
                if args.iter().any(|a| !a.known) {
                    return ASK;
                }
                let text: Vec<&str> = args.iter().map(|a| a.text.as_str()).collect();
                let (ask, made) = self.line(&text.join(" "), here);
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
            // What `xargs` runs gets arguments it does not show: readers only.
            if program.via_xargs && !READS.contains(&name) {
                return ASK;
            }
            if SHELLS.contains(&name) {
                // Options as bash reads them: `-c` anywhere in a cluster
                // (`-xc`, `-cx`), `-o`/`-O` take a value, `--` ends them; the
                // first operand is the script (with `-c`) or its file.
                let (mut i, mut command_string, mut reads_stdin) = (0, false, false);
                while let Some(a) = args.get(i) {
                    let t = a.text.as_str();
                    // `--` and a lone `-` end the options.
                    if t == "--" || t == "-" {
                        i += 1;
                        break;
                    }
                    if !(t.starts_with('-') || t.starts_with('+')) {
                        break;
                    }
                    if ["--rcfile", "--init-file"].contains(&t) {
                        i += 1;
                    } else if !t.starts_with("--") {
                        // A cluster: `-c` anywhere; `-s` reads stdin; one
                        // ending in `o`/`O` takes a value (`-euxo pipefail`).
                        command_string |= t.starts_with('-') && t.contains('c');
                        reads_stdin |= t.starts_with('-') && t.contains('s');
                        if t.ends_with('o') || t.ends_with('O') {
                            i += 1;
                        }
                    }
                    i += 1;
                }
                if reads_stdin && !command_string {
                    return ASK;
                }
                match (command_string, args.get(i)) {
                    (true, body) => {
                        let (Some(body), Some(here)) = (body.filter(|b| b.known), cwd.as_ref())
                        else {
                            return ASK;
                        };
                        let (ask, made) = self.line(&body.text, here);
                        if ask {
                            return ASK;
                        }
                        creates.extend(made);
                    }
                    // No script named: it reads one from stdin (`| sh`, `bash <<EOF`).
                    (false, None) => return ASK,
                    (false, Some(script)) => {
                        if !self.owners_script(script, cwd.as_ref()) {
                            return ASK;
                        }
                    }
                }
                continue;
            }
            if name == "find" {
                let texts: Vec<&str> = args.iter().map(|a| a.text.as_str()).collect();
                if texts.contains(&"-delete") {
                    return ASK;
                }
                for (at, t) in texts.iter().enumerate() {
                    // What `-fprint FILE` and its kin write.
                    if ["-fprint", "-fprint0", "-fprintf", "-fls"].contains(t) {
                        match args.get(at + 1) {
                            Some(file) if !target(file, &mut creates) => {}
                            _ => return ASK,
                        }
                    }
                    // What `-exec` runs: free only when it only reads.
                    if ["-exec", "-execdir", "-ok", "-okdir"].contains(t) {
                        let end = texts[at + 1..]
                            .iter()
                            .position(|w| *w == ";" || *w == "+")
                            .map_or(args.len(), |n| at + 1 + n);
                        let run = unwrap(&args[at + 1..end]);
                        if !run.known || run.git_env || !READS.contains(&run.name.as_str()) {
                            return ASK;
                        }
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
                                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                                .any(|c| c == 'R'))
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
                for w in &operands {
                    let asks = if appends {
                        // Appending is free, except to `.env*` or inside `.git/`.
                        match cwd
                            .as_ref()
                            .map(|h| h.physical.as_path())
                            .filter(|_| w.known)
                        {
                            Some(dir) => protected(&locate(dir, &w.text)),
                            None => true,
                        }
                    } else {
                        target(w, &mut creates)
                    };
                    if asks {
                        return ASK;
                    }
                }
                continue;
            }
            if matches!(name, "mv" | "cp") {
                // Forms it does not follow: a joined target folder, `-T`,
                // copies that make links.
                let unread = args.iter().any(|a| {
                    let t = a.text.as_str();
                    t.starts_with("--target-directory=")
                        || (t.starts_with("-t") && t.len() > 2 && !t.starts_with("--"))
                        || t == "-T"
                        || t == "--no-target-directory"
                        || t == "--link"
                        || t == "--symbolic-link"
                        || (name == "cp"
                            && t.starts_with('-')
                            && !t.starts_with("--")
                            && t[1..].contains(['l', 's']))
                });
                if unread {
                    return ASK;
                }
                let recursive = name == "cp"
                    && args.iter().any(|a| {
                        let t = a.text.as_str();
                        t == "--recursive"
                            || t == "--archive"
                            || (t.starts_with('-')
                                && !t.starts_with("--")
                                && t[1..].contains(['r', 'R', 'a']))
                    });
                let into = args
                    .iter()
                    .position(|a| a.text == "-t" || a.text == "--target-directory");
                let dir = into.and_then(|at| args.get(at + 1));
                let sources: Vec<&Word> = match dir {
                    Some(dir) => operands
                        .iter()
                        .copied()
                        .filter(|a| !std::ptr::eq(*a, dir))
                        .collect(),
                    None => operands[..operands.len().saturating_sub(1)].to_vec(),
                };
                let Some(dest) = dir.or(operands.last().copied()) else {
                    continue;
                };
                if sources.is_empty() {
                    continue;
                }
                // Moving a file away deletes it where it was.
                if name == "mv" && sources.iter().any(|s| deletes(s)) {
                    return ASK;
                }
                let Some(here) = cwd
                    .as_ref()
                    .map(|h| h.physical.as_path())
                    .filter(|_| dest.known)
                else {
                    return ASK;
                };
                let dest_path = locate(here, &dest.text);
                // A whole tree into a folder that is there: what lands where
                // is not read from the command (`cp -R dir/ .` copies dir's
                // contents), so only into a folder of the agent's own.
                if recursive && dest_path.is_dir() && !self.free(&dest_path) {
                    return ASK;
                }
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
            if name == "ln" {
                // Forced: it replaces what is at the link's name.
                let forced = flags
                    .iter()
                    .any(|f| *f == "--force" || (!f.starts_with("--") && f[1..].contains('f')));
                let Some(dest) = operands.last().filter(|d| d.known) else {
                    return ASK;
                };
                let Some(dir) = cwd.as_ref().map(|h| h.physical.as_path()) else {
                    return ASK;
                };
                if forced && target(dest, &mut creates) {
                    return ASK;
                }
                let link = locate(dir, &dest.text);
                let link = if operands.len() > 1 && link.is_dir() {
                    // `ln -s a b dir/`: links inside the folder, named as given.
                    return ASK;
                } else {
                    link
                };
                linked.borrow_mut().push(link);
                continue;
            }
            if name == "install" {
                if let Some(dest) = operands.last() {
                    if operands.len() >= 2 && target(dest, &mut creates) {
                        return ASK;
                    }
                }
                continue;
            }
            if name == "rsync" {
                if flags
                    .iter()
                    .any(|f| f.starts_with("--delete") || *f == "--remove-source-files")
                {
                    return ASK;
                }
                // Into a folder that is not there yet, or the agent's own.
                let Some(dest) = operands.last().filter(|d| d.known && operands.len() >= 2) else {
                    return ASK;
                };
                let Some(dir) = cwd.as_ref().map(|h| h.physical.as_path()) else {
                    return ASK;
                };
                let path = locate(dir, &dest.text);
                if path.exists() && !self.free(&path) {
                    return ASK;
                }
                continue;
            }
            if matches!(name, "curl" | "wget") {
                // Where it writes: `-o FILE`, `-oFILE`, `-sSLo FILE`,
                // `--output[=]FILE` (wget: `-O`, `--output-document`; its log
                // `-o`, `--output-file`). `curl -O` names the file from the URL.
                let (short, long): (&[char], &[&str]) = if name == "curl" {
                    (
                        &['o', 'D', 'c'],
                        &[
                            "--output",
                            "--dump-header",
                            "--cookie-jar",
                            "--trace",
                            "--trace-ascii",
                            "--stderr",
                        ],
                    )
                } else {
                    (&['O', 'o'], &["--output-document", "--output-file"])
                };
                // Writing where it cannot see: another folder, files named
                // from the server, overwriting by date.
                let elsewhere: &[&str] = if name == "curl" {
                    &["--output-dir", "--create-dirs"]
                } else {
                    &[
                        "-N",
                        "--timestamping",
                        "-r",
                        "--recursive",
                        "-m",
                        "--mirror",
                        "-P",
                        "--directory-prefix",
                    ]
                };
                if args.iter().any(|a| {
                    elsewhere.contains(&a.text.as_str())
                        || elsewhere
                            .iter()
                            .any(|e| a.text.starts_with(&format!("{e}=")))
                        || (name == "wget"
                            && a.text.starts_with('-')
                            && !a.text.starts_with("--")
                            && a.text[1..].contains(['N', 'r', 'm', 'P']))
                }) {
                    return ASK;
                }
                if name == "curl"
                    && args.iter().any(|a| {
                        a.text == "--remote-name"
                            || a.text == "--remote-name-all"
                            || (a.text.starts_with('-')
                                && !a.text.starts_with("--")
                                && a.text.contains('O'))
                    })
                {
                    return ASK;
                }
                let mut at = 0;
                while let Some(a) = args.get(at) {
                    let t = a.text.as_str();
                    let mut value: Option<Word> = None;
                    if long.contains(&t) {
                        value = args.get(at + 1).cloned();
                    } else if let Some(joined) =
                        long.iter().find_map(|o| t.strip_prefix(&format!("{o}=")))
                    {
                        value = Some(Word {
                            text: joined.to_owned(),
                            known: a.known,
                        });
                    } else if t.starts_with('-') && !t.starts_with("--") {
                        if let Some(pos) = t[1..].find(|c| short.contains(&c)) {
                            let rest = &t[2 + pos..];
                            value = if rest.is_empty() {
                                args.get(at + 1).cloned()
                            } else {
                                Some(Word {
                                    text: rest.to_owned(),
                                    known: a.known,
                                })
                            };
                        }
                    }
                    if let Some(value) = value {
                        if value.text != "-" && target(&value, &mut creates) {
                            return ASK;
                        }
                    }
                    at += 1;
                }
                continue;
            }
            if name == "git" {
                if !git_free(args, cwd.as_ref().map(|h| h.physical.as_path())) {
                    return ASK;
                }
                continue;
            }
            if name == "trap" {
                // Its first argument is shell text run later: read it now.
                let Some(here) = cwd.as_ref() else {
                    return ASK;
                };
                match args.first() {
                    Some(code) if code.known => {
                        let (ask, made) = self.line(&code.text, here);
                        if ask {
                            return ASK;
                        }
                        creates.extend(made);
                    }
                    Some(_) => return ASK,
                    None => {}
                }
                continue;
            }
            if INTERPRETERS.contains(&name) {
                if !self.interpreter_free(name, args, cwd.as_ref()) {
                    return ASK;
                }
                continue;
            }
            // Everything else: free only when it is known to do no harm here.
            if !allowed(name, args, &operands, &flags) {
                return ASK;
            }
            // `uniq IN OUT` writes OUT; `tar -cf FILE` writes FILE.
            if name == "uniq" && operands.len() >= 2 && target(operands[1], &mut creates) {
                return ASK;
            }
            if name == "zip" {
                if let Some(archive) = operands.first() {
                    if target(archive, &mut creates) {
                        return ASK;
                    }
                }
            }
            if name == "tar" {
                if let Some(file) = tar_file(args) {
                    if creates_archive(args) && target(&file, &mut creates) {
                        return ASK;
                    }
                }
            }
        }
        (false, creates)
    }

    /// A script the owner has (it exists, the agent did not make it, it is
    /// not in scratch): running it is the owner's code.
    fn owners_script(&self, script: &Word, cwd: Option<&Here>) -> bool {
        let Some(here) = cwd.filter(|_| script.known) else {
            return false;
        };
        let path = locate(&here.physical, &script.text);
        path.is_file() && !self.free(&path)
    }

    /// An interpreter running the owner's script, or a known module or flag
    /// that runs no code of the agent's.
    fn interpreter_free(&self, name: &str, args: &[Word], cwd: Option<&Here>) -> bool {
        let text = |i: usize| args.get(i).map(|w| w.text.as_str()).unwrap_or("");
        if ["--version", "-V", "-v", "--help"].contains(&text(0)) && args.len() == 1 {
            return true;
        }
        if name.starts_with("python") && text(0) == "-m" {
            return match text(1) {
                "pytest" | "unittest" | "mypy" | "pyflakes" | "pylint" | "compileall" => true,
                "pip" => matches!(text(2), "list" | "show" | "freeze" | "check"),
                _ => false,
            };
        }
        if name == "node" && text(0) == "--test" {
            return true;
        }
        match args.first() {
            Some(script) if !script.text.starts_with('-') => self.owners_script(script, cwd),
            _ => false,
        }
    }
}

/// Interpreters: they run whatever code they are given.
const INTERPRETERS: [&str; 10] = [
    "python", "python3", "node", "deno", "bun", "ruby", "perl", "php", "lua", "Rscript",
];

/// Whether a program with no rule of its own is known to do no harm with
/// these arguments: it reads, builds, tests, or writes only what the
/// redirections and rules above already judged.
fn allowed(name: &str, args: &[Word], operands: &[&Word], flags: &[&str]) -> bool {
    let first = operands.first().map(|w| w.text.as_str()).unwrap_or("");
    let cluster = |c: char| {
        flags
            .iter()
            .any(|f| !f.starts_with("--") && f[1..].contains(c))
    };
    let has = |f: &str| flags.contains(&f) || flags.iter().any(|g| g.starts_with(&format!("{f}=")));
    match name {
        // Shell keywords that end or open a block, and loop headers (data).
        "fi" | "done" | "esac" | "}" | "]]" | "for" | "select" => true,
        // Readers.
        "ls" | "cat" | "head" | "tail" | "wc" | "grep" | "egrep" | "fgrep" | "rg" | "fd"
        | "file" | "stat" | "du" | "df" | "pwd" | "echo" | "printf" | "true" | "false" | "test"
        | "[" | "[[" | ":" | "which" | "type" | "whereis" | "basename" | "dirname" | "realpath"
        | "readlink" | "date" | "printenv" | "whoami" | "id" | "uname" | "hostname" | "cut"
        | "tr" | "comm" | "diff" | "cmp" | "tree" | "md5sum" | "sha1sum" | "sha256sum"
        | "shasum" | "xxd" | "od" | "hexdump" | "nl" | "column" | "fold" | "expand"
        | "unexpand" | "seq" | "sleep" | "jq" | "yq" | "wait" | "jobs" | "uniq" | "mktemp"
        | "command" => true,
        // Shell state.
        "export" | "unset" | "set" | "local" | "readonly" | "declare" | "typeset" | "read"
        | "shift" | "break" | "continue" | "return" | "exit" => true,
        // Making a folder or a new empty file, or a time stamp.
        "mkdir" | "touch" => true,
        // Permissions of one file at a time (recursive ones ask).
        "chmod" | "chown" => !(has("--recursive") || cluster('R')),
        // Output to a file, or a program to compress with.
        "sort" => !(has("--output") || cluster('o') || has("--compress-program")),
        "sed" => sed_free(args),
        "awk" | "gawk" | "mawk" => {
            // Options other than a separator and variables can bring in a
            // program not read here (`-f`, `--source`, …).
            let mut i = 0;
            let mut readable = true;
            while let Some(a) = args.get(i) {
                match a.text.as_str() {
                    "-F" | "-v" => i += 2,
                    t if t.starts_with("-F") || t.starts_with("-v") => i += 1,
                    "--" => {
                        i += 1;
                        break;
                    }
                    t if t.starts_with('-') => {
                        readable = false;
                        break;
                    }
                    _ => break,
                }
            }
            // Its program can write files and run commands.
            let program = args.get(i).filter(|w| w.known).map(|w| w.text.as_str());
            readable
                && program.is_some_and(|p| {
                    !["system", ">", "|", "getline", "fflush", "close"]
                        .iter()
                        .any(|t| p.contains(t))
                })
        }
        // Listing or creating an archive (its file judged by the caller);
        // never removing files or running a program.
        "tar" => {
            let mode = args
                .first()
                .map(|w| w.text.trim_start_matches('-'))
                .unwrap_or("");
            let runs = [
                "--remove-files",
                "--to-command",
                "--use-compress-program",
                "--checkpoint-action",
                "--info-script",
                "--new-volume-script",
                "--rmt-command",
                "--rsh-command",
            ]
            .iter()
            .any(|o| has(o))
                || flags.contains(&"-I")
                || flags.contains(&"-F");
            (mode.contains('t') || has("--list") || creates_archive(args))
                && !mode.contains('x')
                && !runs
        }
        "unzip" => cluster('l') || cluster('t') || cluster('v'),
        // Its archive is judged by the caller; never moving files in or
        // running a test command.
        "zip" => !cluster('m') && !has("--move") && !has("-TT") && !has("--unzip-command"),
        // Building and testing: they run the owner's code.
        "cargo" => {
            let builds = matches!(
                first,
                "build"
                    | "b"
                    | "check"
                    | "c"
                    | "test"
                    | "t"
                    | "fmt"
                    | "doc"
                    | "d"
                    | "tree"
                    | "metadata"
                    | "run"
                    | "r"
                    | "bench"
                    | "version"
                    | "search"
                    | "nextest"
                    | "locate-project"
                    | "pkgid"
                    | "verify-project"
            );
            let lints = first == "clippy" && !has("--fix");
            // Pointed at code elsewhere: not the owner's build.
            if has("--manifest-path")
                || has("--config")
                || flags.iter().any(|f| f.starts_with("-Z"))
            {
                return false;
            }
            let version = operands.is_empty()
                && flags
                    .iter()
                    .all(|f| ["--version", "-V", "--help"].contains(f));
            builds || lints || version
        }
        "go" => {
            // `go run file.go` runs that file; `-C` builds elsewhere.
            let elsewhere = has("-C") || operands.iter().any(|w| w.text.ends_with(".go"));
            !elsewhere
                && matches!(
                    first,
                    "build" | "test" | "vet" | "fmt" | "run" | "list" | "env" | "version"
                )
                || (first == "mod"
                    && matches!(
                        operands.get(1).map(|w| w.text.as_str()),
                        Some("graph" | "why" | "verify" | "download")
                    ))
        }
        "npm" | "pnpm" | "yarn" => {
            !(has("--prefix") || has("-C") || has("--dir"))
                && matches!(
                    first,
                    "test"
                        | "t"
                        | "run"
                        | "run-script"
                        | "ls"
                        | "list"
                        | "view"
                        | "info"
                        | "outdated"
                        | "why"
                )
                || (first == "audit" && !operands.iter().any(|w| w.text == "fix"))
        }
        // The owner's Makefile, here (not `-f`/`-C` elsewhere).
        "make" | "gmake" => {
            ![
                "-f",
                "--file",
                "--makefile",
                "-C",
                "--directory",
                "-I",
                "--include-dir",
            ]
            .iter()
            .any(|o| has(o) || flags.iter().any(|f| f.starts_with(o) && o.len() == 2))
                && !operands.iter().any(|w| {
                    ["install", "uninstall", "clean", "distclean"].contains(&w.text.as_str())
                })
        }
        "pytest" | "ctest" | "tsc" | "eslint" | "prettier" | "vitest" | "jest" | "mypy" => {
            // Not rewriting files, nor loading plugins or configuration from
            // elsewhere.
            ![
                "--write",
                "--fix",
                "-p",
                "-c",
                "--rootdir",
                "--config",
                "--config-file",
                "--confcutdir",
            ]
            .iter()
            .any(|o| has(o))
        }
        // `npx tsc` and the like: the checkers above.
        "npx" => {
            let tool = operands.first().map(|w| w.text.as_str()).unwrap_or("");
            ["tsc", "eslint", "prettier", "vitest", "jest"].contains(&tool)
                && !["--write", "--fix", "-c", "--config"]
                    .iter()
                    .any(|o| has(o))
        }
        "uv" | "poetry" => {
            first == "run"
                && matches!(
                    operands.get(1).map(|w| w.text.as_str()),
                    Some("pytest" | "mypy")
                )
                && !["-p", "-c", "--rootdir"].iter().any(|o| has(o))
        }
        "rustfmt" => has("--check"),
        // GitHub's CLI, reading.
        "gh" => {
            let what = operands.get(1).map(|w| w.text.as_str()).unwrap_or("");
            matches!(
                first,
                "pr" | "issue" | "run" | "release" | "repo" | "workflow"
            ) && matches!(what, "view" | "list" | "diff" | "checks" | "status")
                && !has("--web")
        }
        "rustc" | "rustup" | "gcc" | "clang" | "cc" => {
            flags.iter().any(|f| ["--version", "-V", "-v"].contains(f)) && operands.is_empty()
                || (name == "rustup"
                    && matches!(first, "show" | "toolchain" if operands.get(1).map(|w| w.text.as_str()) != Some("install")))
        }
        _ => false,
    }
}

/// Whether a `sed` call only reads: no `-i`, no script file, and every
/// command of its script one that prints, deletes, quits or substitutes
/// (`s///` without the `w` or `e` flags).
fn sed_free(args: &[Word]) -> bool {
    let mut scripts: Vec<&str> = Vec::new();
    let mut i = 0;
    let mut explicit = false;
    while let Some(a) = args.get(i) {
        let t = a.text.as_str();
        match t {
            "-n" | "--quiet" | "--silent" | "-E" | "-r" | "--regexp-extended" | "-s" | "-u"
            | "-z" | "--posix" => {}
            "-e" | "--expression" => {
                match args.get(i + 1).filter(|w| w.known) {
                    Some(w) => scripts.push(&w.text),
                    None => return false,
                }
                explicit = true;
                i += 1;
            }
            t if t.starts_with('-') => return false,
            _ if !explicit && scripts.is_empty() => {
                if !a.known {
                    return false;
                }
                scripts.push(t);
            }
            _ => {}
        }
        i += 1;
    }
    !scripts.is_empty() && scripts.iter().all(|s| sed_script_reads(s))
}

fn sed_script_reads(script: &str) -> bool {
    let chars: Vec<char> = script.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // Separators and an address (line numbers, `$`, `/re/`, ranges).
        while i < chars.len() && (chars[i].is_whitespace() || chars[i] == ';') {
            i += 1;
        }
        loop {
            match chars.get(i) {
                Some(c) if c.is_ascii_digit() || matches!(c, '$' | ',' | '!' | '~' | '+') => i += 1,
                Some('/') => {
                    i += 1;
                    while i < chars.len() && chars[i] != '/' {
                        if chars[i] == '\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                    i += 1;
                }
                _ => break,
            }
        }
        match chars.get(i) {
            None => break,
            Some(
                'p' | 'P' | 'd' | 'D' | 'q' | 'Q' | '=' | 'n' | 'N' | 'g' | 'G' | 'h' | 'H' | 'x'
                | 'l',
            ) => i += 1,
            Some('{' | '}') => i += 1,
            Some('s') => {
                let Some(&delim) = chars.get(i + 1) else {
                    return false;
                };
                i += 2;
                for _ in 0..2 {
                    while i < chars.len() && chars[i] != delim {
                        if chars[i] == '\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                    i += 1;
                }
                while let Some(c) = chars.get(i) {
                    if matches!(c, 'g' | 'i' | 'I' | 'p' | 'm' | 'M') || c.is_ascii_digit() {
                        i += 1;
                    } else {
                        break;
                    }
                }
            }
            // `w`, `W`, `e`, `r`, `R`, `a`/`i`/`c` text, `y`, labels: not read here.
            Some(_) => return false,
        }
    }
    true
}

/// `tar`'s archive file (`-f FILE`, `-cf FILE`, `cf FILE`, `--file=FILE`).
fn tar_file(args: &[Word]) -> Option<Word> {
    for (at, a) in args.iter().enumerate() {
        let t = a.text.as_str();
        if let Some(file) = t.strip_prefix("--file=") {
            return Some(Word {
                text: file.to_owned(),
                known: a.known,
            });
        }
        let cluster = t.trim_start_matches('-');
        if (at == 0 || t.starts_with('-')) && !t.starts_with("--") && cluster.ends_with('f') {
            return args.get(at + 1).cloned();
        }
    }
    None
}

/// `tar -c…` (or `--create`).
fn creates_archive(args: &[Word]) -> bool {
    args.iter().any(|a| a.text == "--create")
        || args.first().is_some_and(|a| {
            !a.text.starts_with("--") && a.text.trim_start_matches('-').contains('c')
        })
}

/// Whether a git command is known to do no harm: the subcommands that read,
/// or add, commit and share without discarding anything, with the options
/// that keep them so. Anything else — a subcommand or option not listed, an
/// abbreviation (`--har`), a cluster (`-qf`), `-c`, a `--git-dir` elsewhere,
/// an option that runs a command (`-x`, `--upload-pack`, `-O`) — asks.
fn git_free(args: &[Word], cwd: Option<&Path>) -> bool {
    let mut i = 0;
    while let Some(a) = args.get(i) {
        match a.text.as_str() {
            "--no-pager" | "-P" | "--no-optional-locks" | "--literal-pathspecs" => i += 1,
            // A folder for git to work in: the paths after it are not read here.
            "-C" => i += 2,
            t if t.starts_with('-') => return false,
            _ => break,
        }
    }
    if args.iter().any(|a| !a.known) {
        return false;
    }
    let sub = args.get(i).map(|w| w.text.as_str()).unwrap_or("");
    let rest: Vec<&str> = args[(i + 1).min(args.len())..]
        .iter()
        .map(|w| w.text.as_str())
        .collect();
    let flags: Vec<&str> = rest
        .iter()
        .copied()
        .filter(|t| t.starts_with('-') && *t != "--")
        .collect();
    let operands: Vec<&str> = rest
        .iter()
        .copied()
        .filter(|t| !t.starts_with('-'))
        .collect();
    // Every option is one of these (exactly, or `--opt=value` for a long one).
    let only = |safe: &[&str]| {
        flags.iter().all(|f| {
            safe.contains(f)
                || f.split_once('=')
                    .is_some_and(|(name, _)| name.starts_with("--") && safe.contains(&name))
        })
    };
    // Options no reading subcommand may have: they write a file or run one.
    let runs_or_writes = flags.iter().any(|f| {
        f.starts_with("--output")
            || f.starts_with("-O")
            || f.starts_with("--open-files-in-pager")
            || f.starts_with("--ext-diff")
            || f.starts_with("--exec")
            || f.starts_with("--upload-pack")
            || f.starts_with("--receive-pack")
            || f.starts_with("--config")
            || *f == "-x"
    });
    match sub {
        "" | "--version" | "version" | "help" => true,
        // Reading.
        "status" | "diff" | "log" | "show" | "blame" | "ls-files" | "ls-tree" | "rev-parse"
        | "rev-list" | "describe" | "shortlog" | "cat-file" | "merge-base" | "name-rev"
        | "for-each-ref" | "show-ref" | "grep" | "whatchanged" | "count-objects" | "var"
        | "check-ignore" | "check-attr" | "show-branch" | "cherry" | "range-diff" => {
            !runs_or_writes
        }
        "reflog" => {
            matches!(operands.first(), None | Some(&"show") | Some(&"list")) && !runs_or_writes
        }
        "remote" => {
            matches!(operands.first(), None | Some(&"show") | Some(&"get-url"))
                && only(&["-v", "--verbose"])
        }
        "worktree" => operands.first() == Some(&"list"),
        "stash" => match operands.first() {
            Some(&"list") | Some(&"show") => !runs_or_writes,
            // Putting changes aside keeps them (untracked files too would
            // leave the tree: `-u`, `--all` ask).
            None | Some(&"push") | Some(&"save") => only(&[
                "-m",
                "--message",
                "-q",
                "--quiet",
                "--staged",
                "-S",
                "--keep-index",
                "-k",
                "-p",
                "--patch",
            ]),
            _ => false,
        },
        "branch" => only(&[
            "-a",
            "--all",
            "-r",
            "--remotes",
            "-v",
            "-vv",
            "--verbose",
            "--list",
            "-l",
            "--show-current",
            "--contains",
            "--merged",
            "--no-merged",
            "--sort",
            "--format",
            "--no-color",
            "--color",
        ]),
        "tag" => only(&[
            "-l",
            "--list",
            "-n",
            "--sort",
            "--contains",
            "--points-at",
            "--format",
            "-a",
            "--annotate",
            "-m",
            "--message",
            "-s",
            "--sign",
        ]),
        "config" => {
            let reads = [
                "--get",
                "--get-all",
                "--get-regexp",
                "--get-urlmatch",
                "--list",
                "-l",
                "--show-origin",
                "--show-scope",
                "--name-only",
                "--global",
                "--local",
                "--system",
                "--worktree",
                "--includes",
                "--null",
                "-z",
                "--type",
                "--file",
                "-f",
            ];
            match operands.first() {
                Some(&"get") | Some(&"list") => only(&reads),
                _ => {
                    only(&reads)
                        && (flags
                            .iter()
                            .any(|f| f.starts_with("--get") || *f == "--list" || *f == "-l")
                            || operands.len() <= 1)
                }
            }
        }
        // Adding and committing: nothing is discarded.
        "add" => only(&[
            "-A",
            "--all",
            "-u",
            "--update",
            "-N",
            "--intent-to-add",
            "-p",
            "--patch",
            "-v",
            "--verbose",
            "-n",
            "--dry-run",
            "-f",
            "--force",
            "--",
        ]),
        "commit" => only(&[
            "-m",
            "--message",
            "-a",
            "--all",
            "-q",
            "--quiet",
            "-v",
            "--verbose",
            "-s",
            "--signoff",
            "--amend",
            "--no-edit",
            "--allow-empty",
            "-S",
            "--gpg-sign",
            "--no-verify",
            "-n",
            "-F",
            "--file",
            "--author",
            "--date",
        ]),
        "init" => only(&["-q", "--quiet", "-b", "--initial-branch", "--bare"]),
        "clone" => {
            only(&[
            "--depth",
            "--branch",
            "-b",
            "-q",
            "--quiet",
            "--single-branch",
            "--no-tags",
            "--filter",
            "--no-checkout",
            "--bare",
            "--origin",
            "-o",
            "--shallow-since",
        ])
                // Into a folder that is not there yet (its last operand, when it names one).
                && (operands.len() < 2
                    || cwd.is_some_and(|c| !locate(c, operands[operands.len() - 1]).exists()))
        }
        "fetch" => {
            only(&[
            "--all",
            "--prune",
            "-p",
            "--tags",
            "-t",
            "-q",
            "--quiet",
            "-v",
            "--verbose",
            "--depth",
            "--unshallow",
            "--no-tags",
            "--dry-run",
        ])
                // A refspec naming a local ref (`src:dst`, `+…`) can overwrite a branch.
                && !operands.iter().skip(1).any(|r| r.contains(':') || r.starts_with('+'))
        }
        "pull" => {
            only(&[
            "--ff-only",
            "--no-rebase",
            "--rebase",
            "-r",
            "-q",
            "--quiet",
            "-v",
            "--verbose",
            "--no-edit",
            "--tags",
        ])
                // A refspec naming a local ref (`src:dst`, `+…`) can overwrite a branch.
                && !operands.iter().skip(1).any(|r| r.contains(':') || r.starts_with('+'))
        }
        "push" => {
            only(&[
                "-u",
                "--set-upstream",
                "-q",
                "--quiet",
                "-v",
                "--verbose",
                "--tags",
                "--follow-tags",
                "--dry-run",
                "-n",
                "--no-verify",
            ]) && !operands
                .iter()
                .any(|r| r.starts_with('+') || r.starts_with(':'))
        }
        // Making and moving to a new branch; unstaging; a dry run.
        "checkout" => {
            flags.iter().any(|f| ["-b", "--orphan"].contains(f))
                && only(&[
                    "-b",
                    "--orphan",
                    "-q",
                    "--quiet",
                    "--no-track",
                    "--track",
                    "-t",
                ])
                && operands.len() <= 2
        }
        "switch" => {
            flags
                .iter()
                .any(|f| ["-c", "--create", "--orphan"].contains(f))
                && only(&[
                    "-c",
                    "--create",
                    "--orphan",
                    "-q",
                    "--quiet",
                    "--no-track",
                    "--track",
                    "-t",
                ])
                && operands.len() <= 2
        }
        "restore" => {
            flags.iter().any(|f| ["--staged", "-S"].contains(f))
                && only(&["--staged", "-S", "-q", "--quiet", "-p", "--patch"])
        }
        "rm" => {
            flags.contains(&"--cached")
                && only(&["--cached", "-r", "-q", "--quiet", "-n", "--dry-run", "--"])
        }
        "reset" => only(&["--soft", "--mixed", "-q", "--quiet", "--"]) && operands.len() <= 1,
        "clean" => {
            flags
                .iter()
                .all(|f| ["-n", "--dry-run", "-d", "-x", "-X"].contains(f))
                && flags.iter().any(|f| ["-n", "--dry-run"].contains(f))
        }
        _ => false,
    }
}

/// A path with a component starting `.env` (`.env`, `.env.local`, …) or
/// inside `.git/`, in any case (macOS and Windows file systems ignore it).
fn protected(path: &Path) -> bool {
    let parts: Vec<String> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(name) => Some(name.to_string_lossy().to_lowercase()),
            _ => None,
        })
        .collect();
    parts.iter().any(|p| p.starts_with(".env") || p == ".git")
}

/// A call's arguments in full, for a question.
fn pretty(args: &Json) -> String {
    serde_json::to_string_pretty(args).unwrap_or_else(|_| args.to_string())
}

/// A file's device and inode (not following a symlink at the end).
fn identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path)
        .ok()
        .map(|m| (m.dev(), m.ino()))
}

/// A folder, or a file with one name only.
fn single(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir() || m.nlink() == 1)
}

/// `file` against `dir`, with `.` and `..` applied and symlinks resolved.
/// As the kernel resolves it: a `..` applies to the path so far with its
/// symlinks followed (`link/..` is the parent of where `link` points).
fn locate(dir: &Path, file: &str) -> PathBuf {
    let mut out = dir.to_owned();
    for part in Path::new(file).components() {
        match part {
            Component::Prefix(_) | Component::RootDir => out = PathBuf::from(part.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                out = real(&out);
                out.pop();
            }
            Component::Normal(name) => out.push(name),
        }
    }
    real(&out)
}

/// `path` with symlinks resolved, also for a file not created yet — and a
/// dangling symlink resolved to where it points (a write through it lands
/// there).
fn real(path: &Path) -> PathBuf {
    real_within(path, 40)
}

fn real_within(path: &Path, hops: usize) -> PathBuf {
    if let Ok(real) = path.canonicalize() {
        return real;
    }
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return path.to_owned();
    };
    if parent == path {
        return path.to_owned();
    }
    let here = real_within(parent, hops).join(name);
    match std::fs::read_link(&here) {
        Ok(target) if hops > 0 => {
            real_within(&here.parent().unwrap_or(&here).join(target), hops - 1)
        }
        _ => here,
    }
}

/// `file` against `dir` as text: `..` drops the component before it,
/// whatever it is (bash's logical `cd`).
fn lexical(dir: &Path, file: &str) -> PathBuf {
    let mut out = dir.to_owned();
    for part in Path::new(file).components() {
        match part {
            Component::Prefix(_) | Component::RootDir => out = PathBuf::from(part.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(name) => out.push(name),
        }
    }
    out
}

#[cfg(test)]
mod tests;
