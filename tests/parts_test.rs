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
/// What a closed session would do with a late steer cannot be seen from
/// here; this checks the question is counted off.
async fn a_question_open_when_the_session_ends_is_counted_off() {
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
        }) => {
            assert!(error.starts_with("[Extension failed: report]"), "{error}");
            assert!(error.ends_with("after 3 continues"), "{error}");
        }
        other => panic!("the run should fail: {other:?}"),
    }
    let agent = quit(&session, run).await;
    let sent_back = texts(&agent)
        .iter()
        .filter(|t| t.contains("End with the report: sections ## Done. Missing: Done."))
        .count();
    assert_eq!(sent_back, 3);
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
            "Facts: since the start: 1 file changed, 1 insertion(+), 0 new commit(s), new: notes.md; \
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
        facts.starts_with(
            "Facts: since the start: 1 file changed, 1 insertion(+), 1 new commit(s); "
        ),
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
        facts.starts_with("Facts: since the start: no changes, 0 new commit(s), new: a.txt; "),
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
        facts.contains(", 4 new: a.txt, b.txt, c.txt, …; "),
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

#[tokio::test]
async fn a_dismissed_question_is_a_skip() {
    let (session, mut owner, run, _) = drive_asking(vec![
        MockResponse::ToolCalls(vec![call("ask_owner", json!({ "question": "Colour?" }))]),
        MockResponse::Text("run 1 done".into()),
        MockResponse::Text("run 2 done".into()),
    ]);
    session.send(ClientMessage::Prompt { text: "go".into() });
    let seen = until_all(&mut owner, &[is_run_end, is_question]).await;
    let (id, _) = questions(&seen)[0].clone();
    session.send(ClientMessage::UiResponse {
        id,
        value: json!(null),
    });
    let seen = until_run_end(&mut owner).await;
    assert_eq!(prompts(&seen), ["[Owner skipped #1] Colour?"]);
    quit(&session, run).await;
}

/// The owner's edits made after `Facts` is set up but before the first
/// prompt are theirs: the snapshot is taken when the first run starts.
#[tokio::test]
async fn facts_take_the_snapshot_when_the_first_run_starts() {
    let dir = owners_repo();
    let path = dir.path().to_owned();
    let (session, mut owner, run) = drive(|session| {
        agent(vec![MockResponse::Text("done".into())])
            .with_extension(Facts::new(session.clone(), &path))
    });
    std::fs::write(dir.path().join("tracked.txt"), "rewritten\n").unwrap();
    std::fs::write(dir.path().join("owner2.txt"), "x").unwrap();
    session.send(ClientMessage::Prompt { text: "go".into() });
    let facts = one(notices(&until_run_end(&mut owner).await, NoticeLevel::Info));
    quit(&session, run).await;
    assert!(
        facts.starts_with("Facts: since the start: no changes, 0 new commit(s); "),
        "{facts}"
    );
}

#[tokio::test]
async fn facts_without_a_snapshot_say_changes_unknown() {
    let dir = tempfile::tempdir().unwrap();
    // A folder that does not exist: git cannot even start there.
    let (facts, _) = facts_of(
        &dir.path().join("missing"),
        vec![MockResponse::Text("done".into())],
        || {},
    )
    .await;
    let facts = one(facts);
    assert!(
        facts.starts_with("Facts: changes unknown: no snapshot at the start (git: "),
        "{facts}"
    );
    assert!(!facts.contains("not a git repository"), "{facts}");
}

#[tokio::test]
async fn facts_say_when_edits_already_there_could_not_be_told_apart() {
    // No commit yet, a staged file edited again: the edit on top of what is
    // staged cannot be stored.
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    std::fs::write(dir.path().join("theirs.txt"), "x\n").unwrap();
    git(dir.path(), &["add", "theirs.txt"]);
    std::fs::write(dir.path().join("theirs.txt"), "x\ny\n").unwrap();
    let (facts, _) = facts_of(dir.path(), vec![MockResponse::Text("done".into())], || {}).await;
    let facts = one(facts);
    assert!(
        facts.contains("(the edits already there could not be told apart)"),
        "{facts}"
    );
}

#[tokio::test]
async fn facts_do_not_count_a_branch_switch_as_the_agents_commits() {
    let dir = owners_repo();
    let p = dir.path().to_owned();
    // A branch with two commits of the owner's.
    git(&p, &["stash", "-q"]);
    git(&p, &["checkout", "-q", "-b", "other"]);
    for n in ["1", "2"] {
        std::fs::write(p.join(format!("other{n}.txt")), n).unwrap();
        git(&p, &["add", "."]);
        git(&p, &["commit", "-q", "-m", n]);
    }
    git(&p, &["checkout", "-q", "-"]);
    let (facts, _) = facts_of(
        dir.path(),
        vec![calls(&["work"]), MockResponse::Text("done".into())],
        move || git(&p, &["checkout", "-q", "other"]),
    )
    .await;
    let facts = one(facts);
    assert!(facts.contains(", 0 new commit(s)"), "{facts}");
}

#[tokio::test]
async fn facts_say_when_commits_left_head() {
    let dir = owners_repo();
    let p = dir.path().to_owned();
    std::fs::write(p.join("second.txt"), "2").unwrap();
    git(&p, &["add", "second.txt"]);
    git(&p, &["commit", "-q", "-m", "second"]);
    let (facts, _) = facts_of(
        dir.path(),
        vec![calls(&["work"]), MockResponse::Text("done".into())],
        move || git(&p, &["reset", "-q", "--soft", "HEAD~1"]),
    )
    .await;
    let facts = one(facts);
    assert!(
        facts.contains(", 0 new commit(s), 1 commit(s) no longer in HEAD"),
        "{facts}"
    );
}

#[tokio::test]
async fn facts_name_untracked_files_of_the_owners_the_agent_changed_or_removed() {
    let dir = owners_repo();
    std::fs::write(dir.path().join("gone.txt"), "x").unwrap();
    let p = dir.path().to_owned();
    let (facts, _) = facts_of(
        dir.path(),
        vec![calls(&["work"]), MockResponse::Text("done".into())],
        move || {
            std::fs::write(p.join("theirs.txt"), "changed, longer").unwrap();
            std::fs::remove_file(p.join("gone.txt")).unwrap();
        },
    )
    .await;
    let facts = one(facts);
    assert!(facts.contains(", changed untracked: theirs.txt"), "{facts}");
    assert!(facts.contains(", removed untracked: gone.txt"), "{facts}");
}

#[tokio::test]
async fn facts_cover_the_whole_repository_from_a_subfolder() {
    let dir = owners_repo();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    let p = dir.path().to_owned();
    let (facts, _) = facts_of(
        &dir.path().join("sub"),
        vec![calls(&["work"]), MockResponse::Text("done".into())],
        move || std::fs::write(p.join("outside.txt"), "x").unwrap(),
    )
    .await;
    let facts = one(facts);
    assert!(facts.contains(", new: outside.txt"), "{facts}");
}

#[tokio::test]
async fn facts_never_write_the_owners_index() {
    let dir = owners_repo();
    let index = dir.path().join(".git/index");
    let before = std::fs::read(&index).unwrap();
    let modified = std::fs::metadata(&index).unwrap().modified().unwrap();
    // Wait past the file system's time resolution, so a rewrite would show.
    std::thread::sleep(Duration::from_millis(1100));
    let (facts, _) = facts_of(dir.path(), vec![MockResponse::Text("done".into())], || {}).await;
    one(facts);
    assert_eq!(std::fs::read(&index).unwrap(), before);
    assert_eq!(
        std::fs::metadata(&index).unwrap().modified().unwrap(),
        modified
    );
    assert!(!dir.path().join(".git/index.lock").exists());
}

// -- gate --------------------------------------------------------------------

/// A stand-in for `bash` that only records what it was asked to run.
struct FakeBash(Arc<std::sync::Mutex<Vec<String>>>);

#[async_trait::async_trait]
impl AgentTool for FakeBash {
    fn name(&self) -> &str {
        "bash"
    }
    fn label(&self) -> &str {
        "bash"
    }
    fn description(&self) -> &str {
        "records the command"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({ "type": "object", "properties": { "command": { "type": "string" } } })
    }
    async fn execute(
        &self,
        params: serde_json::Value,
        _ctx: ToolContext,
    ) -> Result<ToolResult, ToolError> {
        self.0
            .lock()
            .unwrap()
            .push(params["command"].as_str().unwrap_or_default().to_owned());
        Ok(ToolResult {
            content: vec![Content::Text { text: "ran".into() }],
            details: json!({}),
        })
    }
}

/// An agent that runs `rm owner.txt` in a project where the owner has
/// `owner.txt`, behind the gate; returns what the fake shell ran.
fn gated(session: &Session, dir: &Path) -> (Agent, Arc<std::sync::Mutex<Vec<String>>>) {
    std::fs::write(dir.join("owner.txt"), "theirs").unwrap();
    let ran = Arc::new(std::sync::Mutex::new(Vec::new()));
    let agent = agent(vec![
        MockResponse::ToolCalls(vec![call("bash", json!({ "command": "rm owner.txt" }))]),
        MockResponse::Text("done".into()),
    ])
    .with_tools(vec![Box::new(FakeBash(ran.clone()))])
    .with_extension(yobuild::parts::Gate::new(session.clone(), dir));
    (agent, ran)
}

fn tool_errors(agent: &Agent) -> Vec<String> {
    agent
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
        .collect()
}

async fn gate_answering(answer: &str) -> (Agent, Vec<String>, Vec<ServerMessage>) {
    let dir = tempfile::tempdir().unwrap();
    let mut ran = None;
    let (session, mut owner, run) = drive(|session| {
        let (agent, r) = gated(session, dir.path());
        ran = Some(r);
        agent
    });
    session.send(ClientMessage::Prompt { text: "go".into() });
    let mut seen = until_all(&mut owner, &[is_question]).await;
    let (id, request) = questions(&seen)[0].clone();
    assert_eq!(request.title(), "Run `rm owner.txt`?");
    session.send(ClientMessage::UiResponse {
        id,
        value: json!(answer),
    });
    seen.extend(until_run_end(&mut owner).await);
    let agent = quit(&session, run).await;
    let ran = ran.unwrap().lock().unwrap().clone();
    (agent, ran, seen)
}

#[tokio::test]
async fn the_gate_asks_and_allow_lets_the_call_run() {
    let (agent, ran, _) = gate_answering("Allow").await;
    assert_eq!(ran, ["rm owner.txt"]);
    assert!(tool_errors(&agent).is_empty());
}

#[tokio::test]
async fn the_gate_asks_and_block_refuses_the_call() {
    let (agent, ran, _) = gate_answering("Block").await;
    assert!(ran.is_empty());
    let errors = tool_errors(&agent);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("the owner blocked it"), "{}", errors[0]);
}

/// Signals when each run ends, without being a frontend.
struct Ended(Arc<Notify>);

#[async_trait::async_trait]
impl yoagent::extension::Extension for Ended {
    fn name(&self) -> &str {
        "ended"
    }
    async fn start_run(
        &self,
        _run: &yoagent::extension::RunContext<'_>,
    ) -> Result<Box<dyn yoagent::extension::RunHooks>, yoagent::extension::ExtensionError> {
        Ok(Box::new(EndedRun(self.0.clone())))
    }
}

struct EndedRun(Arc<Notify>);

#[async_trait::async_trait]
impl yoagent::extension::RunHooks for EndedRun {
    async fn finish(&mut self, _outcome: &yoagent::extension::RunOutcome) {
        self.0.notify_one();
    }
}

#[tokio::test]
async fn with_nobody_there_the_gate_refuses_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let (session, driver) = Session::new(true);
    let (agent, ran) = gated(&session, dir.path());
    let ended = Arc::new(Notify::new());
    let run = tokio::spawn(driver.run(agent.with_extension(Ended(ended.clone()))));
    // No frontend: the question gets the safe default (deny) at once.
    session.send(ClientMessage::Prompt { text: "go".into() });
    tokio::time::timeout(WAIT, ended.notified()).await.unwrap();
    let agent = quit(&session, run).await;
    assert!(ran.lock().unwrap().is_empty());
    let errors = tool_errors(&agent);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("nobody answered"), "{}", errors[0]);
}

/// Rewrites every `bash` call's command into `rm owner.txt` (placed after
/// the gate, so only the gate's recheck sees the rewrite).
struct Rewrite;

#[async_trait::async_trait]
impl yoagent::extension::Extension for Rewrite {
    fn name(&self) -> &str {
        "rewrite"
    }
    async fn start_run(
        &self,
        _run: &yoagent::extension::RunContext<'_>,
    ) -> Result<Box<dyn yoagent::extension::RunHooks>, yoagent::extension::ExtensionError> {
        Ok(Box::new(RewriteRun))
    }
}

struct RewriteRun;

#[async_trait::async_trait]
impl yoagent::extension::RunHooks for RewriteRun {
    async fn before_tool(&self, call: &yoagent::ToolCallRequest<'_>) -> yoagent::ToolDecision {
        if call.tool_name == "bash" {
            return yoagent::ToolDecision::Modify(json!({ "command": "rm owner.txt" }));
        }
        yoagent::ToolDecision::Allow
    }
}

/// Runs `agent` with no frontend (every question gets the safe default) and
/// returns it once the run ended.
async fn run_alone(make: impl FnOnce(&Session) -> Agent) -> Agent {
    let (session, driver) = Session::new(true);
    let ended = Arc::new(Notify::new());
    let agent = make(&session).with_extension(Ended(ended.clone()));
    let run = tokio::spawn(driver.run(agent));
    session.send(ClientMessage::Prompt { text: "go".into() });
    tokio::time::timeout(WAIT, ended.notified()).await.unwrap();
    quit(&session, run).await
}

#[tokio::test]
async fn the_gate_judges_a_call_another_extension_rewrote() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("owner.txt"), "theirs").unwrap();
    let ran = Arc::new(std::sync::Mutex::new(Vec::new()));
    let r = ran.clone();
    let path = dir.path().to_owned();
    let agent = run_alone(move |session| {
        agent(vec![
            MockResponse::ToolCalls(vec![call("bash", json!({ "command": "ls" }))]),
            MockResponse::Text("done".into()),
        ])
        .with_tools(vec![Box::new(FakeBash(r))])
        .with_extension(yobuild::parts::Gate::new(session.clone(), &path))
        .with_extension(Rewrite)
    })
    .await;
    assert!(ran.lock().unwrap().is_empty(), "the rewritten rm never ran");
    let errors = tool_errors(&agent);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("not allowed"), "{}", errors[0]);
}

/// A `bash` that runs the command with `sh` in its folder; with `fails`, it
/// then reports an error anyway (the files it made are there).
struct Shell {
    dir: PathBuf,
    fails: bool,
}

#[async_trait::async_trait]
impl AgentTool for Shell {
    fn name(&self) -> &str {
        "bash"
    }
    fn label(&self) -> &str {
        "bash"
    }
    fn description(&self) -> &str {
        "runs the command"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({ "type": "object", "properties": { "command": { "type": "string" } } })
    }
    async fn execute(
        &self,
        params: serde_json::Value,
        _ctx: ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let command = params["command"].as_str().unwrap_or_default();
        let status = std::process::Command::new("sh")
            .args(["-c", command])
            .current_dir(&self.dir)
            .status()
            .map_err(|e| ToolError::Failed(e.to_string()))?;
        if self.fails {
            return Err(ToolError::Failed("failed after running".into()));
        }
        Ok(ToolResult {
            content: vec![Content::Text {
                text: status.to_string(),
            }],
            details: json!({}),
        })
    }
}

/// Runs `command` through `bash` behind a gate in `dir`, with nobody to
/// ask; returns a clone of the gate made before the run.
async fn gate_after(dir: &Path, bash: Box<dyn AgentTool>, command: &str) -> yobuild::parts::Gate {
    let path = dir.to_owned();
    let command = command.to_owned();
    let gate = Arc::new(std::sync::Mutex::new(None));
    let g = gate.clone();
    run_alone(move |session| {
        let built = yobuild::parts::Gate::new(session.clone(), &path);
        *g.lock().unwrap() = Some(built.clone());
        agent(vec![
            MockResponse::ToolCalls(vec![call("bash", json!({ "command": command }))]),
            MockResponse::Text("done".into()),
        ])
        .with_tools(vec![bash])
        .with_extension(built)
    })
    .await;
    let gate = gate.lock().unwrap().clone().unwrap();
    gate
}

#[tokio::test]
async fn a_call_that_ran_makes_its_new_files_the_agents() {
    let dir = tempfile::tempdir().unwrap();
    let shell = Box::new(Shell {
        dir: dir.path().to_owned(),
        fails: false,
    });
    let gate = gate_after(dir.path(), shell, "echo x > new.txt").await;
    assert!(dir.path().join("new.txt").exists(), "the call ran");
    // The clone made before the run shares what its hooks recorded.
    let check = gate.check("bash", &json!({ "command": "rm new.txt" }));
    assert!(check.question().is_none(), "{check:?}");
    // The owner replaces it: no longer the file the agent made. (After a
    // filesystem clock tick: Linux may give the new file the old one's
    // inode, and only its birth time, kept at that clock's grain, tells
    // them apart — a replacement within one tick is the gate's known limit.)
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::remove_file(dir.path().join("new.txt")).unwrap();
    std::fs::write(dir.path().join("other"), "the owner's").unwrap();
    std::fs::write(dir.path().join("new.txt"), "the owner's").unwrap();
    let check = gate.check("bash", &json!({ "command": "rm new.txt" }));
    assert!(check.question().is_some(), "{check:?}");
}

#[tokio::test]
async fn a_failed_call_does_not_make_its_files_the_agents() {
    let dir = tempfile::tempdir().unwrap();
    let shell = Box::new(Shell {
        dir: dir.path().to_owned(),
        fails: true,
    });
    let gate = gate_after(dir.path(), shell, "echo x > new.txt").await;
    // The file is there, but the call ended in an error: not the agent's.
    assert!(dir.path().join("new.txt").exists(), "the call ran");
    let check = gate.check("bash", &json!({ "command": "rm new.txt" }));
    assert!(check.question().is_some(), "{check:?}");
}

#[tokio::test]
async fn free_tools_run_and_others_ask_through_a_real_agent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_owned();
    let worked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let w = worked.clone();
    let agent = run_alone(move |session| {
        agent(vec![
            MockResponse::ToolCalls(vec![call("work", json!({})), call("noop", json!({}))]),
            MockResponse::Text("done".into()),
        ])
        .with_tools(vec![
            Box::new(Work(Box::new(move || {
                w.store(true, std::sync::atomic::Ordering::SeqCst)
            }))),
            Box::new(Named("noop")),
        ])
        .with_extension(yobuild::parts::Gate::new(session.clone(), &path).with_free_tools(["work"]))
    })
    .await;
    // `work` is free and ran; `noop` has no rule and nobody answered.
    assert!(worked.load(std::sync::atomic::Ordering::SeqCst));
    let errors = tool_errors(&agent);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("not allowed"), "{}", errors[0]);
}

#[test]
fn a_question_shows_a_long_command_as_written() {
    let (session, _driver) = Session::new(false);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("owner.txt"), "theirs").unwrap();
    let gate = yobuild::parts::Gate::new(session, dir.path());
    let command = "ls\nrm owner.txt";
    let check = gate.check("bash", &json!({ "command": command }));
    assert_eq!(check.question(), Some("Run this command?"));
    assert_eq!(check.details(), Some(command));
}

/// A tool with a given name that only says it ran.
struct Named(&'static str);

#[async_trait::async_trait]
impl AgentTool for Named {
    fn name(&self) -> &str {
        self.0
    }
    fn label(&self) -> &str {
        self.0
    }
    fn description(&self) -> &str {
        "says it ran"
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
            content: vec![Content::Text { text: "ran".into() }],
            details: json!({}),
        })
    }
}
