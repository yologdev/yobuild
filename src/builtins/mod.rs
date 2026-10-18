//! The built-ins: yobuild's own behaviours (`[builtins]` in a manifest), small
//! and generic — what more than one kind of agent wants, with no opinion
//! about the task (design §3). Not plugins moved into Rust: a capability an
//! ecosystem already has stays a plugin. Each plugs into yoagent's
//! loop on its own — a tool or an [`Extension`](yoagent::extension::Extension)
//! — so it applies to every tool the agent has, whichever ecosystem the
//! plugin behind it comes from.
//!
//! - [`AskOwner`]: a tool that asks the owner without waiting; the answer
//!   comes back as a steer message.
//! - [`Report`]: sends the agent back until its last message has the
//!   required sections.
//! - [`Facts`]: after each run, tells the frontends what the host counted —
//!   changes since the start, new files, tokens, tool calls, open questions.
//! - [`Gate`]: an allow-list — asks the owner before a call does anything
//!   it does not know to be harmless (deleting, discarding or overwriting
//!   what they have above all); refuses it when nobody answers.
//! - [`Scratch`]: a private working folder, named to the agent and the gate.
//!
//! `AskOwner`'s question itself ([`AskOwner::ask`], with a validated
//! [`Question`]) is separate from the tool, so a host service can offer it to
//! plugins too. `Report`, `Facts` and `Gate` are the loop's checks on the
//! agent, and stay out of plugins' reach.

mod ask_owner;
mod facts;
mod gate;
mod report;
mod scratch;

pub use ask_owner::{AskOwner, OpenQuestions, Question};
pub use facts::Facts;
pub use gate::{Check, Gate, OfferedCheck, JUDGED_TOOLS, READ_ONLY_TOOLS};
pub use report::Report;
pub use scratch::Scratch;
