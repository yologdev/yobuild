//! Every program the gate lets run without asking, and what each may do.
//!
//! An entry is one of:
//! - a [`Reader`](Entry::Reader): no option of it writes a file or runs
//!   anything, so any arguments are fine (and it may run under `xargs` or
//!   `find -exec`, which add arguments the line does not show);
//! - a [`Spec`](Entry::Spec): the options it may be given (anything else
//!   asks), and a judge that turns what they say into effects — files
//!   written, appended to, deleted, made, scripts run — each judged by
//!   [`Ctx`];
//! - [`Raw`](Entry::Raw): a program with a grammar of its own (`find`, `dd`,
//!   `tar`'s old style, `git`, `cargo`'s subcommands), read by its judge.
//!
//! A program not here asks. Adding one is adding an entry: its options
//! and what it does with them.

use std::path::Path;

use super::args::{Parsed, Spec};
use super::git;
use super::grammars::*;
use super::invocation::safe_variable;
use super::judge::{ok_if, Ask, Ctx, Verdict};
use super::parse::Word;
use super::specs::*;

pub(super) type Judge = fn(&Parsed, &mut Ctx) -> Verdict;

pub(super) enum Entry {
    Reader,
    Spec(Spec, Judge),
    Raw(fn(&[Word], &mut Ctx) -> Verdict),
}

/// Programs none of whose options writes a file or runs anything.
const READERS: &[&str] = &[
    "ls", "cat", "tac", "head", "tail", "wc", "grep", "egrep", "fgrep", "zcat", "stat", "du", "df",
    "pwd", "echo", "true", "false", "test", "[", "[[", ":", "which", "type", "whereis",
    // Only `command -v`/`-V` (a lookup) reaches here; otherwise it is a wrapper.
    "command", "basename", "dirname", "realpath", "readlink", "printenv", "whoami", "id", "uname",
    "nproc", "uptime", "cut", "tr", "rev", "paste", "join", "comm", "diff", "cmp", "md5sum",
    "sha1sum", "sha256sum", "sha512sum", "shasum", "od", "hexdump", "nl", "column", "fold",
    "expand", "unexpand", "seq", "sleep", "jq", "wait", "jobs", "pyflakes", "ps",
];

/// Builds, tests and checks: they run the project's code, so only with the
/// line inside the project.
pub(super) const BUILDS: &[&str] = &[
    "cargo", "go", "npm", "pnpm", "yarn", "make", "gmake", "pytest", "ctest", "tsc", "eslint",
    "prettier", "vitest", "jest", "mypy", "ruff", "npx", "uv", "poetry",
];

/// Shell builtins that change only the shell's own state (or end it), and
/// loop headers (`for x in …`, `select`): their words are data.
const BUILTINS: &[&str] = &[
    "unset", "shift", "break", "continue", "return", "exit", "for", "select",
];

pub(super) fn entry(name: &str) -> Option<Entry> {
    use Entry::{Raw, Reader};
    if READERS.contains(&name) || BUILTINS.contains(&name) {
        return Some(Reader);
    }
    let spec = |spec: Spec, judge: Judge| Some(Entry::Spec(spec, judge));
    match name {
        // Shell state and shell text.
        "set" => Some(Raw(set)),
        "export" => Some(Raw(|args, _| declare(args, true))),
        "declare" | "typeset" | "local" | "readonly" => Some(Raw(|args, _| declare(args, false))),
        "read" => spec(READ, |p, _| {
            let mut names = p.operands.iter().chain(p.value("-a"));
            ok_if(names.all(|w| w.known && safe_variable(w)))
        }),
        "printf" => spec(PRINTF, |p, _| {
            ok_if(p.value("-v").is_none_or(|w| w.known && safe_variable(w)))
        }),
        "eval" => Some(Raw(|args, ctx| {
            ok_if(args.iter().all(|a| a.known))?;
            let text: Vec<&str> = args.iter().map(|a| a.text.as_str()).collect();
            ctx.run(&text.join(" "))
        })),
        "trap" => Some(Raw(trap)),
        "sh" | "bash" | "zsh" | "dash" | "ksh" => spec(SHELL, |p, ctx| {
            let first = p.operand(0).ok_or(Ask)?;
            match p.has("-c") {
                true if first.known => ctx.run(&first.text),
                true => Err(Ask),
                // Not reading a script from stdin (`bash -`, `| bash`).
                false => ctx.owners_file(first),
            }
        }),
        // Files: making, changing, copying, deleting.
        "mkdir" => spec(MKDIR, |p, ctx| each(&p.operands, |w| ctx.add_to(w))),
        "touch" => spec(TOUCH, |p, ctx| each(&p.operands, |w| ctx.touch(w))),
        "chmod" | "chown" | "chgrp" => spec(CHMOD, |p, ctx| {
            // The mode or owner, then the files.
            ok_if(p.operands.len() >= 2)?;
            each(&p.operands[1..], |w| ctx.change(w))
        }),
        "rm" => spec(RM, |p, ctx| each(&p.operands, |w| ctx.delete(w))),
        "rmdir" => spec(RMDIR, |p, ctx| each(&p.operands, |w| ctx.delete(w))),
        "unlink" => spec(Spec::new(&[], &[]), |p, ctx| {
            each(&p.operands, |w| ctx.delete(w))
        }),
        "cp" => spec(CP, |p, ctx| copy(p, ctx, false)),
        "mv" => spec(MV, |p, ctx| copy(p, ctx, true)),
        "install" => spec(INSTALL, |p, ctx| {
            match p.has("-d") || p.has("--directory") {
                true => each(&p.operands, |w| ctx.add_to(w)),
                false => copy(p, ctx, false),
            }
        }),
        "ln" => spec(LN, |p, ctx| {
            let [_, name] = p.operands.as_slice() else {
                return Err(Ask);
            };
            // A link inside a folder that is there is named from the target.
            ok_if(!ctx.path(name)?.is_dir())?;
            if p.has("-f") || p.has("--force") {
                ctx.write(name)?;
            }
            ctx.link(name)
        }),
        "tee" => spec(TEE, |p, ctx| {
            let append = p.has("-a") || p.has("--append");
            each(&p.operands, |w| match append {
                true => ctx.add_to(w),
                false => ctx.write(w),
            })
        }),
        "dd" => Some(Raw(dd)),
        "rsync" => spec(RSYNC, |p, ctx| {
            // Local paths only, into a folder not there yet or the agent's own.
            let [.., dest] = p.operands.as_slice() else {
                return Err(Ask);
            };
            ok_if(p.operands.len() >= 2 && p.operands.iter().all(local))?;
            ctx.new_or_own(dest)
        }),
        "curl" => spec(CURL, |p, ctx| {
            // `-w '%output{file}…'` writes a file too.
            ok_if(
                p.values(&["-w", "--write-out"])
                    .all(|w| w.known && !w.text.contains("%output")),
            )?;
            let outputs = [
                "-o", "--output", "-D", "--dump-header", "-c", "--cookie-jar", "--trace",
                "--trace-ascii", "--stderr",
            ];
            each_value(p, &outputs, |w| ctx.write(w))?;
            // A file it sends (`-d @FILE`, `-F f=@FILE`, `-F f=<FILE`): not
            // a protected one (`.env`).
            let sends = [
                "-d", "--data", "--data-binary", "--data-urlencode", "-F", "--form",
            ];
            each_value(p, &sends, |w| match w.text.find(['@', '<']) {
                Some(at) => {
                    let file = w.text[at + 1..].split(';').next().unwrap_or("");
                    ctx.send(&Word {
                        text: file.to_owned(),
                        ..w.clone()
                    })
                }
                None => Ok(()),
            })
        }),
        "wget" => spec(WGET, |p, ctx| {
            // Without `-O`, a new file named from the URL (never replacing one).
            each_value(
                p,
                &["-O", "--output-document", "-o", "--output-file"],
                |w| ctx.write(w),
            )?;
            each_value(p, &["-a", "--append-output"], |w| ctx.add_to(w))
        }),
        // Reading, with options that could write or run left out.
        "find" => Some(Raw(find)),
        "sed" => spec(SED, |p, _| {
            let scripts: Vec<&Word> = p.values(&["-e", "--expression"]).collect();
            let scripts = match scripts.is_empty() {
                true => vec![p.operand(0).ok_or(Ask)?],
                false => scripts,
            };
            ok_if(scripts.iter().all(|s| s.known && sed_reads(&s.text)))
        }),
        "awk" | "gawk" | "mawk" | "nawk" => spec(AWK, |p, _| {
            // Its program can write files and run commands; gawk's `@include`
            // and `@load` bring in more.
            let program = p.operand(0).filter(|w| w.known).ok_or(Ask)?;
            let writes_or_runs = ["system", ">", "|", "getline", "@"];
            ok_if(!writes_or_runs.iter().any(|t| program.text.contains(t)))
        }),
        "sort" => spec(SORT, free),
        "uniq" => spec(UNIQ, |p, ctx| match p.operands.as_slice() {
            [] | [_] => Ok(()),
            [_, out] => ctx.write(out),
            _ => Err(Ask),
        }),
        "rg" => spec(RG, free),
        "fd" => spec(FD, free),
        "tree" => spec(TREE, free),
        "file" => spec(FILE, free),
        "xxd" => spec(XXD, |p, ctx| match p.operands.as_slice() {
            [] | [_] => Ok(()),
            [_, out] => ctx.write(out),
            _ => Err(Ask),
        }),
        "base64" => spec(BASE64, |p, ctx| {
            each_value(p, &["-o", "--output"], |w| ctx.write(w))
        }),
        "date" => spec(DATE, |p, _| {
            // A format, not a date to set.
            ok_if(p.operands.iter().all(|w| w.text.starts_with('+')))
        }),
        "hostname" => spec(HOSTNAME, |p, _| ok_if(p.operands.is_empty())),
        // A new file with a random name (in the temp folder, or where `-p` or
        // the template says), never one that is there.
        "mktemp" => spec(MKTEMP, free),
        // Archives: listing, or creating one (never extracting or removing).
        "tar" => Some(Raw(tar)),
        "zip" => spec(ZIP, |p, ctx| ctx.write(p.operand(0).ok_or(Ask)?)),
        "unzip" => spec(UNZIP, |p, _| {
            ok_if(["-l", "-t", "-v", "-z"].iter().any(|o| p.has(o)))
        }),
        "gzip" | "gunzip" => spec(GZIP, |p, _| {
            ok_if(
                ["-c", "--stdout", "-l", "--list", "-t", "--test"]
                    .iter()
                    .any(|o| p.has(o)),
            )
        }),
        // Version control.
        "git" => Some(Raw(git::judge)),
        "gh" => Some(Raw(gh)),
        // Building, testing, checking: they run the project's code as it is.
        "cargo" => Some(Raw(cargo)),
        "go" => Some(Raw(go)),
        "npm" | "pnpm" | "yarn" => Some(Raw(npm)),
        "make" | "gmake" => {
            spec(MAKE, |p, _| {
                // Targets of the project's Makefile; `VAR=value` changes what it runs.
                let changes = ["install", "uninstall", "clean", "distclean"];
                ok_if(p.operands.iter().all(|w| {
                    w.known && !w.text.contains('=') && !changes.contains(&w.text.as_str())
                }))
            })
        }
        // Test files and folders inside the project (`tests/x.py::name`).
        "pytest" => spec(PYTEST, |p, ctx| {
            each(&p.operands, |w| {
                let path = w.text.split("::").next().unwrap_or("");
                ctx.inside(&Word {
                    text: path.to_owned(),
                    ..w.clone()
                })
            })
        }),
        "ctest" => spec(CTEST, free),
        "tsc" => spec(TSC, |p, _| ok_if(p.has("--noEmit"))),
        "eslint" => spec(ESLINT, |p, _| {
            // A formatter named by its path is code it loads.
            ok_if(
                p.values(&["-f", "--format"])
                    .all(|w| w.known && builtin_name(&w.text)),
            )
        }),
        "prettier" => spec(PRETTIER, |p, _| {
            ok_if(
                ["--check", "-c", "--list-different", "-l"]
                    .iter()
                    .any(|o| p.has(o)),
            )
        }),
        "vitest" => spec(VITEST, |p, _| {
            ok_if(
                p.values(&["--reporter"])
                    .all(|w| w.known && builtin_name(&w.text)),
            )
        }),
        "jest" => spec(JEST, free),
        "mypy" => spec(MYPY, free),
        "ruff" => Some(Raw(ruff)),
        "rustfmt" => spec(RUSTFMT, |p, _| ok_if(p.has("--check"))),
        "gofmt" => spec(GOFMT, free),
        // Runners: the command they run is judged as if it ran alone.
        "npx" => Some(Raw(|args, ctx| {
            let tool = args.first().filter(|w| w.known).ok_or(Ask)?;
            // Only checkers the project has (npx would fetch anything else).
            ok_if(["tsc", "eslint", "prettier", "vitest", "jest"].contains(&tool.text.as_str()))?;
            ctx.command(args, false)
        })),
        "uv" | "poetry" => Some(Raw(|args, ctx| match args {
            [run, rest @ ..] if run.known && run.text == "run" => {
                ok_if(rest.first().is_some_and(|w| !w.text.starts_with('-')))?;
                ctx.command(rest, false)
            }
            _ => Err(Ask),
        })),
        // Interpreters: only the owner's scripts.
        "python" | "python3" => Some(Raw(python)),
        "node" => spec(NODE, |p, ctx| {
            if p.has("--test") {
                // Test files inside the project (none: the ones it finds here).
                ctx.in_project()?;
                return each(&p.operands, |w| ctx.inside(w));
            }
            match p.operand(0) {
                Some(script) => ctx.owners_file(script),
                None => ok_if(p.has("--version") || p.has("-v")),
            }
        }),
        "deno" | "bun" | "ruby" | "perl" | "php" | "lua" | "Rscript" => {
            spec(INTERPRETER, |p, ctx| match p.operand(0) {
                Some(script) => ctx.owners_file(script),
                None => ok_if(!p.options.is_empty()),
            })
        }
        "rustc" | "gcc" | "clang" | "cc" => Some(Raw(|args, _| {
            ok_if(matches!(args, [v] if ["--version", "-V", "-v"].contains(&v.text.as_str())))
        })),
        "rustup" => Some(Raw(rustup)),
        _ => None,
    }
}

pub(super) fn free(_: &Parsed, _: &mut Ctx) -> Verdict {
    Ok(())
}

pub(super) fn each(words: &[Word], mut judge: impl FnMut(&Word) -> Verdict) -> Verdict {
    words.iter().try_for_each(&mut judge)
}

/// The values of these options, each judged (`-` is standard output).
pub(super) fn each_value(
    p: &Parsed,
    options: &[&str],
    mut judge: impl FnMut(&Word) -> Verdict,
) -> Verdict {
    p.values(options)
        .filter(|w| w.text != "-")
        .try_for_each(&mut judge)
}

/// A name, not a path to a module (`stylish`, not `./x.js`).
pub(super) fn builtin_name(text: &str) -> bool {
    !text.contains('/') && !text.starts_with('.')
}

/// A local path (not `host:path`).
pub(super) fn local(w: &Word) -> bool {
    match w.text.find(':') {
        Some(colon) => w.text[..colon].contains('/'),
        None => true,
    }
}

/// `cp`/`mv`/`install`: each source lands at the destination (inside it,
/// when it is a folder); moving also deletes the source.
fn copy(p: &Parsed, ctx: &mut Ctx, moves: bool) -> Verdict {
    let into = p.value("-t").or(p.value("--target-directory"));
    let (sources, dest) = match into {
        Some(dir) => (p.operands.as_slice(), dir),
        None => match p.operands.split_last() {
            Some((dest, sources)) => (sources, dest),
            None => return Err(Ask),
        },
    };
    ok_if(!sources.is_empty())?;
    if moves {
        each(sources, |s| ctx.delete(s))?;
    }
    let dest = ctx.path(dest)?;
    let recursive = ["-r", "-R", "-a", "--recursive", "--archive"]
        .iter()
        .any(|o| p.has(o));
    // A tree into a folder that is there: what lands where is not read
    // from the line (`cp -R dir/ .` copies dir's contents).
    ok_if(!(recursive && dest.is_dir() && !ctx.free(&dest)))?;
    if into.is_none() && !dest.is_dir() {
        return ctx.write_path(dest);
    }
    each(sources, |s| {
        ok_if(s.known)?;
        let name = Path::new(&s.text).file_name().ok_or(Ask)?;
        ctx.write_path(dest.join(name))
    })
}
