//! `ask_owner`: questions that do not block the run.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::{json, Value as Json};
use yoagent::{AgentTool, Content, ToolContext, ToolError, ToolResult};

use crate::frontend::{ClientMessage, Session, UiRequest};

/// A question for the owner.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Question {
    /// The question, on its own.
    pub question: String,
    /// What depends on it, briefly (may be empty).
    pub why: String,
    /// The choices; with fewer than two the owner answers in words.
    pub options: Vec<String>,
}

/// How many questions are waiting on the owner. Clone it to share the count
/// (with [`Facts`](super::Facts), say).
#[derive(Debug, Clone, Default)]
pub struct OpenQuestions(Arc<AtomicUsize>);

impl OpenQuestions {
    pub fn count(&self) -> usize {
        self.0.load(Ordering::Relaxed)
    }
}

/// The `ask_owner` tool: puts a question to every frontend of the session
/// and returns at once. The question stays open while nobody is there
/// ([`Session::ask_later`]); the answer, whenever it comes, is sent in as a
/// steer message — picked up by the run in progress, or starting a new run
/// when the agent had stopped:
///
/// - `[Owner's answer to #N] <question>\n<answer>`
/// - `[Owner skipped #N] <question>` (an empty answer)
///
/// A question withdrawn by a reset, or still open when the session ends,
/// sends nothing.
#[derive(Clone)]
pub struct AskOwner {
    session: Session,
    next: Arc<AtomicU64>,
    open: OpenQuestions,
}

impl AskOwner {
    pub fn new(session: Session) -> Self {
        Self {
            session,
            next: Arc::default(),
            open: OpenQuestions::default(),
        }
    }

    /// The count of questions not answered yet.
    pub fn open_questions(&self) -> OpenQuestions {
        self.open.clone()
    }

    /// Asks without waiting; returns the question's number (1, 2, …).
    pub fn ask(&self, question: Question) -> u64 {
        let n = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let title = format!("#{n} {}", question.question);
        let request = if question.options.len() >= 2 {
            UiRequest::Select {
                title,
                message: question.why,
                options: question.options,
                multiple: false,
            }
        } else {
            UiRequest::Input {
                title,
                message: question.why,
                placeholder: "Your answer".into(),
                value: String::new(),
            }
        };
        let (session, open, text) = (self.session.clone(), self.open.clone(), question.question);
        open.0.fetch_add(1, Ordering::Relaxed);
        tokio::spawn(async move {
            let answer = session.ask_later(request).await;
            open.0.fetch_sub(1, Ordering::Relaxed);
            // Withdrawn (a reset forgot the conversation it belongs to) or the
            // session ended: nothing to tell the agent.
            let Some(answer) = answer else { return };
            let text = match answer.as_str().map(str::trim) {
                Some(answer) if !answer.is_empty() => {
                    format!("[Owner's answer to #{n}] {text}\n{answer}")
                }
                _ => format!("[Owner skipped #{n}] {text}"),
            };
            // Mid-run it is picked up between tool batches; when the agent
            // had stopped, it starts a new run.
            session.send(ClientMessage::Steer { text });
        });
        n
    }
}

#[async_trait::async_trait]
impl AgentTool for AskOwner {
    fn name(&self) -> &str {
        "ask_owner"
    }
    fn label(&self) -> &str {
        "Ask the owner"
    }
    fn description(&self) -> &str {
        "Ask the owner a question they decide, without waiting: this returns at once. Continue \
         with work that does not depend on the answer; it arrives later as a message. Give \
         `options` when the answer is one of a few choices."
    }
    fn parameters_schema(&self) -> Json {
        json!({
            "type": "object",
            "properties": {
                "question": { "type": "string", "description": "The question, on its own." },
                "why": { "type": "string", "description": "What depends on it, briefly." },
                "options": { "type": "array", "items": { "type": "string" } }
            },
            "required": ["question"]
        })
    }

    async fn execute(&self, params: Json, _ctx: ToolContext) -> Result<ToolResult, ToolError> {
        let question = params["question"]
            .as_str()
            .filter(|q| !q.trim().is_empty())
            .ok_or_else(|| ToolError::InvalidArgs("`question` is required".into()))?
            .to_owned();
        let why = params["why"].as_str().unwrap_or_default().to_owned();
        let options = params["options"]
            .as_array()
            .map(|o| {
                o.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let n = self.ask(Question {
            question,
            why,
            options,
        });
        Ok(ToolResult {
            content: vec![Content::Text {
                text: format!(
                    "Asked the owner as #{n}. Do not wait: continue with what does not depend on \
                     it. The answer will arrive as a message starting with [Owner's answer to #{n}]."
                ),
            }],
            details: json!({ "question": n }),
        })
    }
}
