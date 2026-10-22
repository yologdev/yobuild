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

/// The most files and bytes [`names_read`] reads of one entry: a package
/// is its own files, not its dependencies, but some ship a lot.
const SCAN_FILES: usize = 5000;
const SCAN_BYTES: u64 = 4 << 20;

/// The environment variables the code at `path` (a file, or a package's
/// folder without its `node_modules`) names: `process.env.X` and
/// `process.env["X"]` (or `'X'`, `` `X` ``) in its `.js`, `.mjs`, `.cjs`,
/// `.ts`, `.mts` and `.cts` files (design §12 "Step 5", the measurement's
/// scan). A hint, not a proof: a name a dependency reads, one destructured
/// (`const { X } = process.env`), computed, or looked up through another
/// object is not seen; one in dead code is. What cannot be read is skipped.
pub(crate) fn names_read(path: &std::path::Path) -> std::collections::BTreeSet<String> {
    let mut names = std::collections::BTreeSet::new();
    let mut files = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(at) = stack.pop() {
        let Ok(meta) = std::fs::metadata(&at) else {
            continue;
        };
        if meta.is_dir() {
            if at.file_name().is_some_and(|n| n == "node_modules") && at != path {
                continue;
            }
            if let Ok(read) = std::fs::read_dir(&at) {
                stack.extend(read.flatten().map(|e| e.path()));
            }
            continue;
        }
        let code = at
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| ["js", "mjs", "cjs", "ts", "mts", "cts"].contains(&e));
        if !code || meta.len() > SCAN_BYTES {
            continue;
        }
        files += 1;
        if files > SCAN_FILES {
            break;
        }
        if let Ok(text) = std::fs::read_to_string(&at) {
            names.extend(env_names(&text));
        }
    }
    names
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
            assert!(
                !CODE_LOADING.contains(name) && !name.starts_with(CODE_LOADING_PREFIX),
                "{name} loads code"
            );
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
        let names: Vec<String> = names_read(dir.path()).into_iter().collect();
        assert_eq!(names, ["NESTED", "OWN"]);
        assert_eq!(
            names_read(&dir.path().join("a.ts"))
                .into_iter()
                .collect::<Vec<_>>(),
            ["OWN"]
        );
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
