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

/// An extension that asks the owner before a tool call deletes, discards
/// or overwrites what they already have — and before any tool it has no rule
/// for — and refuses it when nobody answers "Allow" (no frontend, a timeout,
/// or "Block"). The refusal reaches the model as an error tool result with
/// the reason.
///
/// Shell commands (`bash`) are read command by command — quotes, escapes,
/// operators, redirections, heredocs, substitutions, `bash -c` / `eval` of
/// literal text, backslash-newlines joined — following `cd` only where it
/// surely applies (not through a symlink: bash's `cd` is logical). What asks:
/// - `rm`, `unlink`, `rmdir`, `git rm` (not `--cached`), `xargs` running
///   one of them, `find … -delete` or `-exec rm …`, `mv` of a file that is
///   not the agent's (moving it away is deleting it here);
/// - git: `reset --hard`/`--merge`/`--keep`, `clean` (not `-n`),
///   `checkout` / `restore` of paths (not `restore --staged`),
///   `checkout -f`, `switch -f` / `--discard-changes`, `branch -D`, a forced
///   push (`-f`, `--force*`, `+ref`, `:ref`, `--mirror`, `--delete`),
///   `stash drop` / `clear`, any `git -c` and `git config` write, and any
///   `GIT_*` variable set for a command (an alias can run a shell);
/// - `mv`, `cp`, `tee` (without `-a`), `dd of=`, `ln -f`, `install`,
///   `curl -o` / `wget -O` (also in a cluster, `-sSLo`), `curl -O` and
///   truncating redirections (`>`, `>|`, `2>`,
///   `&>`, `>&file`; not `>>`, not to `/dev/null`) onto a file that exists;
///   `rsync --delete` / `--remove-source-files`;
/// - `sudo`, `truncate`, `shred`, `mkfs`, recursive `chmod` / `chown`, a
///   shell reading its script from stdin (`| sh`, `bash <<EOF`), a shell or
///   deleter run by any `find -exec`;
/// - anything written or appended to `.env*`, `.git` or inside `.git/` (any
///   case), also through a dangling symlink;
/// - anything it cannot know: a path with `$VAR`, `~`, globs or after a `cd`
///   it cannot follow; a program named by an expansion (`$(echo rm)`).
///
/// File tools: `write_file` (or pi's `write`) over a file that exists, and
/// `write_file` / `edit_file` (`edit`) of `.env*` or inside `.git/`, or of a
/// path it cannot read (not a string, `~`, `@`).
///
/// Other tools: free when named in [`with_free_tools`](Self::with_free_tools)
/// (by default [`READ_ONLY_TOOLS`]); any other tool asks — a plugin's tool
/// may run code or delete files, and the gate cannot see inside it.
///
/// Free: files the agent created in this session (by a call that ran),
/// anything in its [`Scratch`], and targeted edits of other files. Paths are
/// resolved as the kernel does: symlinks followed before `..`.
///
/// **Not a sandbox:** a script or program can still change files in ways a
/// command line does not show (`sed -i`, `python3 -c …`, `bash script.sh`, a
/// Makefile). For a guarantee, run the agent in a container or a disposable
/// copy.
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
                created: Mutex::default(),
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
            created: Mutex::new(self.rules.created.lock().unwrap().clone()),
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
pub struct Check {
    /// The question for the owner, when it needs their yes.
    pub ask: Option<String>,
    /// Files it creates: the agent's own once the call has run.
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
        // The files it makes become the agent's own once it has run (a
        // recheck replaces what the first look found).
        self.pending
            .lock()
            .unwrap()
            .insert(call.tool_call_id.to_owned(), check.creates);
        ToolDecision::Allow
    }

    async fn after_tool(
        &self,
        call: &ToolCallRequest<'_>,
        output: &mut ToolOutput,
    ) -> Result<(), ExtensionError> {
        let creates = self.pending.lock().unwrap().remove(call.tool_call_id);
        if let (Some(creates), false) = (creates, output.is_error) {
            self.rules.created.lock().unwrap().extend(creates);
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
        let logical = std::env::var_os("PWD")
            .map(PathBuf::from)
            .filter(|pwd| pwd.is_absolute() && real(pwd) == physical)
            .unwrap_or_else(|| cwd.to_owned());
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
                let Some(command) = args["command"].as_str() else {
                    check.ask = Some(format!("Run `bash` with {}?", short(args)));
                    return check;
                };
                let (ask, creates) = self.line(command, &self.start);
                check.creates = creates;
                if ask {
                    check.ask = Some(format!("Run `{command}`?"));
                }
            }
            "write_file" | "write" | "edit_file" | "edit" => {
                let writes = matches!(tool, "write_file" | "write");
                let file = match args["path"].as_str() {
                    // `~` and `@` mean something else to some tools (pi's).
                    Some(file) if !file.is_empty() && !file.starts_with(['~', '@']) => file,
                    _ => {
                        check.ask = Some(format!("Run `{tool}` with {}?", short(args)));
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
            tool => check.ask = Some(format!("Run `{tool}` with {}?", short(args))),
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
                if protected(&path) || self.owners(&path) {
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
                    Some(dir) => !self.free(&locate(dir, &w.text)),
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
            if program.via_xargs && ["mv", "cp", "dd", "tee", "git"].contains(&name) {
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
                    (false, Some(_)) => {}
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
                if forced {
                    let Some(dest) = operands.last() else {
                        return ASK;
                    };
                    if target(dest, &mut creates) {
                        return ASK;
                    }
                }
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
            if name == "rsync"
                && flags
                    .iter()
                    .any(|f| f.starts_with("--delete") || *f == "--remove-source-files")
            {
                return ASK;
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
            if name == "git" && self.git_risky(args, cwd.as_ref().map(|h| h.physical.as_path())) {
                return ASK;
            }
        }
        (false, creates)
    }

    fn git_risky(&self, args: &[Word], cwd: Option<&Path>) -> bool {
        let mut i = 0;
        while i < args.len() && args[i].text.starts_with('-') {
            let option = args[i].text.as_str();
            // Configuration on the command line can define an alias that runs
            // a shell (`alias.x=!rm …`) or a command git runs: not judged.
            if option == "-c" || option.starts_with("--config-env") {
                return true;
            }
            if ["-C", "--git-dir", "--work-tree", "--namespace"].contains(&option) {
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
            // Writing configuration: an alias or a hook path can run a shell.
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
                ];
                // Where it reads from does not make a read a write.
                let scope = [
                    "--global",
                    "--local",
                    "--system",
                    "--worktree",
                    "--includes",
                    "--null",
                    "-z",
                ];
                let other_flags = flags
                    .iter()
                    .filter(|f| {
                        !scope.contains(f) && !f.starts_with("--type") && !f.starts_with("--file")
                    })
                    .count();
                match paths.first().copied() {
                    // git 2.46+: `git config get|list …` read, the rest write.
                    Some("get") | Some("list") => false,
                    Some("set")
                    | Some("unset")
                    | Some("rename-section")
                    | Some("remove-section")
                    | Some("edit") => true,
                    _ => !(reads.iter().any(|r| has(r)) || (other_flags == 0 && paths.len() <= 1)),
                }
            }
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

/// A call's arguments, briefly, for a question.
fn short(args: &Json) -> String {
    let text = args.to_string();
    match text.char_indices().nth(200) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text,
    }
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
