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

use super::args::{parse, Parsed, Spec};
use super::git;
use super::invocation::{safe_env, safe_variable};
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
const READERS: [&str; 62] = [
    "ls", "cat", "tac", "head", "tail", "wc", "grep", "egrep", "fgrep", "zcat", "stat", "du", "df",
    "pwd", "echo", "true", "false", "test", "[", "[[", ":", "which", "type", "whereis", "command",
    "basename", "dirname", "realpath", "readlink", "printenv", "whoami", "id", "uname", "nproc",
    "uptime", "cut", "tr", "rev", "paste", "join", "comm", "diff", "cmp", "md5sum", "sha1sum",
    "sha256sum", "sha512sum", "shasum", "od", "hexdump", "nl", "column", "fold", "expand",
    "unexpand", "seq", "sleep", "jq", "wait", "jobs", "pyflakes", "ps",
];

/// Shell builtins that change only the shell's own state (or end it).
const BUILTINS: [&str; 7] = [
    "unset", "shift", "break", "continue", "return", "exit", "for",
];

pub(super) fn entry(name: &str) -> Option<Entry> {
    use Entry::{Raw, Reader};
    if READERS.contains(&name) || BUILTINS.contains(&name) || name == "select" {
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
        "touch" => spec(TOUCH, |p, ctx| each(&p.operands, |w| ctx.add_to(w))),
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
            each_value(p, &outputs, |w| ctx.write(w))
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
        // A new file in the temporary folder, never one that is there.
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
        "pytest" => spec(PYTEST, free),
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
                // Test files the owner has (none: the ones it finds).
                return each(&p.operands, |w| ctx.owners_file(w));
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

fn free(_: &Parsed, _: &mut Ctx) -> Verdict {
    Ok(())
}

fn each(words: &[Word], mut judge: impl FnMut(&Word) -> Verdict) -> Verdict {
    words.iter().try_for_each(&mut judge)
}

/// The values of these options, each judged (`-` is standard output).
fn each_value(p: &Parsed, options: &[&str], mut judge: impl FnMut(&Word) -> Verdict) -> Verdict {
    p.values(options)
        .filter(|w| w.text != "-")
        .try_for_each(&mut judge)
}

/// A name, not a path to a module (`stylish`, not `./x.js`).
fn builtin_name(text: &str) -> bool {
    !text.contains('/') && !text.starts_with('.')
}

/// A local path (not `host:path`).
fn local(w: &Word) -> bool {
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

/// `set`: shell options (`-euo pipefail`), or the positional parameters.
fn set(args: &[Word], _: &mut Ctx) -> Verdict {
    const LETTERS: &str = "euxvECBfhTn";
    const NAMED: [&str; 10] = [
        "pipefail", "errexit", "nounset", "xtrace", "verbose", "noclobber", "noglob", "errtrace",
        "functrace", "posix",
    ];
    let mut words = args.iter();
    while let Some(w) = words.next() {
        ok_if(w.known)?;
        let t = w.text.as_str();
        if t == "--" || t == "-" || !(t.starts_with('-') || t.starts_with('+')) {
            // The positional parameters.
            return Ok(());
        }
        for letter in t[1..].chars() {
            match letter {
                'o' => ok_if(
                    words
                        .next()
                        .is_some_and(|v| NAMED.contains(&v.text.as_str())),
                )?,
                c => ok_if(LETTERS.contains(c))?,
            }
        }
    }
    Ok(())
}

/// `export`, `declare`, …: variables a program may be given (exported:
/// `export`, `-x`), or the script's own.
fn declare(args: &[Word], mut exports: bool) -> Verdict {
    let mut options = true;
    for w in args {
        ok_if(!w.opens_unknown)?;
        let t = w.text.as_str();
        if options && t == "--" {
            options = false;
        } else if options && (t.starts_with('-') || t.starts_with('+')) && t.len() > 1 {
            // Not `-n` (a name for another variable).
            ok_if(t[1..].chars().all(|c| "xraAigpfFltu".contains(c)))?;
            exports |= t.starts_with('-') && t.contains('x');
        } else {
            options = false;
            ok_if(match exports {
                true => safe_env(w),
                false => safe_variable(w),
            })?;
        }
    }
    Ok(())
}

/// `trap`: its first argument is shell text run later; read it now.
fn trap(args: &[Word], ctx: &mut Ctx) -> Verdict {
    match args.first() {
        None => Ok(()),
        Some(w) if w.known && ["-l", "-p", "-", ""].contains(&w.text.as_str()) => Ok(()),
        Some(w) if w.known => ctx.run(&w.text),
        Some(_) => Err(Ask),
    }
}

/// `dd`: `if=`, `of=` and sizes; `of=` is written.
fn dd(args: &[Word], ctx: &mut Ctx) -> Verdict {
    const READ: [&str; 10] = [
        "if", "bs", "ibs", "obs", "count", "skip", "seek", "status", "iflag", "oflag",
    ];
    each(args, |w| {
        ok_if(!w.opens_unknown)?;
        let (key, value) = w.text.split_once('=').ok_or(Ask)?;
        match key {
            "of" => ctx.write(&Word {
                text: value.to_owned(),
                ..w.clone()
            }),
            "conv" => Ok(()),
            key => ok_if(READ.contains(&key)),
        }
    })
}

/// `find`: its starting points, then tests and actions; `-exec` only runs
/// a reader, `-fprint` and its kin write.
fn find(args: &[Word], ctx: &mut Ctx) -> Verdict {
    const TESTS: [&str; 30] = [
        "-name", "-iname", "-path", "-ipath", "-wholename", "-iwholename", "-regex", "-iregex",
        "-type", "-xtype", "-maxdepth", "-mindepth", "-newer", "-mtime", "-mmin", "-atime",
        "-amin", "-ctime", "-cmin", "-size", "-perm", "-user", "-group", "-uid", "-gid", "-links",
        "-samefile", "-lname", "-regextype", "-printf",
    ];
    const ALONE: [&str; 27] = [
        "(", ")", "!", ",", "-not", "-a", "-and", "-o", "-or", "-print", "-print0", "-prune",
        "-ls", "-quit", "-true", "-false", "-empty", "-readable", "-writable", "-executable",
        "-nouser", "-nogroup", "-depth", "-xdev", "-mount", "-follow", "-daystart",
    ];
    let mut i = 0;
    while args
        .get(i)
        .is_some_and(|w| ["-L", "-H", "-P"].contains(&w.text.as_str()))
    {
        i += 1;
    }
    // Starting points.
    while let Some(w) = args.get(i) {
        ok_if(!w.opens_unknown)?;
        if w.text.starts_with('-') || w.text == "(" || w.text == "!" {
            break;
        }
        i += 1;
    }
    while let Some(w) = args.get(i) {
        ok_if(w.known)?;
        let t = w.text.as_str();
        let value = args.get(i + 1);
        i += match t {
            t if ALONE.contains(&t) => 1,
            t if TESTS.contains(&t) || (t.starts_with("-newer") && t.len() == 8) => {
                value.ok_or(Ask)?;
                2
            }
            "-fprint" | "-fprint0" | "-fls" => {
                ctx.write(value.ok_or(Ask)?)?;
                2
            }
            "-fprintf" => {
                ctx.write(value.ok_or(Ask)?)?;
                args.get(i + 2).ok_or(Ask)?;
                3
            }
            "-exec" | "-execdir" | "-ok" | "-okdir" => {
                let end = args[i + 1..]
                    .iter()
                    .position(|w| w.known && (w.text == ";" || w.text == "+"))
                    .ok_or(Ask)?;
                ctx.command(&args[i + 1..i + 1 + end], true)?;
                end + 2
            }
            _ => return Err(Ask),
        };
    }
    Ok(())
}

/// Whether a `sed` script only reads: every command one that prints,
/// deletes, quits or substitutes (`s///` without the `w` or `e` flags).
fn sed_reads(script: &str) -> bool {
    let chars: Vec<char> = script.chars().collect();
    let mut i = 0;
    // Past the delimited text that starts at `i` (`/re/`, `|a|`).
    let skip = |i: &mut usize, delim: char| {
        while *i < chars.len() && chars[*i] != delim {
            if chars[*i] == '\\' {
                *i += 1;
            }
            *i += 1;
        }
        *i += 1;
    };
    while i < chars.len() {
        while i < chars.len() && (chars[i].is_whitespace() || chars[i] == ';') {
            i += 1;
        }
        // An address: line numbers, `$`, `/re/`, ranges.
        loop {
            match chars.get(i) {
                Some(c) if c.is_ascii_digit() || matches!(c, '$' | ',' | '!' | '~' | '+') => i += 1,
                Some('/') => {
                    i += 1;
                    skip(&mut i, '/');
                }
                _ => break,
            }
        }
        match chars.get(i) {
            None => break,
            Some(
                'p' | 'P' | 'd' | 'D' | 'q' | 'Q' | '=' | 'n' | 'N' | 'g' | 'G' | 'h' | 'H' | 'x'
                | 'l' | '{' | '}',
            ) => i += 1,
            Some('s') => {
                let Some(&delim) = chars.get(i + 1) else {
                    return false;
                };
                i += 2;
                skip(&mut i, delim);
                skip(&mut i, delim);
                while chars.get(i).is_some_and(|c| {
                    matches!(c, 'g' | 'i' | 'I' | 'p' | 'm' | 'M') || c.is_ascii_digit()
                }) {
                    i += 1;
                }
            }
            // `w`, `W`, `e`, `r`, `R`, `a`/`i`/`c` text, `y`, labels: not read here.
            Some(_) => return false,
        }
    }
    true
}

/// `tar`: listing (`-t`) or creating (`-c`) an archive, whose file is
/// written. The old style (`tar czf FILE …`) is read as `-c -z -f FILE`.
fn tar(args: &[Word], ctx: &mut Ctx) -> Verdict {
    let words: Vec<Word> = match args.first() {
        Some(first) if !first.text.starts_with('-') && !first.opens_unknown => {
            ok_if(first.known)?;
            let mut rest = args[1..].iter();
            let mut words = Vec::new();
            for letter in first.text.chars() {
                let option = format!("-{letter}");
                let valued = TAR.valued.contains(&option.as_str());
                words.push(Word::literal(option));
                if valued {
                    words.push(rest.next().ok_or(Ask)?.clone());
                }
            }
            words.extend(rest.cloned());
            words
        }
        _ => args.to_vec(),
    };
    let p = parse(&TAR, &words).ok_or(Ask)?;
    let creates = p.has("-c") || p.has("--create");
    let lists = p.has("-t") || p.has("--list");
    ok_if(creates != lists)?;
    match (creates, p.value("-f").or(p.value("--file"))) {
        (false, _) => Ok(()),
        (true, Some(file)) if file.text == "-" => Ok(()),
        (true, Some(file)) => ctx.write(file),
        // The default archive may be a device.
        (true, None) => Err(Ask),
    }
}

/// `python`: the owner's script, or a module that tests or checks.
fn python(args: &[Word], ctx: &mut Ctx) -> Verdict {
    const FLAGS: [&str; 8] = ["-u", "-B", "-O", "-E", "-s", "-I", "-q", "-b"];
    let mut i = 0;
    loop {
        let w = args.get(i).ok_or(Ask)?;
        ok_if(!w.opens_unknown)?;
        match w.text.as_str() {
            t if FLAGS.contains(&t) => i += 1,
            "-W" | "-X" => i += 2,
            "--version" | "-V" | "--help" | "-h" => return Ok(()),
            "-m" => break,
            t if t.starts_with('-') => return Err(Ask),
            _ => return ctx.owners_file(w),
        }
    }
    let module = args.get(i + 1).filter(|w| w.known).ok_or(Ask)?;
    let rest = &args[i + 2..];
    match module.text.as_str() {
        "pytest" | "mypy" | "pyflakes" => ctx.command(&args[i + 1..], false),
        "unittest" => ok_if(parse(&UNITTEST, rest).is_some()),
        "pip" => match rest {
            [sub, rest @ ..]
                if ["list", "show", "freeze", "check"].contains(&sub.text.as_str()) =>
            {
                ok_if(parse(&PIP, rest).is_some())
            }
            _ => Err(Ask),
        },
        _ => Err(Ask),
    }
}

/// A subcommand and its arguments, read past the program's own options.
fn subcommand<'a>(global: &Spec, args: &'a [Word]) -> Result<Option<(&'a str, &'a [Word])>, Ask> {
    let (_, at) = super::args::parse_until(global, args, 0).ok_or(Ask)?;
    match args.get(at) {
        None => Ok(None),
        Some(sub) if sub.known => Ok(Some((sub.text.as_str(), &args[at + 1..]))),
        Some(_) => Err(Ask),
    }
}

fn cargo(args: &[Word], _: &mut Ctx) -> Verdict {
    let Some((sub, rest)) = subcommand(&CARGO, args)? else {
        return Ok(());
    };
    let reads = |spec: &Spec| ok_if(parse(spec, rest).is_some());
    match sub {
        "build" | "b" | "check" | "c" | "test" | "t" | "bench" | "run" | "r" | "doc" | "d"
        | "clippy" => reads(&CARGO_BUILD),
        "fmt" => ok_if(parse(&CARGO_FMT, rest).is_some_and(|p| p.has("--check"))),
        "tree" | "metadata" | "version" | "search" | "locate-project" | "pkgid"
        | "verify-project" => reads(&CARGO_READ),
        "nextest" => match rest {
            [run, rest @ ..] if run.text == "run" || run.text == "list" => {
                ok_if(parse(&CARGO_BUILD, rest).is_some())
            }
            _ => Err(Ask),
        },
        _ => Err(Ask),
    }
}

fn go(args: &[Word], ctx: &mut Ctx) -> Verdict {
    let Some((sub, rest)) = subcommand(&Spec::new(&[], &[]), args)? else {
        return Ok(());
    };
    // Packages here (`.`, `./…`), or the owner's `.go` files: not code
    // fetched from elsewhere.
    let here = |p: &Parsed, ctx: &mut Ctx| {
        each(&p.operands, |w| match w.text.ends_with(".go") {
            true => ctx.owners_file(w),
            false => ok_if(w.known && (w.text == "." || w.text.starts_with("./"))),
        })
    };
    let parsed = |spec: &Spec| parse(spec, rest).ok_or(Ask);
    match sub {
        "build" => {
            let p = parsed(&GO_BUILD)?;
            each_value(&p, &["-o"], |w| ctx.write(w))?;
            here(&p, ctx)
        }
        "test" => {
            let p = parsed(&GO_TEST)?;
            let outputs = ["-o", "-coverprofile", "-cpuprofile", "-memprofile"];
            each_value(&p, &outputs, |w| ctx.write(w))?;
            here(&p, ctx)
        }
        "vet" => here(&parsed(&GO_VET)?, ctx),
        "run" => {
            // The package (or files), then the program's own arguments;
            // `.go` files after the first are compiled too.
            let (p, at) = super::args::parse_until(&GO_RUN, rest, 1).ok_or(Ask)?;
            let first = p.operand(0).ok_or(Ask)?;
            if first.text.ends_with(".go") {
                let more = rest[at..].iter().take_while(|w| w.text.ends_with(".go"));
                each(&more.cloned().collect::<Vec<_>>(), |w| ctx.owners_file(w))?;
            }
            here(&p, ctx)
        }
        "list" => parsed(&GO_LIST).map(drop),
        "env" => parsed(&GO_ENV).map(drop),
        "version" => parsed(&GO_VERSION).map(drop),
        "mod" => match rest {
            [what, rest @ ..]
                if ["graph", "why", "verify", "download"].contains(&what.text.as_str()) =>
            {
                ok_if(parse(&GO_MOD, rest).is_some())
            }
            _ => Err(Ask),
        },
        _ => Err(Ask),
    }
}

fn npm(args: &[Word], _: &mut Ctx) -> Verdict {
    let Some((sub, rest)) = subcommand(&Spec::new(&[], &[]), args)? else {
        return Err(Ask);
    };
    let p = parse(&NPM, rest).ok_or(Ask)?;
    match sub {
        "test" | "t" | "run" | "run-script" | "ls" | "list" | "view" | "info" | "outdated"
        | "why" => Ok(()),
        "audit" => ok_if(p.operands.is_empty()),
        _ => Err(Ask),
    }
}

fn ruff(args: &[Word], _: &mut Ctx) -> Verdict {
    match subcommand(&Spec::new(&[], &[]), args)? {
        Some(("check", rest)) => ok_if(parse(&RUFF_CHECK, rest).is_some()),
        Some(("format", rest)) => {
            ok_if(parse(&RUFF_FORMAT, rest).is_some_and(|p| p.has("--check") || p.has("--diff")))
        }
        _ => Err(Ask),
    }
}

/// GitHub's CLI, reading.
fn gh(args: &[Word], _: &mut Ctx) -> Verdict {
    let [group, action, rest @ ..] = args else {
        return Err(Ask);
    };
    let groups = ["pr", "issue", "run", "release", "repo", "workflow"];
    let actions = ["view", "list", "diff", "checks", "status"];
    ok_if(
        group.known
            && action.known
            && groups.contains(&group.text.as_str())
            && actions.contains(&action.text.as_str())
            && parse(&GH, rest).is_some(),
    )
}

fn rustup(args: &[Word], _: &mut Ctx) -> Verdict {
    let texts: Vec<&str> = args.iter().map(|w| w.text.as_str()).collect();
    ok_if(
        args.iter().all(|w| w.known)
            && matches!(
                texts.as_slice(),
                ["--version" | "-V"]
                    | ["show"]
                    | ["show", "active-toolchain"]
                    | ["toolchain", "list"]
                    | ["toolchain", "list", "-v"]
                    | ["target" | "component", "list"]
                    | ["target" | "component", "list", "--installed"]
            ),
    )
}
