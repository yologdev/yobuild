//! A command's words → what runs: shell keywords skipped, assignments
//! checked, wrappers (`env`, `timeout`, `xargs`, …) read through their own
//! [`Spec`]s, and the program resolved. Anything it cannot read is
//! [`Unread`]: the gate asks.

use super::args::{parse_until, Spec};
use super::parse::Word;
use super::paths::system_binary;

/// A program and its arguments.
#[derive(Debug)]
pub(super) struct Invocation<'a> {
    /// Its name (for a system binary named by path, the file's name).
    pub name: String,
    pub args: &'a [Word],
    /// Run by `xargs`: it also gets arguments the command line does not show.
    pub via_xargs: bool,
}

/// A command the gate cannot read.
#[derive(Debug)]
pub(super) struct Unread;

/// Keywords before a command: the command after them is what runs.
const KEYWORDS: [&str; 9] = [
    "if", "then", "else", "elif", "while", "until", "do", "!", "{",
];
/// Keywords that close a block: nothing runs.
const CLOSERS: [&str; 5] = ["fi", "done", "esac", "}", "]]"];

/// Environment variables a command may be given (`NAME=value cmd`,
/// `export NAME=value`): they change how programs look, not what runs.
const SAFE_ENV: [&str; 22] = [
    "RUST_LOG", "RUST_BACKTRACE", "RUST_TEST_THREADS", "RUST_MIN_STACK", "CARGO_TERM_COLOR",
    "CARGO_INCREMENTAL", "NO_COLOR", "FORCE_COLOR", "CLICOLOR", "CI", "TERM", "LANG", "LC_ALL",
    "TZ", "NODE_ENV", "PYTHONUNBUFFERED", "PYTHONDONTWRITEBYTECODE", "PYTHONHASHSEED", "DEBUG",
    "VERBOSE", "COLUMNS", "LINES",
];

/// `GIT_*` settings that change no program and no repository.
const SAFE_GIT: [&str; 7] = [
    "GIT_PAGER=cat", "GIT_PAGER=", "GIT_TERMINAL_PROMPT=0", "GIT_EDITOR=true", "GIT_EDITOR=:",
    "GIT_TRACE=0", "GIT_TRACE=1",
];

/// Whether a program may be given this variable.
pub(super) fn safe_env(assignment: &Word) -> bool {
    let (name, value) = assignment
        .text
        .split_once('=')
        .unwrap_or((&assignment.text, ""));
    if name.starts_with("GIT_") {
        return assignment.known && SAFE_GIT.contains(&assignment.text.as_str());
    }
    if name == "RUSTFLAGS" || name == "RUSTDOCFLAGS" {
        return assignment.known && lint_flags(value);
    }
    SAFE_ENV.contains(&name) || name.starts_with("LC_")
}

/// Rust flags that only set lint levels (`-Dwarnings`, `-D warnings`), not
/// `-C linker=…` or a plugin.
fn lint_flags(value: &str) -> bool {
    let mut words = value.split_whitespace();
    while let Some(word) = words.next() {
        let lint = match word {
            "-D" | "-W" | "-A" | "-F" => words.next().unwrap_or(""),
            word => match word.strip_prefix(['-']) {
                Some(rest) if rest.starts_with(['D', 'W', 'A', 'F']) => &rest[1..],
                _ => return false,
            },
        };
        let name = |c: char| c.is_ascii_lowercase() || matches!(c, '_' | '-' | ':');
        if lint.is_empty() || !lint.chars().all(name) {
            return false;
        }
    }
    true
}

/// A plain shell variable name (`[A-Za-z_][A-Za-z0-9_]*`): not `a[$(…)]`,
/// whose subscript bash evaluates as arithmetic.
pub(super) fn identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether a shell variable may be set (`x=1` alone, `read x`): a lowercase
/// name is the script's own; an uppercase one may be one the shell or a
/// program reads (`PATH`, `IFS`, `BASH_ENV`), so it must be a safe one.
pub(super) fn safe_variable(assignment: &Word) -> bool {
    let name = assignment.text.split('=').next().unwrap_or("");
    identifier(name) && (name.chars().any(|c| c.is_ascii_lowercase()) || safe_env(assignment))
}

/// How the word after a keyword or wrapper is read.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reads {
    /// As bash reads a command: assignments, keywords, then a program.
    Shell,
    /// After `env`: assignments, then a program.
    Env,
    /// After another wrapper: a program, whatever it looks like.
    Program,
}

/// A wrapper: a program that runs the command after its own arguments.
struct Wrapper {
    spec: Spec,
    /// Its own operands before the command (`timeout DURATION cmd`).
    operands: usize,
}

fn wrapper(name: &str) -> Option<Wrapper> {
    let w = |flags, valued, operands| {
        Some(Wrapper {
            spec: Spec::new(flags, valued),
            operands,
        })
    };
    match name {
        "nohup" | "builtin" => w(&[], &[], 0),
        "command" => w(&["-p"], &[], 0),
        "exec" => w(&["-c", "-l"], &["-a"], 0),
        "time" => w(&["-p"], &[], 0),
        "env" => w(
            &["-i", "-0", "--ignore-environment", "--null"],
            &["-u", "--unset"],
            0,
        ),
        "nice" => Some(Wrapper {
            spec: Spec::new(&[], &["-n", "--adjustment"]).numbers(),
            operands: 0,
        }),
        "ionice" => w(&["-t"], &["-c", "-n", "--class", "--classdata"], 0),
        "timeout" => w(
            &["--preserve-status", "--foreground", "-v", "--verbose"],
            &["-s", "-k", "--signal", "--kill-after"],
            1,
        ),
        "stdbuf" => w(
            &[],
            &["-i", "-o", "-e", "--input", "--output", "--error"],
            0,
        ),
        "xargs" => w(
            &[
                "-0", "-r", "-t", "-x", "--null", "--no-run-if-empty", "--verbose",
            ],
            &[
                "-I", "-n", "-L", "-P", "-d", "-E", "-s", "-a", "--max-args", "--max-lines",
                "--max-procs", "--delimiter", "--eof", "--arg-file", "--replace",
            ],
            0,
        ),
        _ => None,
    }
}

/// What `words` runs; `Ok(None)` when nothing does (assignments alone, a
/// keyword closing a block).
pub(super) fn invocation(words: &[Word]) -> Result<Option<Invocation<'_>>, Unread> {
    let mut i = 0;
    let mut via_xargs = false;
    let mut assignments: Vec<&Word> = Vec::new();
    // What the next word may be besides a program: bash reads assignments
    // and keywords; `env` takes `NAME=value` itself; any other wrapper runs
    // its next word as a program (`nohup x=./evil`, `timeout 5 fi`).
    let mut reads = Reads::Shell;
    loop {
        while let Some(w) = words
            .get(i)
            .filter(|w| reads != Reads::Program && w.assigns())
        {
            assignments.push(w);
            i += 1;
        }
        let Some(word) = words.get(i) else {
            // Assignments alone set shell variables.
            return match assignments.iter().all(|a| safe_variable(a)) {
                true => Ok(None),
                false => Err(Unread),
            };
        };
        // For a command, every assignment enters its environment.
        if !assignments.iter().all(|a| safe_env(a)) {
            return Err(Unread);
        }
        let text = word.text.strip_prefix('\\').unwrap_or(&word.text);
        // Quoted or escaped, a keyword is a program's name (`'fi'`).
        let keyword = reads == Reads::Shell && word.known && !word.quoted();
        if keyword && KEYWORDS.contains(&text) {
            i += 1;
            continue;
        }
        if keyword && CLOSERS.contains(&text) {
            return Ok(None);
        }
        // The program's name is literal (a lone `[`, `[[`, `{` is: parse.rs).
        if !word.known {
            return Err(Unread);
        }
        let name = if text.contains('/') {
            if !system_binary(text) {
                return Err(Unread);
            }
            text.trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or("")
                .to_owned()
        } else {
            text.to_owned()
        };
        if name.is_empty() {
            return Err(Unread);
        }
        // `command -v x` only looks `x` up.
        if name == "command"
            && matches!(words.get(i + 1), Some(w) if w.text == "-v" || w.text == "-V")
        {
            return Ok(Some(Invocation {
                name,
                args: &words[i + 1..],
                via_xargs,
            }));
        }
        let Some(wrapper) = wrapper(&name) else {
            return Ok(Some(Invocation {
                name,
                args: &words[i + 1..],
                via_xargs,
            }));
        };
        let (_, at) =
            parse_until(&wrapper.spec, &words[i + 1..], wrapper.operands).ok_or(Unread)?;
        via_xargs |= name == "xargs";
        reads = match name.as_str() {
            // `time` is bash's own word, before a pipeline.
            "time" => Reads::Shell,
            "env" => Reads::Env,
            _ => Reads::Program,
        };
        i += 1 + at;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtins::gate::parse::{parse, Item};

    fn read(line: &str) -> Result<Option<(String, usize, bool)>, Unread> {
        let Some(Item::Command(c)) = parse(line).unwrap().into_iter().next() else {
            panic!("{line}")
        };
        invocation(&c.words).map(|i| i.map(|i| (i.name, i.args.len(), i.via_xargs)))
    }

    #[test]
    fn wrappers_and_keywords_are_seen_through() {
        for (line, name) in [
            ("env -i RUST_LOG=1 rm x", "rm"),
            ("timeout -s KILL 5 rm x", "rm"),
            ("nice nohup /bin/rm x", "rm"),
            ("\\rm x", "rm"),
            ("if rm x", "rm"),
            ("! time -p rm x", "rm"),
            ("command -p rm x", "rm"),
            ("exec -la name rm x", "rm"),
            ("stdbuf --output L rm x", "rm"),
        ] {
            let read = read(line).ok().flatten().map(|r| r.0);
            assert_eq!(read.as_deref(), Some(name), "{line}");
        }
        assert!(read("xargs -I {} rm {}").unwrap().unwrap().2, "via xargs");
    }

    #[test]
    fn what_it_cannot_read_is_unread() {
        for line in [
            "./cat x", "tmp/ls", "$X rm", "env -C sub rm x", "env -S 'rm x'", "xargs -J % rm %",
            "PATH=.:$PATH ls", "LD_PRELOAD=x.so ls", "BASH_ENV=x bash y", "GIT_DIR=/x git status",
            "PATH=/tmp", "IFS=x", "nohup x=./evil", "exec x=./evil",
        ] {
            assert!(read(line).is_err(), "{line}");
        }
    }

    #[test]
    fn harmless_settings_and_variables_are_read() {
        assert_eq!(
            read("RUST_LOG=debug cargo test").unwrap().unwrap().0,
            "cargo"
        );
        assert_eq!(read("GIT_PAGER=cat git log").unwrap().unwrap().0, "git");
        assert_eq!(read("/bin/ls -la").unwrap().unwrap().0, "ls");
        assert!(read("f=foo").unwrap().is_none());
        assert!(read("x=$(ls)").unwrap().is_none());
        assert!(read("fi").unwrap().is_none());
        assert_eq!(read("env RUST_LOG=1 ls").unwrap().unwrap().0, "ls");
        assert_eq!(read("time RUST_LOG=1 ls").unwrap().unwrap().0, "ls");
        assert_eq!(read("! time if ls").unwrap().unwrap().0, "ls");
        // A wrapper but `env` runs its next word as a program: not a keyword,
        // not an assignment.
        for (line, name) in [
            ("timeout 5 fi", "fi"),
            ("nice x=1", "x=1"),
            ("env fi", "fi"),
            ("nohup {", "{"),
        ] {
            assert_eq!(read(line).unwrap().unwrap().0, name, "{line}");
        }
        assert_eq!(read("command -v rm").unwrap().unwrap().0, "command");
    }
}
