//! A scripted call's `expect` (design §11 "pi-lens in the gallery"): what
//! its result must contain, so a reply file checks what a tool answered,
//! not only that it ran. `run/script.rs` carries it in the call's
//! `provider_metadata` under [`KEY`]; this extension, last among a scripted
//! run's, checks each result as it comes back — an error result too —
//! and turns a miss into an error, and says at the run's end each
//! expectation whose call never ran (denied, or no such tool: no
//! `after_tool` then).

use std::sync::{Mutex, PoisonError};

use yoagent::extension::{Extension, ExtensionError, RunContext, RunHooks, RunOutcome, ToolOutput};
use yoagent::{AgentEvent, AgentMessage, Content, Message, ToolCallRequest};

use crate::frontend::{NoticeLevel, Session};

/// The key in a call's `provider_metadata`: a list of texts.
pub(crate) const KEY: &str = "yobuild_expect";

/// The first words of a missed expectation's result (`gallery_test` refuses
/// them).
pub(crate) const MISSED: &str = "the reply file expected `";

/// The extension: one per scripted session.
#[derive(Clone)]
pub(crate) struct Expect {
    session: Session,
}

impl Expect {
    pub(crate) fn new(session: Session) -> Self {
        Self { session }
    }
}

#[async_trait::async_trait]
impl Extension for Expect {
    fn name(&self) -> &str {
        "expect"
    }

    async fn start_run(&self, _run: &RunContext<'_>) -> Result<Box<dyn RunHooks>, ExtensionError> {
        Ok(Box::new(ExpectRun {
            session: self.session.clone(),
            calls: Mutex::default(),
        }))
    }
}

/// One run's state.
struct ExpectRun {
    session: Session,
    calls: Mutex<Calls>,
}

/// The expectations of the latest turn's calls not yet checked, by id (the
/// mock's ids repeat each turn: a new turn moves what is left to `unrun`),
/// and those whose call never ran, by tool.
#[derive(Default)]
struct Calls {
    waiting: Vec<(String, String)>,
    unrun: Vec<String>,
}

impl ExpectRun {
    fn calls(&self) -> std::sync::MutexGuard<'_, Calls> {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The texts `metadata` expects, if it carries [`KEY`].
fn expected(metadata: Option<&serde_json::Value>) -> Option<Vec<String>> {
    let list = metadata?.get(KEY)?.as_array()?;
    Some(
        list.iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
    )
}

/// The tool calls of `message`, if it is the model's: id, name, metadata.
fn tool_calls(message: &AgentMessage) -> Vec<(&str, &str, Option<&serde_json::Value>)> {
    match message {
        AgentMessage::Llm(Message::Assistant { content, .. }) => content
            .iter()
            .filter_map(|c| match c {
                Content::ToolCall {
                    id,
                    name,
                    provider_metadata,
                    ..
                } => Some((id.as_str(), name.as_str(), provider_metadata.as_ref())),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// `output`'s text blocks, joined with line breaks.
fn text_of(output: &ToolOutput) -> String {
    output
        .result
        .content
        .iter()
        .filter_map(|c| match c {
            Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[async_trait::async_trait]
impl RunHooks for ExpectRun {
    fn on_event(&self, event: &AgentEvent) {
        if let AgentEvent::MessageEnd { message } = event {
            let calls = tool_calls(message);
            if calls.is_empty() {
                return;
            }
            let mut state = self.calls();
            let left: Vec<String> = state.waiting.drain(..).map(|(_, tool)| tool).collect();
            state.unrun.extend(left);
            state.waiting = calls
                .into_iter()
                .filter(|(_, _, metadata)| expected(*metadata).is_some())
                .map(|(id, name, _)| (id.to_owned(), name.to_owned()))
                .collect();
        }
    }

    async fn after_tool(
        &self,
        call: &ToolCallRequest<'_>,
        output: &mut ToolOutput,
    ) -> Result<(), ExtensionError> {
        // The call, in the assistant message that carries it (the last).
        let found = call.messages.iter().rev().find_map(|m| {
            tool_calls(m)
                .into_iter()
                .find(|(id, _, _)| *id == call.tool_call_id)
                .map(|(_, _, metadata)| expected(metadata))
        });
        let Some(Some(texts)) = found else {
            return Ok(());
        };
        self.calls()
            .waiting
            .retain(|(id, _)| id != call.tool_call_id);
        let text = text_of(output);
        if let Some(missing) = texts.iter().find(|t| !text.contains(t.as_str())) {
            let first = text
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("(no text)");
            output.result.content.insert(
                0,
                Content::Text {
                    // Its own line, however the blocks are joined.
                    text: format!("{MISSED}{missing}` in the result: {first}\n"),
                },
            );
            output.is_error = true;
        }
        Ok(())
    }

    async fn finish(&mut self, _outcome: &RunOutcome) {
        let state = std::mem::take(&mut *self.calls());
        for tool in state
            .unrun
            .into_iter()
            .chain(state.waiting.into_iter().map(|(_, t)| t))
        {
            self.session.notice(
                NoticeLevel::Warning,
                format!("the reply file's expectation for `{tool}` was not checked: the call did not run"),
            );
        }
    }
}
