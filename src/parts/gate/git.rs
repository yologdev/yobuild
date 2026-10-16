//! `git`: the subcommands that read, or add, commit and share without
//! discarding the working tree or a ref (`reset --soft` and `commit
//! --amend` move history; the reflog keeps it), each with the options that
//! keep it so. Anything
//! else — a subcommand or option not listed, `-c`, `--git-dir`, an alias —
//! asks.

use super::args::{parse, parse_until, Parsed, Spec};
use super::catalogue::Judge;
use super::judge::{ok_if, Ask, Ctx, Verdict};
use super::parse::Word;

pub(super) fn judge(args: &[Word], ctx: &mut Ctx) -> Verdict {
    let (global, at) = parse_until(&GLOBAL, args, 0).ok_or(Ask)?;
    // `git`, `git --version`.
    let Some(sub) = args.get(at) else {
        return Ok(());
    };
    ok_if(sub.known)?;
    let rest = &args[at + 1..];
    let (spec, judge) = subcommand(&sub.text, rest).ok_or(Ask)?;
    // In another folder (`-C`): the paths it names are not judged here.
    ok_if(!(global.has("-C") && matches!(sub.text.as_str(), "clone" | "init")))?;
    let parsed = parse(&spec, rest).ok_or(Ask)?;
    judge(&parsed, ctx)
}

fn free(_: &Parsed, _: &mut Ctx) -> Verdict {
    Ok(())
}

fn subcommand(name: &str, rest: &[Word]) -> Option<(Spec, Judge)> {
    // Its first operand (`git remote -v show`: `show`).
    let first = rest
        .iter()
        .map(|w| w.text.as_str())
        .find(|t| !t.starts_with('-'));
    let pair = |spec: Spec, judge: Judge| Some((spec, judge));
    match name {
        "version" | "help" => pair(Spec::new(&[], &[]), free),
        // Reading.
        "log" | "show" | "diff" | "whatchanged" | "shortlog" | "range-diff" | "cherry"
        | "show-branch" | "blame" | "rev-list" => pair(REVIEW, free),
        "status" => pair(STATUS, free),
        "grep" => pair(GREP, free),
        "ls-files" => pair(LS_FILES, free),
        "ls-tree" => pair(LS_TREE, free),
        "rev-parse" => pair(REV_PARSE, free),
        "describe" => pair(DESCRIBE, free),
        "cat-file" => pair(Spec::new(&["-t", "-s", "-p", "-e"], &[]), free),
        "merge-base" => pair(
            Spec::new(
                &[
                    "--all", "--is-ancestor", "--fork-point", "--octopus", "--independent",
                ],
                &[],
            ),
            free,
        ),
        "name-rev" => pair(
            Spec::new(
                &["--name-only", "--tags", "--all"],
                &["--refs", "--exclude"],
            ),
            free,
        ),
        "for-each-ref" => pair(FOR_EACH_REF, free),
        "show-ref" => pair(SHOW_REF, free),
        "count-objects" => pair(
            Spec::new(&["-v", "-H", "--verbose", "--human-readable"], &[]),
            free,
        ),
        "var" => pair(Spec::new(&["-l"], &[]), free),
        "check-ignore" => pair(
            Spec::new(
                &[
                    "-q", "-v", "-n", "-z", "--quiet", "--verbose", "--non-matching", "--no-index",
                ],
                &[],
            ),
            free,
        ),
        "check-attr" => pair(Spec::new(&["-a", "--all", "--cached", "-z"], &[]), free),
        "reflog" => match first {
            None | Some("show" | "list") => pair(REVIEW, free),
            _ => None,
        },
        "remote" => match first {
            None | Some("show" | "get-url") => pair(Spec::new(&["-v", "--verbose"], &[]), free),
            _ => None,
        },
        "worktree" => match first {
            Some("list") => pair(Spec::new(&["--porcelain", "-v", "-z"], &[]), free),
            _ => None,
        },
        "stash" => match first {
            Some("list" | "show") => pair(REVIEW, free),
            // Putting changes aside keeps them (not untracked files: `-u`).
            None | Some("push" | "save") => pair(STASH_PUSH, free),
            _ => None,
        },
        "branch" => pair(BRANCH, free),
        "tag" => pair(TAG, free),
        "config" => pair(CONFIG, |p, _| {
            // Reading one key (`user.name`) or with a reading option.
            let reads = [
                "--get", "--get-all", "--get-regexp", "--get-urlmatch", "--list", "-l",
            ];
            ok_if(match p.operands.as_slice() {
                [] => true,
                [what, ..] if what.text == "get" || what.text == "list" => true,
                _ if reads.iter().any(|o| p.has(o)) => true,
                [key] => key.text.contains('.'),
                _ => false,
            })
        }),
        // Adding and committing: nothing is discarded.
        "add" => pair(ADD, free),
        "commit" => pair(COMMIT, free),
        "init" => pair(INIT, |p, ctx| match p.operands.as_slice() {
            [] => Ok(()),
            // A new repository in a folder not there yet.
            [dir] => ctx.new_or_own(dir),
            _ => Err(Ask),
        }),
        "clone" => pair(CLONE, |p, ctx| match p.operands.as_slice() {
            [_] => Ok(()),
            // Into a folder not there yet.
            [_, dir] => ctx.new_or_own(dir),
            _ => Err(Ask),
        }),
        "fetch" => pair(FETCH, fetches),
        "pull" => pair(PULL, fetches),
        "push" => pair(PUSH, |p, _| {
            // Not forced (`+…`), not deleting (`:branch`).
            refspecs(p, |r| !r.starts_with(['+', ':']))
        }),
        // A new branch; unstaging; moving HEAD back (`--soft`, `--mixed`); a dry run.
        "checkout" => pair(CHECKOUT, |p, _| {
            ok_if((p.has("-b") || p.has("--orphan")) && p.operands.len() <= 1)
        }),
        // A new branch, or another one: git refuses to switch over local
        // changes it would lose (`--discard-changes`, `-f` are not listed).
        // It does replace ignored files the other branch tracks, and runs
        // the `post-checkout` hook (the owner's, in `.git`).
        "switch" => pair(SWITCH, |p, _| {
            let creates = ["-c", "--create", "--orphan"].iter().any(|o| p.has(o));
            ok_if(p.operands.len() <= 1 && (creates || p.operands.len() == 1))
        }),
        "restore" => pair(RESTORE, |p, _| ok_if(p.has("--staged") || p.has("-S"))),
        "rm" => pair(RM, |p, _| ok_if(p.has("--cached"))),
        "reset" => pair(RESET, free),
        "clean" => pair(CLEAN, |p, _| ok_if(p.has("-n") || p.has("--dry-run"))),
        _ => None,
    }
}

/// Into remote-tracking refs only: a refspec with a destination
/// (`main:main`) or forced (`+…`) can overwrite a branch.
fn fetches(p: &Parsed, _: &mut Ctx) -> Verdict {
    refspecs(p, |r| !r.contains([':', '+']))
}

/// The remote and refspecs are literal, and every refspec after the remote
/// is `fine`.
fn refspecs(p: &Parsed, fine: impl Fn(&str) -> bool) -> Verdict {
    ok_if(p.operands.iter().all(|w| w.known))?;
    ok_if(p.operands.iter().skip(1).all(|r| fine(&r.text)))
}

const GLOBAL: Spec = Spec::new(
    &[
        "--no-pager", "-P", "--no-optional-locks", "--literal-pathspecs", "--version", "--help",
    ],
    &["-C"],
);

/// Options of the subcommands that show history and changes. None writes
/// a file (`--output`) or runs a program (`--ext-diff`, `-O`).
const REVIEW: Spec = Spec::new(
    &[
        "-p", "-u", "--patch", "-s", "--no-patch", "--stat", "--shortstat", "--numstat",
        "--name-only", "--name-status", "--summary", "--raw", "--oneline", "--graph", "--all",
        "--branches", "--tags", "--remotes", "--decorate", "--no-decorate", "--cached", "--staged",
        "--color", "--no-color", "--word-diff", "--color-words", "--reverse", "--first-parent",
        "--no-merges", "--merges", "--follow", "-w", "--ignore-all-space", "-b",
        "--ignore-space-change", "--ignore-blank-lines", "--abbrev-commit", "--no-abbrev-commit",
        "--date-order", "--topo-order", "-q", "--quiet", "--exit-code", "--check",
        "--full-history", "--simplify-by-decoration", "--source", "--boundary", "--left-right",
        "--cherry-pick", "--cherry-mark", "--use-mailmap", "--show-signature", "--relative", "-M",
        "-C", "--find-renames", "--find-copies", "--minimal", "--patience", "--histogram",
        "--no-renames", "--full-index", "--binary", "--no-ext-diff", "--no-textconv", "-R",
        "--no-index", "--merge-base", "-z", "-c", "--cc", "-m", "--no-walk", "-g",
        "--walk-reflogs", "--count", "--objects", "-i", "--regexp-ignore-case", "-E",
        "--extended-regexp", "-F", "--fixed-strings", "--all-match", "--invert-grep", "-e", "-l",
        "-t", "--abbrev", "--pretty", "--date", "--dirstat", "-n", "--numbered", "--email",
        "--porcelain", "--line-porcelain", "--root", "--show-stats",
    ],
    &[
        "--stat", "--abbrev", "--pretty", "--format", "--date", "--color", "--word-diff",
        "--decorate", "--relative", "-M", "-C", "-U", "--unified", "-n", "--max-count", "--skip",
        "--since", "--until", "--after", "--before", "--author", "--committer", "--grep", "-S",
        "-G", "--diff-filter", "-L", "--dirstat", "--encoding", "--diff-algorithm",
        "--max-parents", "--min-parents",
    ],
)
// `-5`: a count.
.numbers();
const STATUS: Spec = Spec::new(
    &[
        "-s", "--short", "-b", "--branch", "--porcelain", "-u", "--untracked-files", "--ignored",
        "-v", "--verbose", "-z", "--long", "--show-stash", "--ahead-behind", "--no-ahead-behind",
        "--no-renames", "--renames",
    ],
    &[
        "--porcelain", "-u", "--untracked-files", "--ignored", "--column",
    ],
);
const GREP: Spec = Spec::new(
    &[
        "-n", "--line-number", "-i", "--ignore-case", "-w", "--word-regexp", "-l",
        "--files-with-matches", "-L", "--files-without-match", "-c", "--count", "-v",
        "--invert-match", "-E", "--extended-regexp", "-F", "--fixed-strings", "-P",
        "--perl-regexp", "-h", "-H", "-I", "--cached", "--untracked", "--no-index", "-q",
        "--quiet", "--heading", "--break", "-p", "--show-function", "-W", "--function-context",
        "--and", "--or", "--not", "--all-match", "--full-name", "--column", "-o",
        "--only-matching", "-z", "--null", "-a", "--text", "--color", "--no-color",
    ],
    &[
        "-e", "-f", "-A", "-B", "-C", "--max-depth", "--after-context", "--before-context",
        "--context", "-m", "--max-count", "--threads", "--color",
    ],
);
const LS_FILES: Spec = Spec::new(
    &[
        "-c", "-d", "-m", "-o", "-i", "-s", "-u", "-k", "-z", "-t", "-v", "-f", "--cached",
        "--deleted", "--modified", "--others", "--ignored", "--stage", "--unmerged", "--killed",
        "--exclude-standard", "--directory", "--no-empty-directory", "--full-name",
        "--error-unmatch", "--abbrev",
    ],
    &[
        "-x", "--exclude", "-X", "--exclude-from", "--format", "--abbrev",
    ],
);
const LS_TREE: Spec = Spec::new(
    &[
        "-r", "-d", "-t", "-l", "-z", "--name-only", "--name-status", "--full-name", "--full-tree",
        "--long", "--object-only", "--abbrev",
    ],
    &["--format", "--abbrev"],
);
const REV_PARSE: Spec = Spec::new(
    &[
        "--show-toplevel",
        "--show-prefix",
        "--show-cdup",
        "--git-dir",
        "--git-common-dir",
        "--absolute-git-dir",
        "--is-inside-work-tree",
        "--is-inside-git-dir",
        "--is-bare-repository",
        "--is-shallow-repository",
        "--abbrev-ref",
        "--short",
        "--verify",
        "-q",
        "--quiet",
        "--symbolic",
        "--symbolic-full-name",
        "--all",
        "--branches",
        "--tags",
        "--remotes",
        "--sq",
        "--show-superproject-working-tree",
        "--show-object-format",
        "--local-env-vars",
    ],
    &[
        "--abbrev-ref", "--short", "--git-path", "--default", "--since", "--until",
    ],
);
const DESCRIBE: Spec = Spec::new(
    &[
        "--tags", "--all", "--always", "--long", "--dirty", "--exact-match", "--contains",
        "--first-parent",
    ],
    &[
        "--abbrev", "--match", "--exclude", "--candidates", "--dirty",
    ],
);
const FOR_EACH_REF: Spec = Spec::new(
    &[
        "--merged", "--no-merged", "--contains", "--shell", "--perl", "--python", "--tcl",
        "--omit-empty",
    ],
    &[
        "--count", "--sort", "--format", "--points-at", "--merged", "--no-merged", "--contains",
    ],
);
const SHOW_REF: Spec = Spec::new(
    &[
        "--heads", "--tags", "--head", "--verify", "-q", "--quiet", "-s", "--hash", "--abbrev",
        "-d", "--dereference", "--exists",
    ],
    &["--hash", "--abbrev"],
);
const STASH_PUSH: Spec = Spec::new(
    &[
        "-q", "--quiet", "--staged", "-S", "--keep-index", "-k", "-p", "--patch",
    ],
    &["-m", "--message"],
);
const BRANCH: Spec = Spec::new(
    &[
        "-a", "--all", "-r", "--remotes", "-v", "--verbose", "--list", "-l", "--show-current",
        "--contains", "--merged", "--no-merged", "--no-color", "--color",
    ],
    &[
        "--sort", "--format", "--contains", "--merged", "--no-merged", "--color", "--points-at",
    ],
);
const TAG: Spec = Spec::new(
    &[
        "-l", "--list", "-a", "--annotate", "-s", "--sign", "--contains",
    ],
    &[
        "--sort", "--contains", "--points-at", "--format", "-m", "--message",
    ],
);
const CONFIG: Spec = Spec::new(
    &[
        "--get", "--get-all", "--get-regexp", "--get-urlmatch", "--list", "-l", "--show-origin",
        "--show-scope", "--name-only", "--global", "--local", "--system", "--worktree",
        "--includes", "--null", "-z",
    ],
    &["--type", "--file", "-f", "--default"],
);
const ADD: Spec = Spec::new(
    &[
        "-A", "--all", "-u", "--update", "-N", "--intent-to-add", "-p", "--patch", "-v",
        "--verbose", "-n", "--dry-run", "-f", "--force",
    ],
    &[],
);
const COMMIT: Spec = Spec::new(
    &[
        "-a", "--all", "-q", "--quiet", "-v", "--verbose", "-s", "--signoff", "--amend",
        "--no-edit", "--allow-empty", "-S", "--gpg-sign", "--no-verify", "-n",
    ],
    &[
        "-m", "--message", "-F", "--file", "--author", "--date", "--fixup", "-S", "--gpg-sign",
    ],
);
const INIT: Spec = Spec::new(&["-q", "--quiet", "--bare"], &["-b", "--initial-branch"]);
const CLONE: Spec = Spec::new(
    &[
        "-q", "--quiet", "--single-branch", "--no-tags", "--no-checkout", "--bare",
    ],
    &[
        "--depth", "--branch", "-b", "--filter", "--origin", "-o", "--shallow-since",
    ],
);
const FETCH: Spec = Spec::new(
    &[
        "--all", "--prune", "-p", "--tags", "-t", "-q", "--quiet", "-v", "--verbose",
        "--unshallow", "--no-tags", "--dry-run",
    ],
    &["--depth"],
);
const PULL: Spec = Spec::new(
    &[
        "--ff-only", "--no-rebase", "--rebase", "-r", "-q", "--quiet", "-v", "--verbose",
        "--no-edit", "--tags",
    ],
    &[],
);
const PUSH: Spec = Spec::new(
    &[
        "-u", "--set-upstream", "-q", "--quiet", "-v", "--verbose", "--tags", "--follow-tags",
        "--dry-run", "-n", "--no-verify",
    ],
    &[],
);
const CHECKOUT: Spec = Spec::new(
    &["-q", "--quiet", "--no-track", "--track", "-t"],
    &["-b", "--orphan"],
);
const SWITCH: Spec = Spec::new(
    &["-q", "--quiet", "--no-track", "--track", "-t"],
    &["-c", "--create", "--orphan"],
);
const RESTORE: Spec = Spec::new(&["--staged", "-S", "-q", "--quiet", "-p", "--patch"], &[]);
const RM: Spec = Spec::new(&["--cached", "-r", "-q", "--quiet", "-n", "--dry-run"], &[]);
const RESET: Spec = Spec::new(&["--soft", "--mixed", "-q", "--quiet"], &[]);
const CLEAN: Spec = Spec::new(&["-n", "--dry-run", "-d", "-x", "-X"], &[]);
