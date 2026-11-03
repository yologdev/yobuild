//! Build an agent by choosing its parts: plugins other ecosystems already
//! publish (pi extensions, DeepSeek Harness plugins, rutis plugins, MCP
//! servers) on the [yoagent](https://docs.rs/yoagent) loop, a few built-ins
//! of its own, and a frontend.
//!
//! - [`frontend`]: one session for any number of frontends (browser, phone,
//!   terminal), plugins' questions to the owner, and the plugin host.
//! - [`builtins`]: yobuild's own behaviours, what more than one kind of
//!   agent wants — asking the owner without waiting, a required report,
//!   host-counted facts, the approval gate, a scratch folder.
//!
//! - [`manifest`]: an agent in one TOML file, read strictly.
//! - [`install`]: a manifest's pinned packages, locked and installed.
//! - [`run`]: an agent from its manifest (the `yobuild run` command).
//! - [`run_log`]: each run, logged as it ends (`yobuild log`).
//! - [`runtime`]: yobuild's own runtime files, installed into the cache.
//! - [`sandbox`]: plugins and stdio MCP servers confined, with `[sandbox]`.
//! - [`agents`]: `yobuild new` — a starter, or a built-in agent copied.
//! - [`skill`]: `yobuild skill install` — the skill where a coding agent reads it.
//! - [`sessions`]: each session saved, and resumed with `yobuild run --resume`.
//!
//! The library is published with the binary but has no API promise, at
//! 1.0 or after: any release may change it. What is stable from 1.0 is
//! the `yobuild` command, `agent.toml` and `yobuild check --json`'s
//! report (versioned by its `version`).

pub mod agents;
pub mod builtins;
pub(crate) mod environment;
pub mod frontend;
pub(crate) mod hints;
pub mod install;
pub mod manifest;
pub mod run;
pub mod run_log;
pub mod runtime;
pub mod sandbox;
pub mod sessions;
pub mod skill;
#[doc(hidden)]
pub mod stops;
