use super::*;

use yoagent::extension::Extension;
use yoagent::provider::ModelConfig;
use yoagent::Message;

/// An agent's folder with its manifest (`extra` appended), and a working
/// folder and a home beside it.
struct Fixture {
    _dir: tempfile::TempDir,
    manifest: Manifest,
    cwd: PathBuf,
    home: PathBuf,
}

fn fixture(extra: &str) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let agent = dir.path().join("agent");
    let cwd = dir.path().join("work");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    let file = agent.join("agent.toml");
    std::fs::write(
        &file,
        format!("[agent]\nname = \"keeper\"\nmodel = \"deepseek/deepseek-flash\"\n{extra}"),
    )
    .unwrap();
    Fixture {
        manifest: Manifest::load(&file).unwrap(),
        _dir: dir,
        cwd,
        home,
    }
}

fn opened(f: &Fixture, resume: bool) -> Result<Opened, String> {
    open(&f.manifest, &f.cwd, Some(&f.home), resume).map_err(|e| e.to_string())
}

fn saver(opened: Opened, session: &Session) -> Saver {
    Saver::new(
        opened.store.expect("a store"),
        session.clone(),
        &opened.manifest,
        &opened.manifest_sha256,
        &opened.cwd,
        "deepseek/deepseek-flash".into(),
        None,
        None,
        opened.resumed.as_ref(),
    )
}

fn agent(texts: &[&str]) -> Agent {
    Agent::from_config(ModelConfig::mock()).with_messages(
        texts
            .iter()
            .map(|t| AgentMessage::Llm(Message::user(*t)))
            .collect(),
    )
}

fn texts(saved: &Saved) -> Vec<String> {
    saved
        .messages
        .iter()
        .filter_map(|m| match m {
            AgentMessage::Llm(Message::User { content, .. }) => match content.first() {
                Some(yoagent::Content::Text { text }) => Some(text.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[test]
fn an_agent_in_another_folder_is_another_session() {
    let home = Path::new("/h");
    let a = folder(home, "keeper", Path::new("/a/agent.toml"), Path::new("/w1"));
    let b = folder(home, "keeper", Path::new("/a/agent.toml"), Path::new("/w2"));
    let c = folder(home, "keeper", Path::new("/b/agent.toml"), Path::new("/w1"));
    assert_ne!(a, b);
    assert_ne!(a, c);
    assert_eq!(a.parent().unwrap(), Path::new("/h/sessions"));
    let name = a.file_name().unwrap().to_str().unwrap();
    assert!(
        name.starts_with("keeper-") && name.len() == "keeper-".len() + 16,
        "{name}"
    );
    assert_eq!(
        a,
        folder(home, "keeper", Path::new("/a/agent.toml"), Path::new("/w1"))
    );
}

#[tokio::test]
async fn a_session_is_saved_once_a_run_ended_and_read_back_whole() {
    let f = fixture("");
    let (session, _driver) = Session::new(false);
    let first = opened(&f, false).unwrap();
    assert!(first.notes.is_empty(), "{:?}", first.notes);
    let mut kept = saver(first, &session);
    let file = kept.file();
    // A new session's reset, before any run of its own: nothing written.
    kept.settled(&agent(&[]), Settled::Reset);
    kept.ending(Some(&agent(&[])));
    assert!(!file.exists());
    kept.settled(&agent(&["remember heliotrope"]), Settled::Run);
    assert_eq!(kept.saved(), Some(file.clone()));
    drop(kept);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&file), 0o600);
        assert_eq!(mode(file.parent().unwrap()), 0o700);
    }
    // Only the file and its lock: no temp file left.
    let mut names: Vec<String> = std::fs::read_dir(file.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["session.json", "session.lock"]);

    // A new session says it is there and replaces it only once its run ends.
    let again = opened(&f, false).unwrap();
    assert_eq!(again.notes.len(), 1);
    let (level, note) = &again.notes[0];
    assert_eq!(*level, NoticeLevel::Info);
    assert!(
        note.starts_with("A session of this agent in this folder was saved ")
            && note.contains(" (1 messages): `--resume` continues it instead. This new session replaces it when its first run ends, keeping it as ")
            && note.ends_with("/session.prev.json (renamed to session.json, `--resume` continues it)."),
        "{note}"
    );
    drop(again);

    let resumed = opened(&f, true).unwrap();
    assert!(resumed.notes.is_empty());
    let saved = resumed.resumed.clone().expect("resumed");
    assert_eq!(texts(&saved), ["remember heliotrope"]);
    assert_eq!(saved.version, FORMAT);
    assert_eq!(saved.model, "deepseek/deepseek-flash");
    assert_eq!(saved.cwd, std::fs::canonicalize(&f.cwd).unwrap());
    assert_eq!(saved.manifest, resumed.manifest);
    assert_eq!(saved.spent_usd, None);
    assert_eq!(saved.questions, Questions::default());
    // A resumed session saves from the start: a reset is kept.
    let mut kept = saver(resumed, &session);
    kept.settled(&agent(&[]), Settled::Reset);
    drop(kept);
    let saved = opened(&f, true).unwrap().resumed.unwrap();
    assert!(saved.messages.is_empty());
}

#[test]
fn resume_with_nothing_saved_starts_a_new_session_said() {
    let f = fixture("");
    let opened = opened(&f, true).unwrap();
    assert!(opened.resumed.is_none() && opened.store.is_some());
    assert_eq!(
        opened.notes,
        [(
            NoticeLevel::Info,
            "No session of this agent in this folder was saved: a new one starts.".to_owned()
        )]
    );
}

#[test]
fn a_later_format_and_a_broken_file_are_refused_on_resume_only() {
    let f = fixture("");
    let dir = opened(&f, false).unwrap().store.unwrap().dir.clone();
    let file = dir.join(FILE);
    std::fs::write(&file, r#"{"version": 2, "whatever": true}"#).unwrap();
    let e = opened(&f, true).err().unwrap();
    assert_eq!(
        e,
        format!(
            "cannot resume {}: saved by a later yobuild (session format 2; this one reads 1): move it aside, or run without --resume (a new session then replaces it when its first run ends)",
            file.display()
        )
    );
    std::fs::write(&file, "{").unwrap();
    let e = opened(&f, true).err().unwrap();
    assert!(e.contains(": not JSON ("), "{e}");
    std::fs::write(&file, r#"{"version": 1, "messages": []}"#).unwrap();
    let e = opened(&f, true).err().unwrap();
    assert!(e.contains(": not a saved session (missing field"), "{e}");
    // Left as it is.
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        r#"{"version": 1, "messages": []}"#
    );
    // A new session says so, and goes on.
    let fresh = opened(&f, false).unwrap();
    assert!(fresh.store.is_some());
    assert!(
        fresh.notes[0]
            .1
            .starts_with("A session of this agent in this folder was saved, and does not read (not a saved session"),
        "{:?}",
        fresh.notes
    );
}

#[cfg(unix)]
#[test]
fn a_session_another_yobuild_holds_is_not_resumed_nor_saved() {
    let f = fixture("");
    let holding = opened(&f, false).unwrap();
    assert!(holding.store.is_some());
    assert_eq!(
        opened(&f, true).err().unwrap(),
        "another yobuild runs this agent in this folder: stop it first, or run without --resume (this session is then not saved)"
    );
    let unsaved = opened(&f, false).unwrap();
    assert!(unsaved.store.is_none());
    assert_eq!(
        unsaved.notes,
        [(
            NoticeLevel::Warning,
            "Another yobuild runs this agent in this folder: this session is not saved (that one is).".to_owned()
        )]
    );
    drop(holding);
    assert!(opened(&f, true).unwrap().store.is_some());
}

#[test]
fn save_false_writes_nothing_and_refuses_resume() {
    let f = fixture("[session]\nsave = false\n");
    let opened_ = opened(&f, false).unwrap();
    assert!(opened_.store.is_none() && opened_.notes.is_empty());
    assert!(!f.home.exists());
    assert_eq!(
        opened(&f, true).err().unwrap(),
        "agent.toml's `[session] save = false`: no session is saved to resume"
    );
}

#[tokio::test]
async fn open_questions_and_the_spend_are_saved_and_said() {
    let f = fixture("[builtins]\nfacts = true\n");
    let (session, _driver) = Session::new(false);
    let opened_ = opened(&f, false).unwrap();
    let ask = AskOwner::new(session.clone());
    assert_eq!(
        ask.ask(Question::new("Which branch?", "the merge", Vec::new()).unwrap()),
        1
    );
    assert_eq!(
        ask.ask(Question::new("Ship it?", "", ["Yes".into(), "No".into()]).unwrap()),
        2
    );
    let spend = SessionSpend::new(
        yoagent::provider::CostConfig::new(1.0, 1.0),
        Some(1.0),
        0.25,
        "session budget",
    );
    let mut saver = Saver::new(
        opened_.store.unwrap(),
        session.clone(),
        &opened_.manifest,
        &opened_.manifest_sha256,
        &opened_.cwd,
        "deepseek/deepseek-flash".into(),
        Some(ask),
        Some(spend),
        None,
    );
    saver.settled(&agent(&["a"]), Settled::Run);
    drop(saver);
    let resumed = opened(&f, true).unwrap();
    let saved = resumed.resumed.as_ref().unwrap();
    assert_eq!(saved.spent_usd, Some(0.25));
    assert_eq!(
        saved.questions,
        Questions {
            next: 2,
            open: vec![
                OpenQuestion {
                    n: 1,
                    question: "Which branch?".into(),
                    why: "the merge".into(),
                    options: Vec::new(),
                },
                OpenQuestion {
                    n: 2,
                    question: "Ship it?".into(),
                    why: String::new(),
                    options: vec!["Yes".into(), "No".into()],
                },
            ],
        }
    );

    // Said on resume: what does not come back, the spend.
    let notes = resumed_notes(saved, &f.manifest, &resumed.manifest_sha256, Some(1.0));
    assert_eq!(
        notes,
        [
            (
                NoticeLevel::Info,
                format!(
                    "Resumed the session saved {} (1 messages). Not resumed: what the page showed before; facts' snapshot (taken again at this session's first run).",
                    saved.saved
                )
            ),
            (
                NoticeLevel::Info,
                "This session has spent $0.2500 so far (its budget: $1.00).".to_owned()
            ),
        ]
    );
    // Asked again with their numbers, the next one after them.
    let (session, _driver) = Session::new(false);
    let mut frontend = session.connect();
    let ask = AskOwner::new(session.clone());
    ask_again(saved, Some(&ask), &session);
    assert_eq!(
        ask.open().into_iter().map(|(n, _)| n).collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(ask.ask(Question::new("Third?", "", Vec::new()).unwrap()), 3);
    let mut said = Vec::new();
    while let Ok(Some(message)) = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        frontend.messages.recv(),
    )
    .await
    {
        if let crate::frontend::ServerMessage::Notice { message, .. } = message {
            said.push(message);
        }
    }
    assert_eq!(
        said,
        ["Asked again: #1 Which branch?", "Asked again: #2 Ship it?"]
    );
    // With no `ask_owner` now: said, not asked.
    let (session, _driver) = Session::new(false);
    let mut frontend = session.connect();
    ask_again(saved, None, &session);
    let mut said = Vec::new();
    while let Ok(Some(message)) = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        frontend.messages.recv(),
    )
    .await
    {
        if let crate::frontend::ServerMessage::Notice { message, .. } = message {
            said.push(message);
        }
    }
    assert_eq!(
        said,
        ["2 open question(s) of the saved session were not asked again: `ask_owner` is off now."]
    );
}

#[test]
fn another_model_and_a_changed_manifest_are_said() {
    let f = fixture(
        "[[plugin]]\nrutis = \"./p.ts\"\nruntime = \"node\"\n\n[extend]\nkinds = [\"remote\"]\n",
    );
    let saved = Saved {
        version: FORMAT,
        manifest: PathBuf::from("/a/agent.toml"),
        cwd: PathBuf::from("/w"),
        manifest_sha256: "old".into(),
        model: "openai/gpt-4o".into(),
        saved: "2026-11-03T10:00:00Z".into(),
        messages: Vec::new(),
        spent_usd: Some(0.0),
        questions: Questions::default(),
    };
    assert_eq!(
        resumed_notes(&saved, &f.manifest, "new", None),
        [
            (
                NoticeLevel::Info,
                "Resumed the session saved 2026-11-03T10:00:00Z (0 messages). Not resumed: what the page showed before; the plugins the agent loaded (it may load them again); the sign-ins (each signs in again); the plugins' own state.".to_owned()
            ),
            (
                NoticeLevel::Warning,
                "This session was saved with openai/gpt-4o; it goes on with deepseek/deepseek-flash.".to_owned()
            ),
            (
                NoticeLevel::Warning,
                "agent.toml changed since this session was saved: it goes on under agent.toml as it is now.".to_owned()
            ),
        ]
    );
}

#[tokio::test]
async fn a_new_session_keeps_the_one_it_replaces() {
    let f = fixture("");
    let (session, _driver) = Session::new(false);
    let mut first = saver(opened(&f, false).unwrap(), &session);
    first.settled(&agent(&["first"]), Settled::Run);
    drop(first);
    let mut second = saver(opened(&f, false).unwrap(), &session);
    let file = second.file();
    let previous = file.with_file_name("session.prev.json");
    assert!(!previous.exists());
    second.settled(&agent(&["second"]), Settled::Run);
    second.settled(&agent(&["second", "more"]), Settled::Run);
    drop(second);
    // Moved aside once, at the first save; the later ones replace only the
    // new session's own.
    let read =
        |p: &Path| serde_json::from_str::<Saved>(&std::fs::read_to_string(p).unwrap()).unwrap();
    assert_eq!(texts(&read(&previous)), ["first"]);
    assert_eq!(texts(&read(&file)), ["second", "more"]);
    // A resumed session replaces nothing aside.
    let mut third = saver(opened(&f, true).unwrap(), &session);
    third.settled(&agent(&["third"]), Settled::Run);
    drop(third);
    assert_eq!(texts(&read(&previous)), ["first"]);
}

#[tokio::test]
async fn a_reset_saves_no_question_and_an_interrupted_session_keeps_its_spend() {
    let f = fixture("");
    let (session, _driver) = Session::new(false);
    let opened_ = opened(&f, false).unwrap();
    let ask = AskOwner::new(session.clone());
    let spend = SessionSpend::new(
        yoagent::provider::CostConfig::new(1_000_000.0, 0.0),
        None,
        0.25,
        "session budget",
    );
    let mut kept = Saver::new(
        opened_.store.unwrap(),
        session.clone(),
        &opened_.manifest,
        &opened_.manifest_sha256,
        &opened_.cwd,
        "deepseek/deepseek-flash".into(),
        Some(ask.clone()),
        Some(spend.clone()),
        None,
    );
    let file = kept.file();
    let read = || serde_json::from_str::<Saved>(&std::fs::read_to_string(&file).unwrap()).unwrap();
    ask.ask(Question::new("Q?", "", Vec::new()).unwrap());
    kept.settled(&agent(&["a"]), Settled::Run);
    assert_eq!(read().questions.open.len(), 1);
    // A reset: the file holds no question at once.
    kept.settled(&agent(&[]), Settled::Reset);
    assert_eq!(read().questions.open, []);
    assert_eq!(read().questions.next, 1);
    kept.settled(&agent(&["b"]), Settled::Run);
    // A run spends, then is stopped (no agent back): its spend and a
    // question it asked are saved over the last save's conversation.
    let cancel = tokio_util::sync::CancellationToken::new();
    let run = spend
        .start_run(&yoagent::extension::RunContext::new("r", &[], &cancel))
        .await
        .unwrap();
    run.on_event(&yoagent::AgentEvent::MessageEnd {
        message: AgentMessage::Llm(Message::assistant(
            Vec::new(),
            yoagent::StopReason::Stop,
            "m",
            "p",
            yoagent::Usage {
                input: 1,
                ..yoagent::Usage::default()
            },
        )),
    });
    ask.ask(Question::new("Later?", "", Vec::new()).unwrap());
    kept.ending(None);
    let saved = read();
    assert_eq!(saved.spent_usd, Some(1.25));
    assert_eq!(texts(&saved), ["b"]);
    assert_eq!(
        saved.questions.open.iter().map(|q| q.n).collect::<Vec<_>>(),
        [2]
    );
    assert_eq!(kept.saved(), Some(file.clone()));
}

#[tokio::test]
async fn with_ask_owner_off_the_numbering_is_carried() {
    let f = fixture("");
    let (session, _driver) = Session::new(false);
    let mut first = saver(opened(&f, false).unwrap(), &session);
    first.settled(&agent(&["a"]), Settled::Run);
    drop(first);
    // As if a session with `ask_owner` had numbered up to #4.
    let file = opened(&f, true).unwrap().store.unwrap().file();
    let mut saved: Saved = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    saved.questions.next = 4;
    std::fs::write(&file, serde_json::to_string(&saved).unwrap()).unwrap();
    let mut resumed = saver(opened(&f, true).unwrap(), &session);
    resumed.settled(&agent(&["a", "b"]), Settled::Run);
    drop(resumed);
    let saved: Saved = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(saved.questions.next, 4);
}
