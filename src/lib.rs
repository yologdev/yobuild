//! Build an agent by choosing its parts: plugins other ecosystems already
//! publish (pi extensions, DeepSeek Harness plugins, rutis plugins, MCP
//! servers) on the [yoagent](https://docs.rs/yoagent) loop, a few parts of
//! its own, and a frontend.
//!
//! - [`frontend`]: one session for any number of frontends (browser, phone,
//!   terminal), plugins' questions to the owner, and the plugin host.
//!
//! Today it has the frontend; the parts, the manifest and MCP servers are
//! coming (see `docs/roadmap.md`). Not published yet and without an API
//! promise.

pub mod frontend;
