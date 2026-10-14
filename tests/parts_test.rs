//! The parts, each through a real session and a scripted model.

use std::path::Path;
use std::time::Duration;

use serde_json::json;
use yoagent::context::ExecutionLimits;
use yoagent::provider::mock::{MockResponse, MockToolCall};
use yoagent::provider::{MockProvider, ModelConfig};
use yoagent::{
    Agent, AgentMessage, AgentTool, Content, Message, ToolContext, ToolError, ToolResult,
};
use yobuild::frontend::{
    ClientMessage, Connection, NoticeLevel, RunOutcome, ServerMessage, Session, UiRequest,
};
use yobuild::parts::{AskOwner, Facts, Report};

const WAIT: Duration = Duration::from_secs(10);

fn agent(responses: Vec<MockResponse>) -> Agent {
    Agent::from_provider(MockProvider::new(responses), ModelConfig::mock())
}

fn call(name: &str, arguments: serde_json::Value) -> MockToolCall {
    MockToolCall {
        name: name.into(),
        arguments,
        provider_metadata: None,
    }
}

async fn next(connection: &mut Connection) -> ServerMessage {
    tokio::time::timeout(WAIT, connection.messages.recv())
        .await
        .expect("timed out waiting for the session")
        .expect("the session ended")
}

/// Messages up to and including the first `want` matches.
async fn until(
    connection: &mut Connection,
    want: impl Fn(&ServerMessage) -> bool,
) -> Vec<ServerMessage> {
    let mut seen = Vec::new();
    loop {
        let message = next(connection).await;
        let done = want(&message);
        seen.push(message);
        if done {
            return seen;
        }
    }
}

fn is_run_end(m: &ServerMessage) -> bool {
    matches!(m, ServerMessage::RunEnd { .. })
}

/// Runs `agent` in a session that accepts `Quit`; returns the session, a
/// frontend connected before the first run, and the driver's task.
fn drive(
    agent: impl FnOnce(&Session) -> Agent,
) -> (Session, Connection, tokio::task::JoinHandle<Agent>) {
    let (session, driver) = Session::new(true);
    let connection = session.connect();
    let agent = agent(&session);
    (session, connection, tokio::spawn(driver.run(agent)))
}

async fn quit(session: &Session, run: tokio::task::JoinHandle<Agent>) -> Agent {
    session.send(ClientMessage::Quit);
    tokio::time::timeout(WAIT, run).await.unwrap().unwrap()
}

fn texts(agent: &Agent) -> Vec<String> {
    agent
        .messages()
        .iter()
        .filter_map(|m| match m {
            AgentMessage::Llm(Message::User { content, .. })
            | AgentMessage::Llm(Message::Assistant { content, .. }) => Some(
                content
                    .iter()
                    .filter_map(|c| match c {
                        Content::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect()
}

fn notices(seen: &[ServerMessage], level: NoticeLevel) -> Vec<String> {
    seen.iter()
        .filter_map(|m| match m {
            ServerMessage::Notice {
                level: l, message, ..
            } if *l == level => Some(message.clone()),
            _ => None,
        })
        .collect()
}

// -- ask_owner ---------------------------------------------------------------

#[tokio::test]
async fn ask_owner_returns_at_once_and_the_answer_starts_the_next_run() {
    let mut open = None;
    let (session, mut owner, run) = drive(|session| {
        let ask = AskOwner::new(session.clone());
        open = Some(ask.open_questions());
        agent(vec![
            MockResponse::ToolCalls(vec![call(
                "ask_owner",
                json!({ "question": "Which format?", "why": "The notes", "options": ["List", "Prose"] }),
            )]),
            MockResponse::Text("run 1 done".into()),
            MockResponse::Text("run 2 done".into()),
        ])
        .with_tools(vec![Box::new(ask)])
    });
    let open = open.unwrap();
    session.send(ClientMessage::Prompt { text: "go".into() });

    // The question reaches the frontend, and the run ends without an answer.
    let seen = until(&mut owner, is_run_end).await;
    let id = seen
        .iter()
        .find_map(|m| match m {
            ServerMessage::UiRequest {
                id,
                request: UiRequest::Select { title, options, .. },
            } => {
                assert_eq!(title, "#1 Which format?");
                assert_eq!(options, &["List", "Prose"]);
                Some(*id)
            }
            _ => None,
        })
        .expect("the question reached the frontend");
    assert!(matches!(
        seen.last(),
        Some(ServerMessage::RunEnd {
            outcome: RunOutcome::Completed,
            ..
        })
    ));
    assert_eq!(open.count(), 1);

    // The answer starts the next run, as a steer message.
    session.send(ClientMessage::UiResponse {
        id,
        value: json!("List"),
    });
    let seen = until(&mut owner, is_run_end).await;
    let prompt = seen
        .iter()
        .find_map(|m| match m {
            ServerMessage::RunStart { prompt, .. } => Some(prompt.clone()),
            _ => None,
        })
        .expect("the answer started a run");
    assert_eq!(prompt, "[Owner's answer to #1] Which format?\nList");
    assert_eq!(open.count(), 0);
    quit(&session, run).await;
}

#[tokio::test]
async fn an_empty_answer_is_a_skip_and_a_question_without_options_asks_for_words() {
    let (session, mut owner, run) = drive(|session| {
        agent(vec![
            MockResponse::ToolCalls(vec![call("ask_owner", json!({ "question": "Budget?" }))]),
            MockResponse::Text("run 1 done".into()),
            MockResponse::Text("run 2 done".into()),
        ])
        .with_tools(vec![Box::new(AskOwner::new(session.clone()))])
    });
    session.send(ClientMessage::Prompt { text: "go".into() });
    let seen = until(&mut owner, is_run_end).await;
    let id = seen
        .iter()
        .find_map(|m| match m {
            ServerMessage::UiRequest {
                id,
                request: UiRequest::Input { title, .. },
            } if title == "#1 Budget?" => Some(*id),
            _ => None,
        })
        .expect("an input question");
    session.send(ClientMessage::UiResponse {
        id,
        value: json!("  "),
    });
    let seen = until(&mut owner, is_run_end).await;
    assert!(seen.iter().any(
        |m| matches!(m, ServerMessage::RunStart { prompt, .. } if prompt == "[Owner skipped #1] Budget?")
    ));
    quit(&session, run).await;
}

#[tokio::test]
async fn ask_owner_refuses_a_missing_question() {
    let (session, mut owner, run) = drive(|session| {
        agent(vec![
            MockResponse::ToolCalls(vec![call("ask_owner", json!({ "question": " " }))]),
            MockResponse::Text("done".into()),
        ])
        .with_tools(vec![Box::new(AskOwner::new(session.clone()))])
    });
    session.send(ClientMessage::Prompt { text: "go".into() });
    let seen = until(&mut owner, is_run_end).await;
    assert!(!seen
        .iter()
        .any(|m| matches!(m, ServerMessage::UiRequest { .. })));
    let agent = quit(&session, run).await;
    let errors: Vec<_> = agent
        .messages()
        .iter()
        .filter(|m| {
            matches!(
                m,
                AgentMessage::Llm(Message::ToolResult { is_error: true, .. })
            )
        })
        .collect();
    assert_eq!(errors.len(), 1);
}

// -- report ------------------------------------------------------------------

#[tokio::test]
async fn report_sends_the_agent_back_until_the_sections_are_there() {
    let (session, mut owner, run) = drive(|_| {
        agent(vec![
            MockResponse::Text("Finished.".into()),
            MockResponse::Text("## Done\n- it\n## Next\n- nothing".into()),
        ])
        .with_extension(Report::new(["Done", "Next"]))
    });
    session.send(ClientMessage::Prompt { text: "go".into() });
    until(&mut owner, is_run_end).await;
    let agent = quit(&session, run).await;
    let texts = texts(&agent);
    assert!(
        texts
            .iter()
            .any(|t| t
                .contains("End with the report: sections ## Done, ## Next. Missing: Done, Next.")),
        "{texts:?}"
    );
    assert_eq!(texts.last().unwrap(), "## Done\n- it\n## Next\n- nothing");
}

// -- facts -------------------------------------------------------------------

fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
        .status;
    assert!(status.success(), "git {args:?}");
}

#[tokio::test]
async fn facts_count_only_what_changed_since_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path();
    git(dir, &["init", "-q"]);
    std::fs::write(dir.join("tracked.txt"), "one\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "start"]);
    // The owner's own uncommitted edit and untracked file, before the start.
    std::fs::write(dir.join("tracked.txt"), "one\ntwo\n").unwrap();
    std::fs::write(dir.join("theirs.txt"), "x").unwrap();

    let ask_dir = dir.to_owned();
    let (session, mut owner, run) = drive(move |session| {
        let ask = AskOwner::new(session.clone());
        let facts = Facts::new(session.clone(), &ask_dir).with_open_questions(ask.open_questions());
        agent(vec![
            MockResponse::ToolCalls(vec![call("ask_owner", json!({ "question": "Q?" }))]),
            MockResponse::Text("done".into()),
        ])
        .with_tools(vec![Box::new(ask)])
        .with_extension(facts)
    });
    // The agent's work: a new file and one more line.
    std::fs::write(dir.join("notes.md"), "n").unwrap();
    std::fs::write(dir.join("tracked.txt"), "one\ntwo\nthree\n").unwrap();
    session.send(ClientMessage::Prompt { text: "go".into() });
    let seen = until(&mut owner, is_run_end).await;
    let facts = notices(&seen, NoticeLevel::Info);
    assert_eq!(facts.len(), 1, "{seen:?}");
    let facts = &facts[0];
    // Only the agent's line counts (the owner's edit was there at the start),
    // and only notes.md is new (theirs.txt was there too).
    assert!(
        facts.starts_with(
            "Facts: since the start: 1 file changed, 1 insertion(+), 0 commit(s), new: notes.md; "
        ),
        "{facts}"
    );
    assert!(facts.contains("; 1 tool call(s); "), "{facts}");
    assert!(
        facts.ends_with("; 1 question(s) waiting on you."),
        "{facts}"
    );
    quit(&session, run).await;
}

#[tokio::test]
async fn facts_outside_a_repository_say_so() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_owned();
    let (session, mut owner, run) = drive(move |session| {
        agent(vec![MockResponse::Text("done".into())])
            .with_extension(Facts::new(session.clone(), &path))
    });
    session.send(ClientMessage::Prompt { text: "go".into() });
    let seen = until(&mut owner, is_run_end).await;
    let facts = notices(&seen, NoticeLevel::Info);
    assert_eq!(facts.len(), 1);
    assert!(
        facts[0].starts_with("Facts: not a git repository; "),
        "{}",
        facts[0]
    );
    // No question count without an `ask_owner` to count.
    assert!(!facts[0].contains("question"), "{}", facts[0]);
    quit(&session, run).await;
}

/// A tool that only says it ran.
struct Noop;

#[async_trait::async_trait]
impl AgentTool for Noop {
    fn name(&self) -> &str {
        "noop"
    }
    fn label(&self) -> &str {
        "noop"
    }
    fn description(&self) -> &str {
        "does nothing"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({ "type": "object" })
    }
    async fn execute(
        &self,
        _params: serde_json::Value,
        _ctx: ToolContext,
    ) -> Result<ToolResult, ToolError> {
        Ok(ToolResult {
            content: vec![Content::Text { text: "ok".into() }],
            details: json!({}),
        })
    }
}

#[tokio::test]
async fn facts_warn_when_a_limit_stopped_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_owned();
    let (session, mut owner, run) = drive(move |session| {
        agent(vec![
            MockResponse::ToolCalls(vec![call("noop", json!({}))]),
            MockResponse::ToolCalls(vec![call("noop", json!({}))]),
            MockResponse::Text("never".into()),
        ])
        .with_tools(vec![Box::new(Noop)])
        .with_execution_limits(ExecutionLimits::default().with_max_turns(1))
        .with_extension(Facts::new(session.clone(), &path))
    });
    session.send(ClientMessage::Prompt { text: "go".into() });
    let seen = until(&mut owner, is_run_end).await;
    let warnings = notices(&seen, NoticeLevel::Warning);
    assert_eq!(warnings.len(), 1, "{seen:?}");
    assert!(
        warnings[0].starts_with("Stopped early: "),
        "{}",
        warnings[0]
    );
    assert!(warnings[0].ends_with("Send \"continue\" to pick up where it left off."));
    quit(&session, run).await;
}
