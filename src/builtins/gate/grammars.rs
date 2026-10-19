//! Programs with a grammar of their own, read by a judge rather than a
//! single [`Spec`]: shell state (`set`, `declare`, `unset`), tests (`test`,
//! `[`, `[[`), shell text (`trap`),
//! `dd`'s `key=value`, `find`'s expressions, `sed` scripts, `tar`'s old
//! style, and the tools with subcommands (`cargo`, `go`, `npm`, `ruff`,
//! `gh`, `rustup`, `python -m`).

use super::args::{parse, parse_until, Parsed, Spec};
use super::catalogue::{each, each_value};
use super::invocation::{identifier, safe_env, safe_variable};
use super::judge::{ok_if, Ask, Ctx, Verdict};
use super::parse::Word;
use super::specs::*;

/// The `make` targets and package scripts that run freely: building,
/// testing and checking, by their usual names. Others (`deploy`, `clean`,
/// `install`, `publish`) ask. (`make` with no target runs the Makefile's
/// default, and `npm test` the `test` script: both free.)
pub(super) const TARGETS: &[&str] = &[
    "all", "build", "test", "tests", "check", "lint", "typecheck", "fmt-check", "format-check",
    "ci",
];

/// A program or subcommand with no options of its own.
pub(super) const NO_OPTIONS: Spec = Spec::new(&[], &[]);

/// `set`: shell options (`-euo pipefail`), or the positional parameters.
pub(super) fn set(args: &[Word], _: &mut Ctx) -> Verdict {
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
pub(super) fn declare(args: &[Word], mut exports: bool) -> Verdict {
    let mut options = true;
    for w in args {
        ok_if(!w.opens_unknown)?;
        let t = w.text.as_str();
        if options && t == "--" {
            options = false;
        } else if options && (t.starts_with('-') || t.starts_with('+')) && t.len() > 1 {
            // Not `-n` (a name for another variable).
            // Not `-i` (the value is evaluated as arithmetic) nor `-n`.
            ok_if(t[1..].chars().all(|c| "xraAgpfFltu".contains(c)))?;
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
pub(super) fn trap(args: &[Word], ctx: &mut Ctx) -> Verdict {
    match args.first() {
        None => Ok(()),
        Some(w) if w.known && ["-l", "-p", "-", ""].contains(&w.text.as_str()) => Ok(()),
        Some(w) if w.known => ctx.run(&w.text),
        Some(_) => Err(Ask),
    }
}

/// `dd`: `if=`, `of=` and sizes; `of=` is written.
pub(super) fn dd(args: &[Word], ctx: &mut Ctx) -> Verdict {
    const READ: [&str; 10] = [
        "if", "bs", "ibs", "obs", "count", "skip", "seek", "status", "iflag", "oflag",
    ];
    each(args, |w| {
        ok_if(!w.opens_unknown)?;
        let (key, value) = w.text.split_once('=').ok_or(Ask)?;
        match key {
            "of" => ctx.write(&w.part(value)),
            "conv" => Ok(()),
            key => ok_if(READ.contains(&key)),
        }
    })
}

/// `find`: its starting points, then tests and actions; `-exec` only runs
/// a reader, `-fprint` and its kin write.
pub(super) fn find(args: &[Word], ctx: &mut Ctx) -> Verdict {
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
/// deletes, quits, moves through lines or the hold space (`p P d D q Q = l
/// n N g G h H x`, blocks), or substitutes (`s///` without the `w` or `e`
/// flags).
pub(super) fn sed_reads(script: &str) -> bool {
    let chars: Vec<char> = script.chars().collect();
    let mut i = 0;
    // Past the delimited text that starts at `i` (`/re/`, `|a|`); in a regex,
    // a bracket expression holds the delimiter as a character (`[/]`,
    // `[]/]`, `[^]/]`). False when it is not closed.
    let skip = |i: &mut usize, delim: char, regex: bool| -> bool {
        while *i < chars.len() && chars[*i] != delim {
            match chars[*i] {
                '\\' => *i += 1,
                '[' if regex => {
                    let mut at = *i + 1;
                    if chars.get(at) == Some(&'^') {
                        at += 1;
                    }
                    if chars.get(at) == Some(&']') {
                        at += 1;
                    }
                    match chars[at.min(chars.len())..].iter().position(|c| *c == ']') {
                        Some(close) => *i = at + close,
                        None => return false,
                    }
                }
                _ => {}
            }
            *i += 1;
        }
        *i += 1;
        *i <= chars.len()
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
                    if !skip(&mut i, '/', true) {
                        return false;
                    }
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
                if !skip(&mut i, delim, true) || !skip(&mut i, delim, false) {
                    return false;
                }
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
pub(super) fn tar(args: &[Word], ctx: &mut Ctx) -> Verdict {
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
pub(super) fn python(args: &[Word], ctx: &mut Ctx) -> Verdict {
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
        "unittest" => {
            // Test modules (`tests.test_x`) or files inside the project.
            ctx.in_project()?;
            let p = parse(&UNITTEST, rest).ok_or(Ask)?;
            each(&p.operands, |w| {
                match w.text.contains('/') || w.text.ends_with(".py") {
                    true => ctx.inside(w),
                    false => ok_if(w.known),
                }
            })
        }
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
pub(super) fn subcommand<'a>(
    global: &Spec,
    args: &'a [Word],
) -> Result<Option<(&'a str, &'a [Word])>, Ask> {
    let (_, at) = parse_until(global, args, 0).ok_or(Ask)?;
    match args.get(at) {
        None => Ok(None),
        Some(sub) if sub.known => Ok(Some((sub.text.as_str(), &args[at + 1..]))),
        Some(_) => Err(Ask),
    }
}

pub(super) fn cargo(args: &[Word], ctx: &mut Ctx) -> Verdict {
    let Some((sub, rest)) = subcommand(&CARGO, args)? else {
        return Ok(());
    };
    let reads = |spec: &Spec| ok_if(parse(spec, rest).is_some());
    match sub {
        "build" | "b" | "check" | "c" | "test" | "t" | "bench" | "run" | "r" | "doc" | "d"
        | "clippy" => {
            let p = parse(&CARGO_BUILD, rest).ok_or(Ask)?;
            // Its build folder inside the project.
            each_value(&p, &["--target-dir"], |w| ctx.inside(w))
        }
        // `cargo fmt --check`, or `-- --check`: what follows `--` goes to
        // rustfmt, read by its own spec (not `--print-config … PATH`).
        "fmt" => {
            let p = parse(&CARGO_FMT, rest).ok_or(Ask)?;
            let rustfmt = parse(&RUSTFMT, &p.operands).ok_or(Ask)?;
            ok_if(p.has("--check") || rustfmt.has("--check"))
        }
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

pub(super) fn go(args: &[Word], ctx: &mut Ctx) -> Verdict {
    let Some((sub, rest)) = subcommand(&NO_OPTIONS, args)? else {
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
            let (p, at) = parse_until(&GO_RUN, rest, 1).ok_or(Ask)?;
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

pub(super) fn npm(args: &[Word], _: &mut Ctx) -> Verdict {
    let Some((sub, rest)) = subcommand(&NO_OPTIONS, args)? else {
        return Err(Ask);
    };
    let p = parse(&NPM, rest).ok_or(Ask)?;
    match sub {
        "test" | "t" | "ls" | "list" | "view" | "info" | "outdated" | "why" => Ok(()),
        // Scripts by a name that builds or checks (not `deploy`, `clean`).
        "run" | "run-script" => ok_if(
            p.operand(0)
                .is_some_and(|w| w.known && TARGETS.contains(&w.text.as_str())),
        ),
        "audit" => ok_if(p.operands.is_empty()),
        _ => Err(Ask),
    }
}

pub(super) fn ruff(args: &[Word], _: &mut Ctx) -> Verdict {
    match subcommand(&NO_OPTIONS, args)? {
        Some(("check", rest)) => ok_if(parse(&RUFF_CHECK, rest).is_some()),
        Some(("format", rest)) => {
            ok_if(parse(&RUFF_FORMAT, rest).is_some_and(|p| p.has("--check") || p.has("--diff")))
        }
        _ => Err(Ask),
    }
}

/// GitHub's CLI, reading.
pub(super) fn gh(args: &[Word], _: &mut Ctx) -> Verdict {
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

pub(super) fn rustup(args: &[Word], _: &mut Ctx) -> Verdict {
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

/// `test` / `[`: free, except `-v NAME` with a name bash would evaluate
/// (`test -v 'a[$(rm f)]'`).
pub(super) fn test(args: &[Word], _: &mut Ctx) -> Verdict {
    names_after(args, &["-v"])
}

/// `[[ … ]]`: a whole test in this one command, ending in an unquoted
/// `]]`. bash reads a test across `&&`, `||`, `(`, redirections and
/// newlines, which this reading splits into commands; then this piece does
/// not end in `]]`, and the line asks rather than judge the rest as commands.
/// Its arithmetic comparisons evaluate both sides as arithmetic (a
/// variable's value too): literal numbers only, or `$#`, `$?`.
pub(super) fn double_bracket(args: &[Word], _: &mut Ctx) -> Verdict {
    const ARITHMETIC: [&str; 6] = ["-eq", "-ne", "-lt", "-le", "-gt", "-ge"];
    let number = |w: Option<&Word>| {
        w.is_some_and(|w| {
            let digits = w.text.strip_prefix('-').unwrap_or(&w.text);
            (w.known && !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
                || w.text == "$#"
                || w.text == "$?"
        })
    };
    ok_if(
        args.last()
            .is_some_and(|w| w.text == "]]" && w.known && !w.quoted()),
    )?;
    for (at, w) in args.iter().enumerate() {
        if w.known && ARITHMETIC.contains(&w.text.as_str()) {
            ok_if(at > 0 && number(args.get(at - 1)) && number(args.get(at + 1)))?;
        }
    }
    names_after(args, &["-v"])
}

/// `unset`: plain names only (`unset 'a[$(rm f)]'` evaluates the subscript).
pub(super) fn unset(args: &[Word], _: &mut Ctx) -> Verdict {
    each(args, |w| {
        ok_if(w.known && (["-v", "-f"].contains(&w.text.as_str()) || identifier(&w.text)))
    })
}

/// The word after each of `options` is a plain variable name.
pub(super) fn names_after(args: &[Word], options: &[&str]) -> Verdict {
    for (at, w) in args.iter().enumerate() {
        if w.known && options.contains(&w.text.as_str()) {
            ok_if(
                args.get(at + 1)
                    .is_some_and(|n| n.known && identifier(&n.text)),
            )?;
        }
    }
    Ok(())
}
