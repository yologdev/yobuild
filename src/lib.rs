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
//! - [`runtime`]: yobuild's own runtime files, installed into the cache.
//! - [`agents`]: `yobuild new` — a starter, or a built-in agent copied.
//! - [`skill`]: `yobuild skill install` — the skill where a coding agent reads it.
//!
//! Not published yet and without an API promise.

pub mod agents;
pub mod builtins;
pub(crate) mod environment;
pub mod frontend;
pub(crate) mod hints;
pub mod install;
pub mod manifest;
pub mod run;
pub mod runtime;
pub mod skill;
