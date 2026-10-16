//! What each program in the [`catalogue`](super::catalogue) may be given.
//! Options left out ask: they write elsewhere, run a program, load code,
//! or are not read here.

use super::args::Spec;

pub(super) const READ: Spec = Spec::new(
    &["-r", "-s", "-e"],
    &["-p", "-t", "-n", "-N", "-d", "-u", "-a"],
);
pub(super) const PRINTF: Spec = Spec::new(&[], &["-v"]).posix();
pub(super) const SHELL: Spec = Spec::new(
    &[
        "-e", "-u", "-x", "-v", "-c", "-l", "-E", "-T", "-f", "-h", "-B", "-C", "-n", "--norc",
        "--noprofile", "--posix", "--login", "--noediting",
    ],
    &["-o", "-O"],
)
.posix();
pub(super) const MKDIR: Spec =
    Spec::new(&["-p", "-v", "--parents", "--verbose"], &["-m", "--mode"]);
pub(super) const TOUCH: Spec = Spec::new(
    &["-a", "-m", "-c", "-h", "--no-create"],
    &["-d", "--date", "-t", "-r", "--reference"],
);
pub(super) const CHMOD: Spec = Spec::new(
    &[
        "-v", "-f", "-c", "-h", "--verbose", "--silent", "--quiet", "--changes",
    ],
    &[],
);
pub(super) const RM: Spec = Spec::new(
    &[
        "-f", "-r", "-R", "-i", "-I", "-v", "-d", "--force", "--recursive", "--verbose", "--dir",
    ],
    &[],
);
pub(super) const RMDIR: Spec = Spec::new(&["-v", "--verbose", "--ignore-fail-on-non-empty"], &[]);
pub(super) const CP: Spec = Spec::new(
    &[
        "-r", "-R", "-a", "-p", "-v", "-f", "-i", "-n", "-u", "-L", "-P", "-H", "--recursive",
        "--archive", "--verbose", "--force", "--interactive", "--no-clobber", "--update",
        "--dereference", "--no-dereference", "--preserve",
    ],
    &["-t", "--target-directory", "--preserve"],
);
pub(super) const MV: Spec = Spec::new(
    &[
        "-v", "-f", "-i", "-n", "-u", "--verbose", "--force", "--interactive", "--no-clobber",
        "--update",
    ],
    &["-t", "--target-directory"],
);
pub(super) const INSTALL: Spec = Spec::new(
    &[
        "-v", "-c", "-C", "-p", "-D", "-d", "--verbose", "--compare", "--preserve-timestamps",
        "--directory",
    ],
    &[
        "-m", "--mode", "-t", "--target-directory", "-o", "-g", "--owner", "--group",
    ],
);
pub(super) const LN: Spec = Spec::new(
    &[
        "-s", "-f", "-n", "-v", "-r", "--symbolic", "--force", "--no-dereference", "--verbose",
        "--relative",
    ],
    &[],
);
pub(super) const TEE: Spec = Spec::new(&["-a", "--append", "-i", "-p", "--ignore-interrupts"], &[]);
pub(super) const RSYNC: Spec = Spec::new(
    &[
        "-a", "-r", "-v", "-z", "-h", "-P", "-n", "-u", "-c", "-l", "-t", "-p", "-o", "-g", "-D",
        "-q", "-i", "--archive", "--recursive", "--verbose", "--compress", "--human-readable",
        "--progress", "--partial", "--dry-run", "--update", "--checksum", "--links", "--times",
        "--perms", "--stats", "--itemize-changes", "--quiet",
    ],
    &["--exclude", "--include", "--exclude-from", "--include-from"],
);
pub(super) const CURL: Spec = Spec::new(
    &[
        "-s", "-S", "-L", "-f", "-k", "-i", "-I", "-v", "-g", "-G", "-N", "-4", "-6", "-#",
        "--silent", "--show-error", "--location", "--fail", "--fail-with-body", "--insecure",
        "--include", "--head", "--verbose", "--compressed", "--progress-bar", "--globoff", "--get",
        "--no-buffer", "--http1.1", "--http2",
    ],
    &[
        "-o", "--output", "-X", "--request", "-H", "--header", "-d", "--data", "--data-raw",
        "--data-binary", "--data-urlencode", "-F", "--form", "-A", "--user-agent", "-e",
        "--referer", "-m", "--max-time", "--connect-timeout", "--retry", "-w", "--write-out", "-u",
        "--user", "-b", "--cookie", "-c", "--cookie-jar", "-D", "--dump-header", "-r", "--range",
        "-x", "--proxy", "--url", "--trace", "--trace-ascii", "--stderr",
    ],
);
pub(super) const WGET: Spec = Spec::new(
    &[
        "-q", "-v", "-S", "--quiet", "--verbose", "--no-verbose", "--server-response", "--spider",
        "--no-check-certificate",
    ],
    &[
        "-O", "--output-document", "-o", "--output-file", "-a", "--append-output", "-U",
        "--user-agent", "--header", "-t", "--tries", "-T", "--timeout", "--post-data",
        "--body-data", "--method", "--max-redirect", "-i", "--input-file",
    ],
);
pub(super) const SED: Spec = Spec::new(
    &[
        "-n", "-E", "-r", "-s", "-u", "-z", "--quiet", "--silent", "--regexp-extended", "--posix",
        "--separate", "--unbuffered", "--null-data", "--debug",
    ],
    &["-e", "--expression", "-l", "--line-length"],
);
pub(super) const AWK: Spec = Spec::new(&[], &["-F", "-v", "--field-separator", "--assign"]).posix();
pub(super) const SORT: Spec = Spec::new(
    &[
        "-n", "-r", "-u", "-f", "-b", "-d", "-g", "-h", "-M", "-V", "-s", "-c", "-C", "-z", "-R",
        "--numeric-sort", "--reverse", "--unique", "--ignore-case", "--version-sort",
        "--human-numeric-sort", "--stable", "--check", "--zero-terminated",
        "--general-numeric-sort", "--random-sort", "--ignore-leading-blanks", "--dictionary-order",
        "--month-sort",
    ],
    &[
        "-k", "-t", "-S", "-T", "--key", "--field-separator", "--buffer-size",
        "--temporary-directory", "--parallel",
    ],
);
pub(super) const UNIQ: Spec = Spec::new(
    &[
        "-c", "-d", "-u", "-i", "-z", "-D", "--count", "--repeated", "--unique", "--ignore-case",
        "--zero-terminated",
    ],
    &[
        "-f", "-s", "-w", "--skip-fields", "--skip-chars", "--check-chars",
    ],
);
pub(super) const RG: Spec = Spec::new(
    &[
        "-i", "-S", "-s", "-w", "-x", "-v", "-c", "-l", "-n", "-N", "-H", "-I", "-o", "-F", "-U",
        "-P", "-L", "-u", "-z", "-a", "-0", "-q", "--hidden", "--no-ignore", "--files",
        "--files-with-matches", "--files-without-match", "--count", "--count-matches", "--json",
        "--vimgrep", "--no-heading", "--heading", "--line-number", "--no-line-number",
        "--fixed-strings", "--multiline", "--pcre2", "--follow", "--search-zip", "--smart-case",
        "--ignore-case", "--case-sensitive", "--word-regexp", "--invert-match", "--only-matching",
        "--with-filename", "--no-filename", "--text", "--null", "--quiet", "--stats",
        "--no-messages", "--trim", "--column", "--byte-offset", "--color",
    ],
    &[
        "-e", "--regexp", "-g", "--glob", "--iglob", "-t", "--type", "-T", "--type-not", "-A",
        "-B", "-C", "--context", "--after-context", "--before-context", "-m", "--max-count", "-d",
        "--max-depth", "--color", "--colors", "--sort", "--sortr", "-j", "--threads", "-M",
        "--max-columns", "-E", "--encoding", "-f", "--file", "-r", "--replace", "--type-add",
        "--max-filesize",
    ],
);
pub(super) const FD: Spec = Spec::new(
    &[
        "-H", "-I", "-u", "-s", "-i", "-g", "-F", "-a", "-L", "-l", "-0", "-p", "-1", "-q",
        "--hidden", "--no-ignore", "--case-sensitive", "--ignore-case", "--glob",
        "--fixed-strings", "--absolute-path", "--follow", "--list-details", "--full-path",
        "--print0", "--quiet", "--show-errors", "--prune",
    ],
    &[
        "-e", "--extension", "-t", "--type", "-d", "--max-depth", "--min-depth", "--exact-depth",
        "-E", "--exclude", "-S", "--size", "--changed-within", "--changed-before", "-o", "--owner",
        "-c", "--color", "-j", "--threads", "--max-results", "--base-directory", "--search-path",
    ],
);
pub(super) const TREE: Spec = Spec::new(
    &[
        "-a", "-d", "-f", "-i", "-l", "-x", "-C", "-n", "-h", "-s", "-D", "-F", "-J", "-p", "-u",
        "-g", "-q", "-N", "-Q", "--noreport", "--dirsfirst", "--gitignore", "--du", "--prune",
        "--matchdirs",
    ],
    &["-L", "-I", "-P", "--charset", "--filelimit", "--sort"],
);
pub(super) const FILE: Spec = Spec::new(
    &[
        "-b", "-i", "-L", "-z", "-k", "-h", "-s", "-E", "-N", "--brief", "--mime", "--mime-type",
        "--mime-encoding", "--dereference", "--no-dereference", "--uncompress",
    ],
    &[
        "-m", "--magic-file", "-F", "--separator", "-f", "--files-from",
    ],
);
pub(super) const XXD: Spec = Spec::new(
    &["-p", "-u", "-i", "-b", "-e", "-a", "-r", "-C"],
    &["-c", "-l", "-s", "-g", "-o", "-n"],
);
pub(super) const BASE64: Spec = Spec::new(
    &["-d", "-D", "--decode", "--ignore-garbage"],
    &["-w", "--wrap", "-b", "-o", "--output", "--input"],
);
pub(super) const DATE: Spec = Spec::new(
    &["-u", "-R", "--utc", "--rfc-2822", "-I", "--iso-8601"],
    &["-d", "--date", "-r", "--reference", "-I", "--iso-8601"],
);
pub(super) const HOSTNAME: Spec = Spec::new(
    &[
        "-s", "-f", "-i", "-I", "-d", "-A", "--short", "--fqdn", "--long",
    ],
    &[],
);
pub(super) const MKTEMP: Spec = Spec::new(
    &[
        "-d", "-q", "-u", "-t", "--directory", "--quiet", "--dry-run",
    ],
    &["-p", "--tmpdir", "--suffix"],
);
pub(super) const TAR: Spec = Spec::new(
    &[
        "-c", "-t", "-z", "-j", "-J", "-v", "-h", "-a", "--create", "--list", "--gzip", "--bzip2",
        "--xz", "--zstd", "--verbose", "--dereference", "--auto-compress", "--exclude-vcs",
        "--no-recursion", "--totals",
    ],
    &[
        "-f", "--file", "-C", "--directory", "--exclude", "-T", "--files-from", "-X",
        "--exclude-from", "--format",
    ],
);
pub(super) const ZIP: Spec = Spec::new(
    &[
        "-r", "-q", "-v", "-j", "-y", "-X", "-D", "--recurse-paths", "--quiet", "--junk-paths",
    ],
    &["-x", "-i", "--exclude", "--include"],
)
.numbers();
pub(super) const UNZIP: Spec = Spec::new(&["-l", "-t", "-v", "-q", "-z"], &[]);
pub(super) const GZIP: Spec = Spec::new(
    &[
        "-c", "-d", "-l", "-t", "-v", "-q", "--stdout", "--decompress", "--list", "--test",
        "--quiet", "--verbose",
    ],
    &[],
)
.numbers();
pub(super) const CARGO: Spec = Spec::new(
    &[
        "-q", "-v", "--quiet", "--verbose", "--frozen", "--locked", "--offline", "--version", "-V",
        "--help", "-h", "--list",
    ],
    &["--color"],
);
pub(super) const CARGO_BUILD: Spec = Spec::new(
    &[
        "--release", "-r", "--all-targets", "--lib", "--bins", "--tests", "--benches",
        "--examples", "--workspace", "--all", "--all-features", "--no-default-features", "-q",
        "-v", "--quiet", "--verbose", "--locked", "--frozen", "--offline", "--no-fail-fast",
        "--doc", "--keep-going", "--no-deps", "--document-private-items", "--no-run",
        // `cargo nextest run`'s own.
        "--no-capture", "--nocapture",
    ],
    &[
        "-p", "--package", "--features", "-F", "--target", "--bin", "--example", "--test",
        "--bench", "--profile", "-j", "--jobs", "--color", "--message-format", "--exclude",
        "--target-dir", "-E", "--filterset",
    ],
);
pub(super) const CARGO_FMT: Spec = Spec::new(
    &["--check", "--all", "-q", "-v", "--quiet", "--verbose"],
    &["-p", "--package"],
);
pub(super) const CARGO_READ: Spec = Spec::new(
    &[
        "-q", "-v", "--quiet", "--verbose", "--no-deps", "--offline", "--locked", "--frozen",
        "--all-features", "--no-default-features", "--workspace", "--duplicates", "-d",
    ],
    &[
        "--format-version", "-p", "--package", "--features", "-F", "--depth", "-i", "--invert",
        "--target", "-e", "--edges", "--prefix", "--format", "--limit", "--color",
        "--message-format",
    ],
);
pub(super) const GO_BUILD: Spec = Spec::single_dash(
    &["-v", "-race", "-x", "-a", "-n", "-trimpath", "-cover"],
    &["-o", "-tags", "-ldflags", "-gcflags", "-p", "-mod"],
);
pub(super) const GO_TEST: Spec = Spec::single_dash(
    &[
        "-v", "-race", "-short", "-cover", "-failfast", "-json", "-x", "-benchmem", "-trimpath",
    ],
    &[
        "-run", "-bench", "-count", "-timeout", "-tags", "-p", "-parallel", "-cpu", "-skip",
        "-coverpkg", "-covermode", "-benchtime", "-coverprofile", "-cpuprofile", "-memprofile",
        "-o", "-mod",
    ],
);
pub(super) const GO_VET: Spec = Spec::single_dash(&["-v", "-json"], &["-tags"]);
pub(super) const GO_RUN: Spec = Spec::single_dash(
    &["-v", "-race", "-x", "-a", "-trimpath"],
    &["-tags", "-ldflags", "-gcflags", "-mod"],
);
pub(super) const GO_LIST: Spec = Spec::single_dash(
    &["-m", "-json", "-e", "-deps", "-test", "-u"],
    &["-f", "-tags"],
);
pub(super) const GO_ENV: Spec = Spec::single_dash(&["-json"], &[]);
pub(super) const GO_VERSION: Spec = Spec::single_dash(&["-m", "-v"], &[]);
pub(super) const GO_MOD: Spec = Spec::single_dash(&["-m", "-json", "-x", "-vendor"], &[]);
pub(super) const NPM: Spec = Spec::new(
    &[
        "--silent", "-s", "--if-present", "--json", "--workspaces", "--long", "--all",
    ],
    &["--workspace", "--filter"],
);
pub(super) const MAKE: Spec = Spec::new(
    &[
        "-k", "-s", "-n", "-B", "-q", "-w", "-j", "--keep-going", "--silent", "--dry-run",
        "--always-make", "--question", "--no-print-directory", "--jobs",
    ],
    &["-j", "--jobs", "-l", "--load-average"],
);
pub(super) const PYTEST: Spec = Spec::new(
    &[
        "-v", "-q", "-x", "-s", "-l", "--lf", "--ff", "--nf", "--sw", "--co", "--collect-only",
        "--no-header", "--disable-warnings", "--strict-markers", "--last-failed", "--failed-first",
        "--exitfirst", "--verbose", "--quiet", "--showlocals", "--color",
    ],
    &[
        "-k", "-m", "--tb", "--maxfail", "-n", "-r", "--durations", "-W", "--color", "--deselect",
        "--ignore", "--timeout",
    ],
);
pub(super) const CTEST: Spec = Spec::new(
    &[
        "--output-on-failure", "-V", "--verbose", "-N", "--show-only",
    ],
    &["-R", "-E", "-j", "-L", "-C", "--timeout", "--test-dir"],
);
pub(super) const TSC: Spec = Spec::new(
    &[
        "--noEmit", "--pretty", "--strict", "--skipLibCheck", "--listFiles",
        "--extendedDiagnostics",
    ],
    &["-p", "--project", "--target", "--module", "--lib"],
);
pub(super) const ESLINT: Spec = Spec::new(
    &[
        "--quiet",
        "--color",
        "--no-color",
        "--no-error-on-unmatched-pattern",
        "--cache",
    ],
    &["--max-warnings", "--format", "-f", "--ext"],
);
pub(super) const PRETTIER: Spec = Spec::new(
    &["--check", "-c", "--list-different", "-l", "--no-color"],
    &["--log-level", "--ignore-path"],
);
pub(super) const VITEST: Spec = Spec::new(
    &["--run", "--silent", "--passWithNoTests"],
    &["-t", "--testNamePattern", "--reporter", "--project"],
);
pub(super) const JEST: Spec = Spec::new(
    &[
        "--ci", "--runInBand", "-i", "--silent", "--verbose", "--passWithNoTests",
    ],
    &["-t", "--testNamePattern", "--maxWorkers"],
);
pub(super) const MYPY: Spec = Spec::new(
    &[
        "--strict", "--ignore-missing-imports", "--no-error-summary", "--pretty",
        "--show-error-codes", "--no-incremental", "--check-untyped-defs", "-v",
    ],
    &[
        "--python-version", "-p", "--package", "-m", "--module", "--exclude",
    ],
);
pub(super) const RUFF_CHECK: Spec = Spec::new(
    &[
        "-q", "--quiet", "--statistics", "--no-fix", "--exit-zero", "--no-cache", "--show-fixes",
    ],
    &[
        "--select", "--ignore", "--extend-select", "--output-format", "--target-version",
    ],
);
pub(super) const RUFF_FORMAT: Spec =
    Spec::new(&["--check", "--diff", "-q", "--quiet", "--no-cache"], &[]);
pub(super) const RUSTFMT: Spec = Spec::new(
    &["--check", "-q", "-v", "--quiet", "--verbose"],
    &["--edition", "--color"],
);
pub(super) const GOFMT: Spec = Spec::single_dash(&["-l", "-d", "-s", "-e"], &[]);
pub(super) const NODE: Spec = Spec::new(
    &[
        "--test", "--enable-source-maps", "--no-warnings", "--trace-warnings", "--trace-uncaught",
        "--version", "-v",
    ],
    &[],
)
.posix();
pub(super) const INTERPRETER: Spec = Spec::new(&["--version", "-v", "-V"], &[]).posix();
pub(super) const UNITTEST: Spec = Spec::new(&["-v", "-q", "-f", "-b", "-c"], &["-k", "-p"]);
pub(super) const PIP: Spec = Spec::new(
    &[
        "-v", "-q", "-o", "-u", "-l", "--outdated", "--uptodate", "--not-required", "--local",
        "-f", "--files",
    ],
    &["--format"],
);
pub(super) const GH: Spec = Spec::new(
    &[
        "--log", "--log-failed", "--comments", "--exit-status", "--name-only", "--patch", "--watch",
    ],
    &[
        "--json", "-R", "--repo", "-L", "--limit", "-s", "--state", "-A", "--author", "-l",
        "--label", "-q", "--jq", "-t", "--template", "--branch", "-b", "--base", "-B", "--search",
        "-S",
    ],
);
