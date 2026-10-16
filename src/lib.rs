//! Build an agent by choosing its parts: plugins other ecosystems already
//! publish (pi extensions, DeepSeek Harness plugins, rutis plugins, MCP
//! servers) on the [yoagent](https://docs.rs/yoagent) loop, a few parts of
//! its own, and a frontend.
//!
//! - [`frontend`]: one session for any number of frontends (browser, phone,
//!   terminal), plugins' questions to the owner, and the plugin host.
//! - [`parts`]: what more than one kind of agent wants — asking the owner
//!   without waiting, a required report, host-counted facts, a scratch
//!   folder.
//!
//! - [`manifest`]: an agent in one TOML file, read strictly.
//!
//! `yobuild run` and MCP servers are coming (see `docs/roadmap.md`). Not
//! published yet and without an API promise.

pub mod frontend;
pub mod manifest;
pub mod parts;
