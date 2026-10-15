//! `ask_owner`: questions that do not block the run.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::{json, Value as Json};
use yoagent::{AgentTool, Content, ToolContext, ToolError, ToolResult};

use crate::frontend::{ClientMessage, Session, UiRequest};

/// A question for the owner: the tool's arguments, and what a host service
/// offering [`AskOwner::ask`] to plugins accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    question: String,
    why: String,
    options: Vec<String>,
}

impl Question {
    /// `question` trimmed and not empty; `why` may be empty. Options are
    /// trimmed, blank ones dropped and repeats removed; with fewer than two
    /// left, the owner answers in words.
    pub fn new(
        question: impl Into<String>,
        why: impl Into<String>,
        options: impl IntoIterator<Item = String>,
    ) -> Result<Self, String> {
        let question = question.into().trim().to_owned();
        if question.is_empty() {
            return Err("`question` is required".into());
        }
        let mut seen = Vec::new();
        for option in options {
            let option = option.trim().to_owned();
            if !option.is_empty() && !seen.contains(&option) {
                seen.push(option);
            }
        }
        Ok(Self {
            question,
            why: why.into().trim().to_owned(),
            options: seen,
        })
    }

    /// From JSON `{question, why?, options?}`; any other shape, or another
    /// key (`choices` for `options`, say), is an error.
    pub fn from_json(value: &Json) -> Result<Self, String> {
        let object = value.as_object().ok_or("the question must be an object")?;
        if let Some(key) = object
            .keys()
            .find(|k| !["question", "why", "options"].contains(&k.as_str()))
        {
            return Err(format!("unknown key `{key}` (use question, why, options)"));
        }
        let question = value["question"]
            .as_str()
            .ok_or("`question` must be a string")?;
        let why = match &value["why"] {
            Json::Null => "",
            Json::String(why) => why,
            _ => return Err("`why` must be a string".into()),
        };
        let options = match &value["options"] {
            Json::Null => Vec::new(),
            Json::Array(options) => options
                .iter()
                .map(|o| o.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
                .ok_or("`options` must be strings")?,
            _ => return Err("`options` must be a list of strings".into()),
        };
        Self::new(question, why, options)
    }

    pub fn question(&self) -> &str {
        &self.question
    }
    pub fn why(&self) -> &str {
        &self.why
    }
    pub fn options(&self) -> &[String] {
        &self.options
    }
}

/// How many questions are waiting on the owner; from
/// [`AskOwner::open_questions`]. Clone it to share the count (with
/// [`Facts`](super::Facts), say).
#[derive(Debug, Clone)]
pub struct OpenQuestions(Arc<AtomicUsize>);

impl OpenQuestions {
    pub fn count(&self) -> usize {
        self.0.load(Ordering::Relaxed)
    }
}

/// Counts a question open until dropped, however its task ends.
struct Open(Arc<AtomicUsize>);

impl Drop for Open {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The `ask_owner` tool: puts a question to every frontend of the session
/// and returns at once. The question stays open while nobody is there
/// ([`Session::ask_later`]); the answer, whenever it comes, is sent in as a
/// steer message — picked up by the run in progress, or starting a new run
/// when the agent had stopped:
///
/// - `[Owner's answer to #N] <question>\n<answer>`
/// - `[Owner skipped #N] <question>` (a blank answer)
///
/// A question withdrawn by a reset, or still open when the session ends,
/// sends nothing.
///
/// Make one per session and clone it: clones share the numbering and the
/// count, so the tool and a host service do not both ask a "#1".
#[derive(Clone)]
pub struct AskOwner {
    session: Session,
    next: Arc<AtomicU64>,
    open: Arc<AtomicUsize>,
}

impl AskOwner {
    pub fn new(session: Session) -> Self {
        Self {
            session,
            next: Arc::default(),
            open: Arc::default(),
        }
    }

    /// The count of questions not answered yet.
    pub fn open_questions(&self) -> OpenQuestions {
        OpenQuestions(self.open.clone())
    }

    /// Asks without waiting; returns the question's number (1, 2, …). Must
    /// be called within a Tokio runtime (the answer is awaited on a task).
    pub fn ask(&self, question: Question) -> u64 {
        let n = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let Question {
            question,
            why,
            options,
        } = question;
        let title = format!("#{n} {question}");
        let request = if options.len() >= 2 {
            UiRequest::Select {
                title,
                message: why,
                options,
                multiple: false,
            }
        } else {
            UiRequest::Input {
                title,
                message: why,
                placeholder: "Your answer".into(),
                value: String::new(),
            }
        };
        self.open.fetch_add(1, Ordering::Relaxed);
        let open = Open(self.open.clone());
        let session = self.session.clone();
        tokio::spawn(async move {
            let answer = session.ask_later(request).await;
            drop(open);
            // Withdrawn (a reset forgot the conversation it belongs to) or the
            // session ended: nothing to tell the agent.
            let Some(answer) = answer else { return };
            let text = match answer.as_str().map(str::trim) {
                Some(answer) if !answer.is_empty() => {
                    format!("[Owner's answer to #{n}] {question}\n{answer}")
                }
                _ => format!("[Owner skipped #{n}] {question}"),
            };
            // Mid-run it is picked up between tool batches; when the agent
            // had stopped, it starts a new run.
            if !session.send(ClientMessage::Steer { text }) {
                tracing::warn!(
                    question = n,
                    "the session closed before the owner's answer could reach the agent"
                );
            }
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
         `options` (two or more) when the answer is one of a few choices."
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
        let question = Question::from_json(&params).map_err(ToolError::InvalidArgs)?;
        let n = self.ask(question);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_question_is_trimmed_and_its_options_cleaned() {
        let q = Question::new(
            "  Which?  ",
            " why ",
            ["A".into(), " ".into(), " A ".into(), "B".into()],
        )
        .unwrap();
        assert_eq!(q.question(), "Which?");
        assert_eq!(q.why(), "why");
        assert_eq!(q.options(), ["A", "B"]);
        assert!(Question::new(" ", "", []).is_err());
    }

    #[test]
    fn from_json_refuses_wrong_shapes() {
        let ok = Question::from_json(&json!({ "question": "Q?" })).unwrap();
        assert_eq!((ok.why(), ok.options().len()), ("", 0));
        for bad in [
            json!({}),
            json!({ "question": 1 }),
            json!({ "question": "Q?", "why": 2 }),
            json!({ "question": "Q?", "options": "A" }),
            json!({ "question": "Q?", "options": ["A", 3] }),
            json!({ "question": "Q?", "choices": ["A", "B"] }),
            json!("Q?"),
        ] {
            assert!(Question::from_json(&bad).is_err(), "{bad}");
        }
    }
}
