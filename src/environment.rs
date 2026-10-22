//! The kept environment (design §12 "Step 5"): what plugin runtimes, stdio
//! MCP servers and the adapters' helper scripts inherit of yobuild's own
//! environment — a fixed set of names and prefixes, and what an entry's
//! `env` grant (or a server's `env` table) adds. Everything else is taken
//! out (`/usr/bin/env -u`, `frontend::host::env_without`), on unix; off
//! unix only the credentials yoagent reads are set empty (step 4).

use std::ffi::OsStr;

use crate::frontend::host::Inherit;

/// The names every runtime, stdio server and helper keeps. None is a
/// credential yoagent reads nor a name `manifest::secret` flags but `PWD`
/// (the working folder, whose name is a password's abbreviation) (tested).
pub(crate) const KEPT: &[&str] = &[
    // The process.
    "PATH", "HOME", "USER", "LOGNAME", "SHELL", "PWD", "TMPDIR", "TMP", "TEMP", "TZ",
    // A `node` from Nix, Linuxbrew, conda or an HPC module, and a native
    // addon linked against such libraries, may not start without it.
    "LD_LIBRARY_PATH", // The locale (and `LC_*`, below).
    "LANG", "LANGUAGE", // The terminal, and pi-tui's detection list.
    "TERM", "COLORTERM", "TERM_PROGRAM", "TERM_PROGRAM_VERSION", "NO_COLOR", "FORCE_COLOR",
    "COLUMNS", "LINES", "TMUX", "STY", "ZELLIJ", "WT_SESSION", "KITTY_WINDOW_ID",
    "ITERM_SESSION_ID", "WEZTERM_PANE", "GHOSTTY_RESOURCES_DIR", "WARP_SESSION_ID",
    "WARP_TERMINAL_SESSION_UUID", "TERMINAL_EMULATOR", "TERMUX_VERSION", "SSH_TTY",
    "SSH_CONNECTION", "SSH_CLIENT", "DISPLAY", "WAYLAND_DISPLAY", // The session.
    "BROWSER", "CI", // The network.
    "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY", "http_proxy", "https_proxy", "all_proxy",
    "no_proxy", "NODE_USE_ENV_PROXY", "NODE_USE_SYSTEM_CA", "NODE_EXTRA_CA_CERTS", "SSL_CERT_FILE",
    "SSL_CERT_DIR", // Standard folders.
    "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME", "XDG_RUNTIME_DIR",
    // Where programs live: those pi-lens reads, and their kin.
    "GOPATH", "GOBIN", "CARGO_HOME", "JAVA_HOME", "JDTLS_PATH", "VIRTUAL_ENV", "CONDA_PREFIX",
    "UV_PROJECT_ENVIRONMENT", "BUN_INSTALL", "GOROOT", "RUSTUP_HOME", "PNPM_HOME", "NVM_DIR",
    "VOLTA_HOME", "DENO_INSTALL",
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
}
