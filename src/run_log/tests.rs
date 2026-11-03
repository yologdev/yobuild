use super::*;

fn base() -> Base {
    Base {
        agent: "a".into(),
        manifest: "/x/agent.toml".into(),
        folder: "/x".into(),
        session: "s-1".into(),
        mode: "once",
        scripted: true,
    }
}

fn stats(turns: u32, input: u64, output: u64) -> yoagent::SessionStats {
    let mut stats = yoagent::SessionStats::default();
    stats.turns = turns;
    stats.usage.input = input;
    stats.usage.cache_read = 5;
    stats.usage.output = output;
    stats
}

fn ended(seeing: Seeing, outcome: RunOutcome, waiting: Vec<String>, hide: &Hide) -> Entry {
    entry(
        &base(),
        seeing,
        Ending {
            outcome,
            error: None,
            stats: &stats(2, 10, 3),
            total_cost_usd: Some(0.0021),
            waiting,
        },
        hide,
    )
}

#[test]
fn an_entry_holds_the_run_as_seen() {
    let mut seeing = Seeing {
        started: "2026-11-03T09:00:00Z".into(),
        run: 2,
        task: "sum the receipts".into(),
        facts: Some("since the start: no changes".into()),
        answer: Some("## Done\nAll summed.".into()),
        ..Default::default()
    };
    seeing.tool("read_file", false);
    seeing.tool("read_file", false);
    seeing.tool("bash", true);
    let e = ended(
        seeing,
        RunOutcome::Completed,
        vec!["#1 Which month?".into()],
        &Hide::default(),
    );
    assert_eq!(e.v, VERSION);
    assert_eq!(
        (e.agent.as_str(), e.run, e.session.as_str()),
        ("a", 2, "s-1")
    );
    assert_eq!((e.tokens_in, e.tokens_out, e.turns), (15, 3, 2));
    assert_eq!(
        e.tools,
        [
            Tools {
                name: "bash".into(),
                ok: 0,
                failed: 1
            },
            Tools {
                name: "read_file".into(),
                ok: 2,
                failed: 0
            }
        ]
    );
    assert_eq!(e.waiting, ["#1 Which month?"]);
    assert_eq!(e.report.as_deref(), Some("## Done\nAll summed."));
    assert_eq!(e.cost_line(), "$0.0021");
    assert_eq!(e.outcome_line(), "Completed");
    // As written and read back.
    let line = serde_json::to_string(&e).unwrap();
    assert!(line.contains(r#""costUsd":0.0021"#), "{line}");
    assert!(line.contains(r#""outcome":"completed""#), "{line}");
    assert_eq!(serde_json::from_str::<Entry>(&line).unwrap(), e);
}

#[test]
fn texts_are_cut_to_their_bounds() {
    assert_eq!(cut("short", 10), "short");
    assert_eq!(cut("abcdef", 3), "abc\n… (cut: 3 more characters)");
    let seeing = Seeing {
        task: "t".repeat(TASK_CHARS + 5),
        answer: Some("r".repeat(REPORT_CHARS * 2)),
        ..Default::default()
    };
    let waiting = (1..=30)
        .map(|n| format!("#{n} {}", "q".repeat(600)))
        .collect();
    let e = ended(seeing, RunOutcome::Completed, waiting, &Hide::default());
    assert!(e.task.ends_with("\n… (cut: 5 more characters)"));
    assert!(e
        .report
        .unwrap()
        .ends_with(&format!("\n… (cut: {REPORT_CHARS} more characters)")));
    assert_eq!(e.waiting.len(), QUESTIONS);
    assert!(e.waiting[0].chars().count() < QUESTION_CHARS + 40);
    // Tools past the bound are not counted; those already there are.
    let mut seeing = Seeing::default();
    for n in 0..TOOLS + 3 {
        seeing.tool(&format!("t{n:03}"), false);
    }
    seeing.tool("t000", true);
    assert_eq!(seeing.tools.len(), TOOLS);
    assert_eq!(seeing.tools["t000"], (1, 1));
}

#[test]
fn credentials_are_never_written() {
    let hide = Hide::of([
        ("DEEPSEEK_API_KEY".to_owned(), "sk-abcdefgh12345".to_owned()),
        ("MY_TOKEN".to_owned(), "tok-12345678".to_owned()),
        // Too short to be one: left.
        ("PORT_KEY".to_owned(), "8080".to_owned()),
    ]);
    let seeing = Seeing {
        task: "use sk-abcdefgh12345 on port 8080".into(),
        answer: Some("I sent tok-12345678.".into()),
        facts: Some("tok-12345678".into()),
        ..Default::default()
    };
    let mut e = entry(
        &base(),
        seeing,
        Ending {
            outcome: RunOutcome::Error,
            error: Some("Auth error: sk-abcdefgh12345 refused"),
            stats: &stats(1, 1, 1),
            total_cost_usd: None,
            waiting: vec!["#1 Is tok-12345678 yours?".into()],
        },
        &hide,
    );
    let line = serde_json::to_string(&e).unwrap();
    assert!(
        !line.contains("sk-abcdefgh12345") && !line.contains("tok-12345678"),
        "{line}"
    );
    assert_eq!(e.task, "use ${DEEPSEEK_API_KEY} on port 8080");
    assert_eq!(e.report.as_deref(), Some("I sent ${MY_TOKEN}."));
    assert_eq!(
        e.error.as_deref(),
        Some("Auth error: ${DEEPSEEK_API_KEY} refused")
    );
    assert_eq!(e.waiting, ["#1 Is ${MY_TOKEN} yours?"]);
    assert_eq!(e.cost_line(), "not priced");
    e.turns = 0;
    assert_eq!(e.cost_line(), "unknown");
    e.outcome = RunOutcome::Completed;
    assert_eq!(e.cost_line(), "nothing spent");
}

#[test]
fn a_credential_at_the_cut_is_hidden_whole_not_left_in_part() {
    let key = "sk-abcdefgh12345";
    let hide = Hide::of([("DEEPSEEK_API_KEY".to_owned(), key.to_owned())]);
    // The key straddles the task's bound: cut first, its first half would
    // stay.
    let task = format!("{}{key} and more", "t".repeat(TASK_CHARS - 6));
    let seeing = Seeing {
        task,
        ..Default::default()
    };
    let e = ended(seeing, RunOutcome::Completed, vec![], &hide);
    assert!(!e.task.contains("sk-abc"), "{}", &e.task[TASK_CHARS - 20..]);
    assert!(e.task.contains("${DEEP"), "{}", &e.task[TASK_CHARS - 20..]);
}

#[test]
fn what_the_manifest_hands_out_is_hidden_whatever_its_name() {
    let text = r#"
[agent]
name = "a"
model = "deepseek/deepseek-flash"

[[plugin]]
rutis = "./p.ts"
runtime = "node"
grants = { env = ["DATA_PLACE", "CORP_*"] }
config = { where = "${CONFIG_PLACE}" }

[[mcp]]
name = "web"
url = "https://example.com/mcp/${URL_PART}"
headers = { "X-Thing" = "${HEADER_PLACE}" }

[[mcp]]
name = "local"
npm = "./server.sh"
args = ["${ARG_PLACE}"]
env = { MODE = "${ENV_PLACE}" }
"#;
    let manifest = Manifest::parse(text, Path::new("/x/agent.toml"), Path::new("/x")).unwrap();
    let (names, prefixes) = handed_out(&manifest);
    for name in [
        "DEEPSEEK_API_KEY", "DATA_PLACE", "CONFIG_PLACE", "URL_PART", "HEADER_PLACE", "ARG_PLACE",
        "ENV_PLACE",
    ] {
        assert!(names.contains(name), "{name}: {names:?}");
    }
    assert_eq!(prefixes, ["CORP_"]);
}

#[test]
fn a_credential_load_plugin_takes_is_hidden_from_then_on() {
    let taken = Credentials::default();
    let hide = Hide::default().with_taken(taken.clone());
    assert_eq!(hide.hidden("abc123"), "abc123");
    taken
        .lock()
        .unwrap()
        .push(("CTX_HEADER".to_owned(), "abc123".to_owned()));
    // Whatever its length.
    assert_eq!(hide.hidden("sent abc123"), "sent ${CTX_HEADER}");
}

#[test]
fn a_value_inside_another_is_hidden_as_the_longer() {
    let hide = Hide::of([
        ("SHORT_KEY".to_owned(), "abcdefgh".to_owned()),
        ("LONG_KEY".to_owned(), "abcdefgh-ijkl".to_owned()),
    ]);
    assert_eq!(
        hide.hidden("abcdefgh-ijkl abcdefgh"),
        "${LONG_KEY} ${SHORT_KEY}"
    );
}

fn log_in(home: &Path) -> RunLog {
    let dir = home.join("agent");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("agent.toml"),
        "[agent]\nname = \"a\"\nmodel = \"deepseek/deepseek-flash\"\n",
    )
    .unwrap();
    let manifest = Manifest::load(dir.join("agent.toml")).unwrap();
    RunLog::of(&manifest, Some(home)).unwrap()
}

#[test]
fn the_log_is_the_agents_own_and_private() {
    let home = tempfile::tempdir().unwrap();
    let log = log_in(home.path());
    // Under the home, named after the agent and its manifest's path.
    let name = log.dir().file_name().unwrap().to_str().unwrap().to_owned();
    assert!(log.dir().starts_with(home.path().join("logs")));
    assert!(
        name.starts_with("a-") && name.len() == "a-".len() + 16,
        "{name}"
    );
    // None yet.
    let read = log.read(10).unwrap();
    assert!(read.entries.is_empty() && read.unread == 0);
    let e = ended(
        Seeing::default(),
        RunOutcome::Completed,
        vec![],
        &Hide::default(),
    );
    log.append(&e).unwrap();
    log.append(&e).unwrap();
    assert_eq!(log.read(10).unwrap().entries, [e.clone(), e.clone()]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(log.dir()), 0o700);
        assert_eq!(mode(&log.file()), 0o600);
    }
    // Another manifest of the same name keeps its own.
    let other = tempfile::tempdir().unwrap();
    std::fs::write(
        other.path().join("agent.toml"),
        "[agent]\nname = \"a\"\nmodel = \"deepseek/deepseek-flash\"\n",
    )
    .unwrap();
    let manifest = Manifest::load(other.path().join("agent.toml")).unwrap();
    assert_ne!(
        RunLog::of(&manifest, Some(home.path())).unwrap().dir(),
        log.dir()
    );
}

#[test]
fn the_log_is_rotated_and_read_whole_and_in_order() {
    let home = tempfile::tempdir().unwrap();
    let log = log_in(home.path());
    let mut e = ended(
        Seeing::default(),
        RunOutcome::Completed,
        vec![],
        &Hide::default(),
    );
    e.run = 1;
    log.append(&e).unwrap();
    // Past the bound: the next append starts a new file.
    let big = std::fs::OpenOptions::new()
        .append(true)
        .open(log.file())
        .unwrap();
    big.set_len(ROTATE_AT).unwrap();
    drop(big);
    e.run = 2;
    log.append(&e).unwrap();
    assert!(log.dir().join(ROTATED).is_file());
    assert!(std::fs::metadata(log.file()).unwrap().len() < 10_000);
    e.run = 3;
    log.append(&e).unwrap();
    std::fs::write(
        log.dir().join(ROTATED),
        format!(
            "{}\n{{\"v\": 99}}\nnot json\n",
            serde_json::to_string(&Entry {
                run: 1,
                ..e.clone()
            })
            .unwrap()
        ),
    )
    .unwrap();
    let read = log.read(10).unwrap();
    let runs: Vec<u64> = read.entries.iter().map(|e| e.run).collect();
    assert_eq!(runs, [1, 2, 3]);
    assert_eq!(read.unread, 2);
    // The last n.
    let runs: Vec<u64> = log.read(2).unwrap().entries.iter().map(|e| e.run).collect();
    assert_eq!(runs, [2, 3]);
}

#[test]
fn a_torn_line_is_one_unread_line_and_the_next_run_keeps_to_its_own() {
    let home = tempfile::tempdir().unwrap();
    let log = log_in(home.path());
    let mut e = ended(
        Seeing::default(),
        RunOutcome::Completed,
        vec![],
        &Hide::default(),
    );
    e.run = 1;
    log.append(&e).unwrap();
    // A crash tore the next line mid-character: no line break, not UTF-8.
    let mut torn = std::fs::OpenOptions::new()
        .append(true)
        .open(log.file())
        .unwrap();
    torn.write_all(b"{\"v\":1,\"task\":\"caf\xc3").unwrap();
    drop(torn);
    e.run = 2;
    log.append(&e).unwrap();
    let read = log.read(10).unwrap();
    let runs: Vec<u64> = read.entries.iter().map(|e| e.run).collect();
    assert_eq!(runs, [1, 2]);
    assert_eq!(read.unread, 1);
}

#[test]
fn two_appenders_rotate_the_log_once() {
    let home = tempfile::tempdir().unwrap();
    let log = log_in(home.path());
    let e = ended(
        Seeing::default(),
        RunOutcome::Completed,
        vec![],
        &Hide::default(),
    );
    log.append(&e).unwrap();
    let file = std::fs::OpenOptions::new()
        .append(true)
        .open(log.file())
        .unwrap();
    file.set_len(ROTATE_AT).unwrap();
    drop(file);
    // Many at once: the one past the bound rotates, the rest append after
    // it, and the rotated history is never rotated away.
    std::thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(|| log.append(&e).unwrap());
        }
    });
    assert_eq!(
        std::fs::metadata(log.dir().join(ROTATED)).unwrap().len(),
        ROTATE_AT
    );
    assert_eq!(log.read(100).unwrap().entries.len(), 8 + 1);
}

#[test]
fn a_log_that_cannot_be_written_says_where() {
    let home = tempfile::tempdir().unwrap();
    let log = log_in(home.path());
    // A file where its folder would be.
    std::fs::create_dir_all(log.dir().parent().unwrap()).unwrap();
    std::fs::write(log.dir(), "").unwrap();
    let e = ended(
        Seeing::default(),
        RunOutcome::Completed,
        vec![],
        &Hide::default(),
    );
    let error = log.append(&e).unwrap_err().to_string();
    assert!(
        error.starts_with(&log.dir().display().to_string()),
        "{error}"
    );
}

#[test]
fn text_shows_each_run() {
    let mut seeing = Seeing {
        started: "2026-11-03T09:00:00Z".into(),
        run: 1,
        task: "sum the receipts\nfor October".into(),
        facts: Some("since the start: no changes; 1 question(s) waiting on you.".into()),
        answer: Some("## Done\n\nSummed.\n## Waiting on you\nWhich month?\n## Next\nx".into()),
        ..Default::default()
    };
    seeing.tool("read_file", false);
    seeing.tool("bash", true);
    seeing.tool("bash", false);
    let mut e = ended(
        seeing,
        RunOutcome::Completed,
        vec!["#1 Which month?".into()],
        &Hide::default(),
    );
    e.seconds = 12.0;
    assert_eq!(
        text(&[e], 3),
        "2026-11-03 09:00:00 UTC  run 1 (once)  Completed  $0.0021  12s
  task: sum the receipts for October
  tools: bash 1✓ 1✗, read_file 1✓
  waiting on you: #1 Which month?
  facts: since the start: no changes; 1 question(s) waiting on you.
  report:
    ## Done
    Summed.
    ## Waiting on you
    …
"
    );
}
