//! The parts, each through a real session and a scripted model.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::sync::Notify;
use yoagent::context::ExecutionLimits;
use yoagent::provider::mock::{MockResponse, MockToolCall};
use yoagent::provider::{MockProvider, ModelConfig};
use yoagent::{
    Agent, AgentMessage, AgentTool, Content, Message, ToolContext, ToolError, ToolResult, Usage,
};
use yobuild::frontend::{
    ClientMessage, Connection, NoticeLevel, ResolveReason, RunOutcome, ServerMessage, Session,
    UiRequest,
};
use yobuild::parts::{AskOwner, Facts, OpenQuestions, Report};

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

fn calls(names: &[&str]) -> MockResponse {
    MockResponse::ToolCalls(names.iter().map(|n| call(n, json!({}))).collect())
}

async fn next(connection: &mut Connection) -> ServerMessage {
    tokio::time::timeout(WAIT, connection.messages.recv())
        .await
        .expect("timed out waiting for the session")
        .expect("the session ended")
}

type Want = fn(&ServerMessage) -> bool;

/// Messages until each of `wants` has matched at least once, in any order:
/// a question is broadcast from its own task, so it may come after the end
/// of the run that asked it.
async fn until_all(connection: &mut Connection, wants: &[Want]) -> Vec<ServerMessage> {
    let mut seen = Vec::new();
    while !wants.iter().all(|want| seen.iter().any(want)) {
        seen.push(next(connection).await);
    }
    seen
}

async fn until_run_end(connection: &mut Connection) -> Vec<ServerMessage> {
    until_all(connection, &[is_run_end]).await
}

fn is_run_end(m: &ServerMessage) -> bool {
    matches!(m, ServerMessage::RunEnd { .. })
}

fn is_question(m: &ServerMessage) -> bool {
    matches!(m, ServerMessage::UiRequest { .. })
}

fn questions(seen: &[ServerMessage]) -> Vec<(u64, UiRequest)> {
    seen.iter()
        .filter_map(|m| match m {
            ServerMessage::UiRequest { id, request } => Some((*id, request.clone())),
            _ => None,
        })
        .collect()
}

fn prompts(seen: &[ServerMessage]) -> Vec<String> {
    seen.iter()
        .filter_map(|m| match m {
            ServerMessage::RunStart { prompt, .. } => Some(prompt.clone()),
            _ => None,
        })
        .collect()
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

/// An `ask_owner` agent with `responses`; also returns its count.
fn asking(responses: Vec<MockResponse>) -> impl FnOnce(&Session) -> (Agent, OpenQuestions) {
    move |session| {
        let ask = AskOwner::new(session.clone());
        let open = ask.open_questions();
        (agent(responses).with_tools(vec![Box::new(ask)]), open)
    }
}

fn drive_asking(
    responses: Vec<MockResponse>,
) -> (
    Session,
    Connection,
    tokio::task::JoinHandle<Agent>,
    OpenQuestions,
) {
    let mut open = None;
    let make = asking(responses);
    let (session, connection, run) = drive(|session| {
        let (agent, count) = make(session);
        open = Some(count);
        agent
    });
    (session, connection, run, open.unwrap())
}

async fn quit(session: &Session, run: tokio::task::JoinHandle<Agent>) -> Agent {
    session.send(ClientMessage::Quit);
    tokio::time::timeout(WAIT, run).await.unwrap().unwrap()
}

/// Waits (briefly) until the count reaches `n`: answers are counted off on
/// the question's own task.
async fn count_reaches(open: &OpenQuestions, n: usize) {
    tokio::time::timeout(WAIT, async {
        while open.count() != n {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("open questions stayed at {}, not {n}", open.count()));
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

/// A tool `work` that does what the test gives it, then says it ran.
struct Work(Box<dyn Fn() + Send + Sync>);

#[async_trait::async_trait]
impl AgentTool for Work {
    fn name(&self) -> &str {
        "work"
    }
    fn label(&self) -> &str {
        "work"
    }
    fn description(&self) -> &str {
        "does the test's work"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({ "type": "object" })
    }
    async fn execute(
        &self,
        _params: serde_json::Value,
        _ctx: ToolContext,
    ) -> Result<ToolResult, ToolError> {
        (self.0)();
        Ok(ToolResult {
            content: vec![Content::Text { text: "ok".into() }],
            details: json!({}),
        })
    }
}

/// A tool `wait` that runs until the test notifies it.
struct Wait(Arc<Notify>);

#[async_trait::async_trait]
impl AgentTool for Wait {
    fn name(&self) -> &str {
        "wait"
    }
    fn label(&self) -> &str {
        "wait"
    }
    fn description(&self) -> &str {
        "waits for the test"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({ "type": "object" })
    }
    async fn execute(
        &self,
        _params: serde_json::Value,
        _ctx: ToolContext,
    ) -> Result<ToolResult, ToolError> {
        self.0.notified().await;
        Ok(ToolResult {
            content: vec![Content::Text {
                text: "waited".into(),
            }],
            details: json!({}),
        })
    }
}

// -- ask_owner ---------------------------------------------------------------

#[tokio::test]
async fn ask_owner_returns_at_once_and_the_answer_starts_the_next_run() {
    let (session, mut owner, run, open) = drive_asking(vec![
        MockResponse::ToolCalls(vec![call(
            "ask_owner",
            json!({ "question": "Which format?", "why": "The notes", "options": ["List", "Prose"] }),
        )]),
        MockResponse::Text("run 1 done".into()),
        MockResponse::Text("run 2 done".into()),
    ]);
    session.send(ClientMessage::Prompt { text: "go".into() });

    // The question reaches the frontend, and the run ends without an answer.
    let seen = until_all(&mut owner, &[is_run_end, is_question]).await;
    let [(id, UiRequest::Select { title, options, .. })] = &questions(&seen)[..] else {
        panic!("one select question: {seen:?}");
    };
    assert_eq!(title, "#1 Which format?");
    assert_eq!(options, &["List", "Prose"]);
    assert!(seen.iter().any(|m| matches!(
        m,
        ServerMessage::RunEnd {
            outcome: RunOutcome::Completed,
            ..
        }
    )));
    assert_eq!(open.count(), 1);

    // The answer starts the next run, as a steer message.
    session.send(ClientMessage::UiResponse {
        id: *id,
        value: json!("List"),
    });
    let seen = until_run_end(&mut owner).await;
    assert_eq!(
        prompts(&seen),
        ["[Owner's answer to #1] Which format?\nList"]
    );
    assert_eq!(open.count(), 0);
    quit(&session, run).await;
}

#[tokio::test]
async fn a_blank_answer_is_a_skip_and_a_question_without_options_asks_for_words() {
    let (session, mut owner, run, _) = drive_asking(vec![
        MockResponse::ToolCalls(vec![call(
            "ask_owner",
            json!({ "question": "Budget?", "options": ["only one"] }),
        )]),
        MockResponse::Text("run 1 done".into()),
        MockResponse::Text("run 2 done".into()),
    ]);
    session.send(ClientMessage::Prompt { text: "go".into() });
    let seen = until_all(&mut owner, &[is_run_end, is_question]).await;
    // One option is no choice: the owner answers in words.
    let [(id, UiRequest::Input { title, .. })] = &questions(&seen)[..] else {
        panic!("one input question: {seen:?}");
    };
    assert_eq!(title, "#1 Budget?");
    session.send(ClientMessage::UiResponse {
        id: *id,
        value: json!("  "),
    });
    let seen = until_run_end(&mut owner).await;
    assert_eq!(prompts(&seen), ["[Owner skipped #1] Budget?"]);
    quit(&session, run).await;
}

#[tokio::test]
async fn questions_are_numbered_and_each_answer_names_its_own() {
    let (session, mut owner, run, open) = drive_asking(vec![
        MockResponse::ToolCalls(vec![
            call("ask_owner", json!({ "question": "First?" })),
            call("ask_owner", json!({ "question": "Second?" })),
        ]),
        MockResponse::Text("asked".into()),
        MockResponse::Text("got #2".into()),
    ]);
    session.send(ClientMessage::Prompt { text: "go".into() });
    let mut seen = until_all(&mut owner, &[is_run_end, is_question]).await;
    while questions(&seen).len() < 2 {
        seen.push(next(&mut owner).await);
    }
    let asked = questions(&seen);
    let second = asked
        .iter()
        .find(|(_, r)| r.title() == "#2 Second?")
        .expect("#2");
    assert!(asked.iter().any(|(_, r)| r.title() == "#1 First?"));
    assert_eq!(open.count(), 2);
    session.send(ClientMessage::UiResponse {
        id: second.0,
        value: json!("yes"),
    });
    let seen = until_run_end(&mut owner).await;
    assert_eq!(prompts(&seen), ["[Owner's answer to #2] Second?\nyes"]);
    assert_eq!(open.count(), 1);
    quit(&session, run).await;
}

#[tokio::test]
async fn an_answer_mid_run_is_picked_up_by_that_run() {
    let gate = Arc::new(Notify::new());
    let mut open = None;
    let wait = Wait(gate.clone());
    let (session, mut owner, run) = drive(|session| {
        let ask = AskOwner::new(session.clone());
        open = Some(ask.open_questions());
        agent(vec![
            MockResponse::ToolCalls(vec![call("ask_owner", json!({ "question": "Colour?" }))]),
            calls(&["wait"]),
            MockResponse::Text("saw it".into()),
        ])
        .with_tools(vec![Box::new(ask), Box::new(wait)])
    });
    let open = open.unwrap();
    session.send(ClientMessage::Prompt { text: "go".into() });
    let seen = until_all(&mut owner, &[is_question]).await;
    let (id, _) = questions(&seen)[0].clone();
    session.send(ClientMessage::UiResponse {
        id,
        value: json!("blue"),
    });
    count_reaches(&open, 0).await;
    gate.notify_one();
    let seen = until_run_end(&mut owner).await;
    assert!(prompts(&seen).is_empty(), "no new run: {seen:?}");
    let agent = quit(&session, run).await;
    let texts = texts(&agent);
    let answer = texts
        .iter()
        .position(|t| t == "[Owner's answer to #1] Colour?\nblue")
        .unwrap_or_else(|| panic!("the answer is in the run: {texts:?}"));
    assert_eq!(texts.last().unwrap(), "saw it");
    assert!(answer < texts.len() - 1);
}

#[tokio::test]
async fn a_reset_withdraws_the_question_and_its_answer_goes_nowhere() {
    let (session, mut owner, run, open) = drive_asking(vec![
        MockResponse::ToolCalls(vec![call("ask_owner", json!({ "question": "Keep?" }))]),
        MockResponse::Text("run 1 done".into()),
        MockResponse::Text("never".into()),
    ]);
    session.send(ClientMessage::Prompt { text: "go".into() });
    let seen = until_all(&mut owner, &[is_run_end, is_question]).await;
    let (id, _) = questions(&seen)[0].clone();
    session.send(ClientMessage::Reset);
    let seen = until_all(
        &mut owner,
        &[|m| {
            matches!(
                m,
                ServerMessage::UiResolved {
                    reason: ResolveReason::Withdrawn,
                    ..
                }
            )
        }],
    )
    .await;
    assert!(prompts(&seen).is_empty());
    count_reaches(&open, 0).await;
    // A late answer to the withdrawn question starts nothing.
    session.send(ClientMessage::UiResponse {
        id,
        value: json!("yes"),
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(300), owner.messages.recv())
            .await
            .map_or(true, |m| !matches!(m, Some(ServerMessage::RunStart { .. }))),
        "the withdrawn answer started a run"
    );
    let agent = quit(&session, run).await;
    assert!(!texts(&agent).iter().any(|t| t.contains("[Owner")));
}

#[tokio::test]
async fn a_question_open_when_the_session_ends_sends_nothing() {
    let (session, mut owner, run, open) = drive_asking(vec![
        MockResponse::ToolCalls(vec![call("ask_owner", json!({ "question": "Later?" }))]),
        MockResponse::Text("run 1 done".into()),
    ]);
    session.send(ClientMessage::Prompt { text: "go".into() });
    until_all(&mut owner, &[is_run_end, is_question]).await;
    let agent = quit(&session, run).await;
    count_reaches(&open, 0).await;
    assert!(!texts(&agent).iter().any(|t| t.contains("[Owner")));
}

#[tokio::test]
async fn ask_owner_refuses_a_question_it_cannot_ask() {
    let (session, mut owner, run, open) = drive_asking(vec![
        MockResponse::ToolCalls(vec![
            call("ask_owner", json!({ "question": " " })),
            call(
                "ask_owner",
                json!({ "question": "Q?", "options": ["A", 1] }),
            ),
        ]),
        MockResponse::Text("done".into()),
    ]);
    session.send(ClientMessage::Prompt { text: "go".into() });
    let seen = until_run_end(&mut owner).await;
    assert!(questions(&seen).is_empty());
    assert_eq!(open.count(), 0);
    let agent = quit(&session, run).await;
    let errors: Vec<String> = agent
        .messages()
        .iter()
        .filter_map(|m| match m {
            AgentMessage::Llm(Message::ToolResult {
                is_error: true,
                content,
                ..
            }) => Some(format!("{content:?}")),
            _ => None,
        })
        .collect();
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(
        errors[0].contains("`question` is required"),
        "{}",
        errors[0]
    );
    assert!(
        errors[1].contains("`options` must be strings"),
        "{}",
        errors[1]
    );
}

// -- report ------------------------------------------------------------------

#[tokio::test]
async fn report_sends_the_agent_back_until_the_sections_are_there() {
    let (session, mut owner, run) = drive(|_| {
        agent(vec![
            MockResponse::Text("Finished.".into()),
            MockResponse::Text("## Done\n- it\n## Next\n- nothing".into()),
        ])
        .with_extension(Report::new(["Done", "Next"]).unwrap())
    });
    session.send(ClientMessage::Prompt { text: "go".into() });
    let seen = until_run_end(&mut owner).await;
    // Sent back within the same run, which then completes.
    assert_eq!(prompts(&seen).len(), 1);
    assert!(matches!(
        seen.last(),
        Some(ServerMessage::RunEnd {
            outcome: RunOutcome::Completed,
            ..
        })
    ));
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

#[tokio::test]
async fn a_run_that_never_reports_fails_visibly() {
    let (session, mut owner, run) = drive(|_| {
        agent(
            (0..6)
                .map(|_| MockResponse::Text("Finished.".into()))
                .collect(),
        )
        .with_extension(Report::new(["Done"]).unwrap())
    });
    session.send(ClientMessage::Prompt { text: "go".into() });
    let seen = until_run_end(&mut owner).await;
    match seen.last() {
        Some(ServerMessage::RunEnd {
            outcome: RunOutcome::Error,
            error: Some(error),
            ..
        }) => assert!(error.contains("not accepted"), "{error}"),
        other => panic!("the run should fail: {other:?}"),
    }
    quit(&session, run).await;
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

/// A repository with one commit (`tracked.txt`), the owner's uncommitted
/// edit to it and an untracked file of theirs.
fn owners_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    git(path, &["init", "-q"]);
    std::fs::write(path.join("tracked.txt"), "one\n").unwrap();
    git(path, &["add", "."]);
    git(path, &["commit", "-q", "-m", "start"]);
    std::fs::write(path.join("tracked.txt"), "one\ntwo\n").unwrap();
    std::fs::write(path.join("theirs.txt"), "x").unwrap();
    dir
}

/// Runs one prompt with `Facts` on `dir` and a `work` tool doing `work`;
/// returns the run's facts and warnings.
async fn facts_of(
    dir: &Path,
    responses: Vec<MockResponse>,
    work: impl Fn() + Send + Sync + 'static,
) -> (Vec<String>, Vec<String>) {
    let path = dir.to_owned();
    let (session, mut owner, run) = drive(move |session| {
        agent(responses)
            .with_tools(vec![Box::new(Work(Box::new(work)))])
            .with_extension(Facts::new(session.clone(), &path))
    });
    session.send(ClientMessage::Prompt { text: "go".into() });
    let seen = until_run_end(&mut owner).await;
    quit(&session, run).await;
    (
        notices(&seen, NoticeLevel::Info),
        notices(&seen, NoticeLevel::Warning),
    )
}

fn one(facts: Vec<String>) -> String {
    assert_eq!(facts.len(), 1, "{facts:?}");
    facts.into_iter().next().unwrap()
}

#[tokio::test]
async fn facts_count_only_what_changed_since_the_start() {
    let dir = owners_repo();
    let path = dir.path().to_owned();
    let mut ask_open = None;
    let (session, mut owner, run) = drive(|session| {
        let ask = AskOwner::new(session.clone());
        ask_open = Some(ask.open_questions());
        let facts = Facts::new(session.clone(), &path).with_open_questions(ask.open_questions());
        let p = path.clone();
        let work = Work(Box::new(move || {
            // The agent's work: a new file and one more line.
            std::fs::write(p.join("notes.md"), "n").unwrap();
            std::fs::write(p.join("tracked.txt"), "one\ntwo\nthree\n").unwrap();
        }));
        agent(vec![
            MockResponse::ToolCallsWithUsage(
                vec![
                    call("ask_owner", json!({ "question": "Q?" })),
                    call("work", json!({})),
                ],
                Usage {
                    input: 100,
                    cache_read: 20,
                    cache_write: 5,
                    output: 7,
                    ..Usage::default()
                },
            ),
            MockResponse::TextWithUsage(
                "done".into(),
                Usage {
                    input: 10,
                    output: 3,
                    ..Usage::default()
                },
            ),
        ])
        .with_tools(vec![Box::new(ask), Box::new(work)])
        .with_extension(facts)
    });
    session.send(ClientMessage::Prompt { text: "go".into() });
    let seen = until_run_end(&mut owner).await;
    let facts = one(notices(&seen, NoticeLevel::Info));
    // Only the agent's line counts (the owner's edit was there at the start),
    // only notes.md is new (theirs.txt was there too); every kind of input
    // token counts.
    assert!(
        facts.starts_with(
            "Facts: since the start: 1 file changed, 1 insertion(+), 0 commit(s), new: notes.md; \
             135 tokens in, 10 out; 2 tool call(s); "
        ),
        "{facts}"
    );
    assert!(
        facts.ends_with("; 1 question(s) waiting on you."),
        "{facts}"
    );
    quit(&session, run).await;
}

#[tokio::test]
async fn facts_count_the_agents_commits() {
    let dir = owners_repo();
    let p = dir.path().to_owned();
    let (facts, _) = facts_of(
        dir.path(),
        vec![calls(&["work"]), MockResponse::Text("done".into())],
        move || {
            std::fs::write(p.join("a.txt"), "a\n").unwrap();
            git(&p, &["add", "a.txt"]);
            git(&p, &["commit", "-q", "-m", "agent"]);
        },
    )
    .await;
    let facts = one(facts);
    assert!(
        facts.starts_with("Facts: since the start: 1 file changed, 1 insertion(+), 1 commit(s); "),
        "{facts}"
    );
}

#[tokio::test]
async fn facts_in_a_repository_with_no_commit_yet() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    // The owner staged a file before the start: not the agent's.
    std::fs::write(dir.path().join("theirs.txt"), "x\n").unwrap();
    git(dir.path(), &["add", "theirs.txt"]);
    let p = dir.path().to_owned();
    let (facts, _) = facts_of(
        dir.path(),
        vec![calls(&["work"]), MockResponse::Text("done".into())],
        move || std::fs::write(p.join("a.txt"), "a").unwrap(),
    )
    .await;
    let facts = one(facts);
    assert!(
        facts.starts_with("Facts: since the start: no changes, 0 commit(s), new: a.txt; "),
        "{facts}"
    );
}

#[tokio::test]
async fn facts_name_three_new_files_and_count_the_rest() {
    let dir = owners_repo();
    let p = dir.path().to_owned();
    let (facts, _) = facts_of(
        dir.path(),
        vec![calls(&["work"]), MockResponse::Text("done".into())],
        move || {
            for name in ["d.txt", "c.txt", "b.txt", "a.txt"] {
                std::fs::write(p.join(name), "x").unwrap();
            }
        },
    )
    .await;
    let facts = one(facts);
    assert!(
        facts.contains(", 4 new files: a.txt, b.txt, c.txt, …; "),
        "{facts}"
    );
}

#[tokio::test]
async fn facts_count_each_run_on_its_own() {
    let dir = owners_repo();
    let path = dir.path().to_owned();
    let p = path.clone();
    let (session, mut owner, run) = drive(move |session| {
        agent(vec![
            calls(&["work"]),
            MockResponse::Text("run 1".into()),
            MockResponse::Text("run 2".into()),
        ])
        .with_tools(vec![Box::new(Work(Box::new(move || {
            std::fs::write(p.join("notes.md"), "n").unwrap()
        })))])
        .with_extension(Facts::new(session.clone(), &path))
    });
    session.send(ClientMessage::Prompt { text: "one".into() });
    let first = one(notices(&until_run_end(&mut owner).await, NoticeLevel::Info));
    session.send(ClientMessage::Prompt { text: "two".into() });
    let second = one(notices(&until_run_end(&mut owner).await, NoticeLevel::Info));
    quit(&session, run).await;
    assert!(first.contains("; 1 tool call(s); "), "{first}");
    // The counts are per run; the changes are since the start.
    assert!(second.contains("; 0 tool call(s); "), "{second}");
    assert!(second.contains(", new: notes.md; "), "{second}");
}

#[tokio::test]
async fn facts_outside_a_repository_say_so() {
    let dir = tempfile::tempdir().unwrap();
    let (facts, _) = facts_of(dir.path(), vec![MockResponse::Text("done".into())], || {}).await;
    let facts = one(facts);
    assert!(
        facts.starts_with("Facts: not a git repository; "),
        "{facts}"
    );
    // No question count without an `ask_owner` to count.
    assert!(!facts.contains("question"), "{facts}");
}

#[tokio::test]
async fn facts_say_when_the_repository_went_missing() {
    let dir = owners_repo();
    let p: PathBuf = dir.path().to_owned();
    let (facts, _) = facts_of(
        dir.path(),
        vec![calls(&["work"]), MockResponse::Text("done".into())],
        move || std::fs::remove_dir_all(p.join(".git")).unwrap(),
    )
    .await;
    let facts = one(facts);
    // Never "no changes" when git cannot tell.
    assert!(
        facts.starts_with("Facts: changes since the start unknown (git: "),
        "{facts}"
    );
}

#[tokio::test]
async fn facts_warn_when_a_limit_stopped_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_owned();
    let (session, mut owner, run) = drive(move |session| {
        agent(vec![
            calls(&["work"]),
            calls(&["work"]),
            MockResponse::Text("never".into()),
        ])
        .with_tools(vec![Box::new(Work(Box::new(|| {})))])
        .with_execution_limits(ExecutionLimits::default().with_max_turns(1))
        .with_extension(Facts::new(session.clone(), &path))
    });
    session.send(ClientMessage::Prompt { text: "go".into() });
    let seen = until_run_end(&mut owner).await;
    let warning = one(notices(&seen, NoticeLevel::Warning));
    assert!(warning.starts_with("Stopped early: "), "{warning}");
    assert!(warning.ends_with("Send \"continue\" to pick up where it left off."));
    // The facts are still told.
    one(notices(&seen, NoticeLevel::Info));
    quit(&session, run).await;
}
