//! The kept environment (design §12 "Step 5"): what plugin runtimes, stdio
//! MCP servers and the adapters' helper scripts inherit of yobuild's own
//! environment — a fixed set of names and prefixes, and what an entry's
//! `env` grant (or a server's `env` table) adds. Everything else is taken
//! out (`/usr/bin/env -u`, `frontend::host::env_without`), on unix; off
//! unix only the credentials yoagent reads, set empty (removed, for the
//! helpers; step 4).

use std::ffi::OsStr;

use crate::frontend::host::Inherit;

/// The names every runtime, stdio server and helper keeps. None is a
/// credential yoagent reads nor a name `manifest::secret` flags but `PWD`
/// (the working folder, whose name is a password's abbreviation) (tested).
#[rustfmt::skip]
pub(crate) const KEPT: &[&str] = &[
    // The process.
    "PATH", "HOME", "USER", "LOGNAME", "SHELL", "PWD", "TMPDIR", "TMP", "TEMP", "TZ",
    // A `node` from Nix, Linuxbrew, conda or an HPC module, and a native
    // addon linked against such libraries, may not start without it.
    "LD_LIBRARY_PATH",
    // The locale (and `LC_*`, below).
    "LANG", "LANGUAGE",
    // The terminal, and pi-tui's detection list.
    "TERM", "COLORTERM", "TERM_PROGRAM", "TERM_PROGRAM_VERSION", "NO_COLOR", "FORCE_COLOR",
    "COLUMNS", "LINES", "TMUX", "STY", "ZELLIJ", "WT_SESSION", "KITTY_WINDOW_ID",
    "ITERM_SESSION_ID", "WEZTERM_PANE", "GHOSTTY_RESOURCES_DIR", "WARP_SESSION_ID",
    "WARP_TERMINAL_SESSION_UUID", "TERMINAL_EMULATOR", "TERMUX_VERSION", "SSH_TTY",
    "SSH_CONNECTION", "SSH_CLIENT", "DISPLAY", "WAYLAND_DISPLAY",
    // The session.
    "BROWSER", "CI",
    // The network.
    "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY", "http_proxy", "https_proxy",
    "all_proxy", "no_proxy", "NODE_USE_ENV_PROXY", "NODE_USE_SYSTEM_CA",
    "NODE_EXTRA_CA_CERTS", "SSL_CERT_FILE", "SSL_CERT_DIR",
    // Standard folders.
    "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME", "XDG_RUNTIME_DIR",
    // Where programs live: those pi-lens reads, and their kin.
    "GOPATH", "GOBIN", "CARGO_HOME", "JAVA_HOME", "JDTLS_PATH", "VIRTUAL_ENV", "CONDA_PREFIX",
    "UV_PROJECT_ENVIRONMENT", "BUN_INSTALL", "GOROOT", "RUSTUP_HOME", "PNPM_HOME", "NVM_DIR",
    "VOLTA_HOME", "DENO_INSTALL",
    // Version managers' homes: a `node` on `PATH` that is an asdf or mise
    // shim finds its versions through them.
    "ASDF_DIR", "ASDF_DATA_DIR", "MISE_DATA_DIR", "MISE_CONFIG_DIR",
    // The rutis Node runtime's one setting (its `RUTIS_TOKEN` and TLS
    // files are a remote link's credentials: not kept).
    "RUTIS_SHUTDOWN_TIMEOUT",
];

/// The prefixes every runtime, stdio server and helper keeps.
pub(crate) const KEPT_PREFIXES: &[&str] = &["LC_"];

/// What the terminal UI's runtime (`ui`, yobuild's own code) keeps beside
/// [`KEPT`]: pi-tui's own settings. A prefix ends in `*`, as
/// `PluginHostBuilder::keep` takes it.
pub(crate) const UI_KEPT: &[&str] = &[
    "PI_HYPERLINKS", "PI_TRUE_COLOR", "PI_IMAGE_PROTOCOL", "PI_PROGRAM_STATUS", "PI_TUI_*",
];

/// What loads code into a process: never kept, and an `env` grant passes
/// one only by its full name (a prefix matching one is refused).
pub(crate) const CODE_LOADING: &[&str] = &["NODE_OPTIONS", "NODE_PATH", "LD_PRELOAD"];

/// [`CODE_LOADING`]'s prefix: macOS's dynamic loader's variables.
pub(crate) const CODE_LOADING_PREFIX: &str = "DYLD_";

/// What every runtime inherits: [`KEPT`] and [`KEPT_PREFIXES`].
pub(crate) fn inherit() -> Inherit {
    Inherit::only(KEPT.iter().copied(), KEPT_PREFIXES.iter().copied())
}

/// `name` is in the kept set.
pub(crate) fn kept(name: &OsStr) -> bool {
    inherit().keeps(name)
}

/// The proxy variables of [`KEPT`]: one whose value holds a password
/// reaches every plugin and stdio server ([`proxy_password`]).
pub(crate) const PROXIES: &[&str] = &[
    "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy",
];

/// A proxy setting whose URL holds a written password, with a scheme
/// (`http://user:pw@proxy:3128`) or without (`user:pw@proxy:3128`, which
/// curl and Node's proxy agents read as `http://`).
pub(crate) fn proxy_password(value: &str) -> bool {
    match value.contains("://") {
        true => crate::manifest::written_password(value),
        false => crate::manifest::written_password(&format!("http://{value}")),
    }
}

/// The limits of [`names_read`] on one entry: a package is its own files,
/// not its dependencies, but some ship a lot. At most `SCAN_FILES` code
/// files are read, each only when at most `SCAN_BYTES` (a bundle is one big
/// file, often: `dsh-univer-office`'s gateway is 58 MiB), at most
/// `SCAN_TOTAL` bytes in all, and at most `SCAN_ENTRIES` files and folders
/// looked at.
const SCAN_FILES: usize = 5000;
const SCAN_BYTES: u64 = 64 << 20;
const SCAN_TOTAL: u64 = 512 << 20;
const SCAN_ENTRIES: usize = 50_000;

/// What [`names_read`] found: the names, and what it could not look at.
#[derive(Debug, Default)]
pub(crate) struct Scan {
    pub names: std::collections::BTreeSet<String>,
    /// Code files read, and their bytes.
    pub files: usize,
    pub bytes: u64,
    /// Code files over `SCAN_BYTES`, not read.
    pub too_big: Vec<std::path::PathBuf>,
    /// Links below the start, not followed (they may lead out of the
    /// entry's code, or back into it).
    pub links: usize,
    /// Files and folders that could not be read.
    pub unreadable: usize,
    /// A limit stopped the walk.
    pub stopped: bool,
}

impl Scan {
    /// What the scan missed, said for `check`'s note on the entry; None
    /// when it read everything it found.
    pub fn missed(&self) -> Option<String> {
        let mut missed = Vec::new();
        if self.stopped {
            missed.push(format!(
                "it stopped at a limit: {SCAN_FILES} code files, {} MiB of code or {SCAN_ENTRIES} files and folders",
                SCAN_TOTAL >> 20
            ));
        }
        if let Some(first) = self.too_big.first() {
            missed.push(match self.too_big.len() {
                1 => format!("`{}` is over {} MiB", first.display(), SCAN_BYTES >> 20),
                n => format!(
                    "{n} files over {} MiB, `{}` among them",
                    SCAN_BYTES >> 20,
                    first.display()
                ),
            });
        }
        if self.links > 0 {
            missed.push(format!("{} link(s) not followed", self.links));
        }
        if self.unreadable > 0 {
            missed.push(format!(
                "{} file(s) or folder(s) could not be read",
                self.unreadable
            ));
        }
        (!missed.is_empty()).then(|| missed.join(", "))
    }
}

/// The environment variables the code at `path` (a file, or a package's
/// folder without its `node_modules`) names: `process.env.X` and
/// `process.env["X"]` (or `'X'`, `` `X` ``) in its `.js`, `.mjs`, `.cjs`,
/// `.ts`, `.mts` and `.cts` files (design §12 "Step 5", the measurement's
/// scan). A hint, not a proof: a name a dependency reads, one destructured
/// (`const { X } = process.env`), computed, or looked up through another
/// object is not seen; one in dead code is. `path` itself may be a link;
/// links below it are not followed. What it could not read is in the
/// [`Scan`], for `check` to say.
pub(crate) fn names_read(path: &std::path::Path) -> Scan {
    let mut scan = Scan::default();
    let mut entries = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(at) = stack.pop() {
        entries += 1;
        if entries > SCAN_ENTRIES {
            scan.stopped = true;
            break;
        }
        let meta = match at == path {
            true => std::fs::metadata(&at),
            false => std::fs::symlink_metadata(&at),
        };
        let Ok(meta) = meta else {
            scan.unreadable += 1;
            continue;
        };
        if meta.file_type().is_symlink() {
            scan.links += 1;
            continue;
        }
        if meta.is_dir() {
            if at.file_name().is_some_and(|n| n == "node_modules") && at != path {
                continue;
            }
            match std::fs::read_dir(&at) {
                Ok(read) => {
                    for entry in read {
                        match entry {
                            Ok(entry) => stack.push(entry.path()),
                            Err(_) => scan.unreadable += 1,
                        }
                    }
                }
                Err(_) => scan.unreadable += 1,
            }
            continue;
        }
        let code = at
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| ["js", "mjs", "cjs", "ts", "mts", "cts"].contains(&e));
        if !code {
            continue;
        }
        if meta.len() > SCAN_BYTES {
            scan.too_big
                .push(at.strip_prefix(path).unwrap_or(&at).to_path_buf());
            continue;
        }
        if scan.files == SCAN_FILES || scan.bytes + meta.len() > SCAN_TOTAL {
            scan.stopped = true;
            break;
        }
        scan.files += 1;
        scan.bytes += meta.len();
        // The names are ASCII: a file with other bytes (Latin-1 in a
        // vendored file) is read all the same.
        match std::fs::read(&at) {
            Ok(bytes) => scan
                .names
                .extend(env_names(&String::from_utf8_lossy(&bytes))),
            Err(_) => scan.unreadable += 1,
        }
    }
    scan.too_big.sort();
    scan
}

/// Whether `name` loads code into a process ([`CODE_LOADING`], `DYLD_*`).
pub(crate) fn loads_code(name: &str) -> bool {
    CODE_LOADING.contains(&name) || name.starts_with(CODE_LOADING_PREFIX)
}

/// The names `text` reads as `process.env.X` or `process.env["X"]`.
fn env_names(text: &str) -> Vec<String> {
    let name = |s: &str| -> Option<String> {
        let end = s
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(s.len());
        let n = &s[..end];
        (!n.is_empty() && !n.starts_with(|c: char| c.is_ascii_digit())).then(|| n.to_owned())
    };
    let mut out = Vec::new();
    for (at, _) in text.match_indices("process.env") {
        let rest = &text[at + "process.env".len()..];
        if let Some(after) = rest.strip_prefix('.') {
            out.extend(name(after));
        } else if let Some(after) = rest.strip_prefix('[') {
            let after = after.trim_start();
            let Some(quote) = after
                .chars()
                .next()
                .filter(|c| matches!(c, '"' | '\'' | '`'))
            else {
                continue;
            };
            let inner = &after[1..];
            if let Some(n) =
                name(inner).filter(|n| inner[n.len()..].trim_start().starts_with(quote))
            {
                let close = inner[n.len()..].trim_start()[1..].trim_start();
                if close.starts_with(']') {
                    out.push(n);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kept_set_holds_no_credential_nor_secret_nor_code_loader() {
        for name in KEPT.iter().chain(UI_KEPT) {
            assert!(
                !crate::manifest::KEY_VARIABLES.contains(name),
                "{name} is a credential yoagent reads"
            );
            assert!(
                *name == "PWD" || !crate::manifest::secret(name),
                "{name} looks like a secret"
            );
            assert!(!loads_code(name), "{name} loads code");
        }
        for prefix in KEPT_PREFIXES {
            assert!(
                !crate::manifest::KEY_VARIABLES
                    .iter()
                    .any(|k| k.starts_with(prefix)),
                "{prefix} matches a credential"
            );
        }
        assert!(!kept(OsStr::new("RUTIS_TOKEN")));
        assert!(!kept(OsStr::new("NODE_OPTIONS")));
        assert!(!kept(OsStr::new("AWS_PROFILE")));
        assert!(kept(OsStr::new("LC_ALL")));
        assert!(kept(OsStr::new("HTTPS_PROXY")));
    }

    #[test]
    fn the_scan_finds_process_env_names_but_not_in_node_modules() {
        let text = "a = process.env.TAVILY_API_KEY; b = process.env[\"X_1\"]; c = process.env[ 'Y' ];\n\
                    d = process.env[`Z`]; e = process.env[name]; f = process.env.1BAD; g = process.envy;\n\
                    h = process.env['A' + b];";
        assert_eq!(env_names(text), ["TAVILY_API_KEY", "X_1", "Y", "Z"]);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.ts"), "process.env.OWN").unwrap();
        std::fs::write(dir.path().join("b.md"), "process.env.DOC").unwrap();
        std::fs::create_dir_all(dir.path().join("lib")).unwrap();
        std::fs::write(dir.path().join("lib/c.mjs"), "process.env['NESTED']").unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/dep")).unwrap();
        std::fs::write(dir.path().join("node_modules/dep/d.js"), "process.env.DEP").unwrap();
        // Not UTF-8 (Latin-1), read all the same.
        std::fs::write(
            dir.path().join("lib/latin.cjs"),
            b"// caf\xe9\nprocess.env.LATIN",
        )
        .unwrap();
        let scan = names_read(dir.path());
        let names: Vec<&String> = scan.names.iter().collect();
        assert_eq!(names, ["LATIN", "NESTED", "OWN"]);
        assert_eq!((scan.files, scan.missed()), (3, None));
        let file = names_read(&dir.path().join("a.ts"));
        assert_eq!(file.names.into_iter().collect::<Vec<_>>(), ["OWN"]);
    }

    #[test]
    fn the_scan_says_what_it_missed_and_follows_no_link_below_its_start() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("pkg");
        std::fs::create_dir_all(root.join("dist")).unwrap();
        std::fs::write(root.join("a.js"), "process.env.OWN").unwrap();
        // A bundle over the limit: not read, said.
        let big = std::fs::File::create(root.join("dist/bundle.js")).unwrap();
        big.set_len(SCAN_BYTES + 1).unwrap();
        std::fs::write(dir.path().join("outside.js"), "process.env.OUTSIDE").unwrap();
        #[cfg(unix)]
        {
            // A loop back to its start, and a link out of it.
            std::os::unix::fs::symlink(&root, root.join("again")).unwrap();
            std::os::unix::fs::symlink("..", root.join("up")).unwrap();
            std::os::unix::fs::symlink(dir.path().join("outside.js"), root.join("b.js")).unwrap();
        }
        let scan = names_read(&root);
        assert_eq!(scan.names.iter().collect::<Vec<_>>(), ["OWN"]);
        assert_eq!(scan.too_big, [std::path::PathBuf::from("dist/bundle.js")]);
        #[cfg(unix)]
        assert_eq!(
            scan.missed().unwrap(),
            "`dist/bundle.js` is over 64 MiB, 3 link(s) not followed"
        );
        // The start itself may be a link.
        #[cfg(unix)]
        {
            let link = dir.path().join("link");
            std::os::unix::fs::symlink(&root, &link).unwrap();
            assert_eq!(names_read(&link).names.iter().collect::<Vec<_>>(), ["OWN"]);
        }
        // The file limit stops the walk, said.
        let many = dir.path().join("many");
        std::fs::create_dir_all(&many).unwrap();
        for i in 0..=SCAN_FILES {
            std::fs::write(many.join(format!("{i}.js")), "").unwrap();
        }
        let scan = names_read(&many);
        assert!(scan.stopped && scan.files == SCAN_FILES);
        assert!(scan
            .missed()
            .unwrap()
            .starts_with("it stopped at a limit: 5000 code files"));
        // Nothing there: nothing read, one thing unreadable.
        let gone = names_read(&dir.path().join("gone"));
        assert_eq!((gone.files, gone.unreadable), (0, 1));
    }

    #[test]
    fn a_proxy_password_is_seen_with_or_without_a_scheme() {
        assert!(proxy_password("http://me:pw@proxy:3128"));
        assert!(proxy_password("me:pw@proxy:3128"));
        assert!(!proxy_password("http://proxy:3128"));
        assert!(!proxy_password("proxy:3128"));
        assert!(!proxy_password("http://me@proxy:3128"));
        for name in PROXIES {
            assert!(KEPT.contains(name));
        }
    }
}
