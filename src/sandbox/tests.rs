//! The sandbox's rules and command lines, and on a system that has the
//! tool, a shell confined by them.

use super::*;

/// A home folder, an agent's folder and folders around them, all real.
struct Places {
    _root: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    cwd: PathBuf,
}

fn places() -> Places {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let home = root.join("home");
    let cwd = root.join("work");
    for folder in [&home, &cwd, &home.join(".ssh"), &root.join("out")] {
        std::fs::create_dir_all(folder).unwrap();
    }
    std::fs::write(home.join(".netrc"), "machine x password p\n").unwrap();
    Places {
        _root: dir,
        root,
        home,
        cwd,
    }
}

fn settings(write: &[&str], hide: &[&str]) -> manifest::Sandbox {
    let mut text =
        String::from("[agent]\nname = \"a\"\nmodel = \"deepseek/deepseek-flash\"\n[sandbox]\n");
    let list = |items: &[&str]| {
        items
            .iter()
            .map(|i| format!("{i:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    text.push_str(&format!(
        "write = [{}]\nhide = [{}]\n",
        list(write),
        list(hide)
    ));
    manifest::Manifest::parse(&text, Path::new("agent.toml"), Path::new("/agents/a"))
        .unwrap()
        .sandbox
        .unwrap()
}

#[test]
fn the_agents_folder_and_temp_are_written_and_the_secrets_there_are_hidden() {
    let p = places();
    let temp = p.root.join("tmp");
    std::fs::create_dir(&temp).unwrap();
    let inside = p.cwd.join("sub");
    std::fs::create_dir(&inside).unwrap();
    let out = p.root.join("out");
    let rules = Rules::of(
        &settings(
            &[&out.display().to_string(), "~/.cache"],
            &["~/.config/none"],
        ),
        &p.cwd,
        &p.home,
        // One not there is left out.
        &[temp.clone(), inside, p.root.join("gone")],
    );
    // `~/.cache` is not there.
    assert_eq!(
        rules.unwrap_err(),
        format!(
            "[sandbox] write {}: not there",
            p.home.join(".cache").display()
        )
    );
    std::fs::create_dir(p.home.join(".cache")).unwrap();
    let rules = Rules::of(
        &settings(
            &[&out.display().to_string(), "~/.cache"],
            &["~/.config/none"],
        ),
        &p.cwd,
        &p.home,
        &[temp.clone(), p.cwd.join("sub"), temp.clone()],
    )
    .unwrap();
    // Each once, sorted; one inside another kept, so the note names it.
    let mut write = vec![
        p.cwd.clone(),
        p.cwd.join("sub"),
        temp,
        out,
        p.home.join(".cache"),
    ];
    write.sort();
    assert_eq!(rules.write, write);
    // The defaults that are there: a folder and a file.
    assert_eq!(
        rules.hide,
        [
            Hidden {
                path: p.home.join(".ssh"),
                folder: true
            },
            Hidden {
                path: p.home.join(".netrc"),
                folder: false
            },
        ]
    );
    assert_eq!(
        rules.notes,
        [format!(
            "[sandbox] hide {}: not there, so nothing is hidden for it",
            p.home.join(".config/none").display()
        )]
    );
}

#[test]
fn what_would_undo_the_sandbox_is_refused() {
    let p = places();
    let none = settings(&[], &[]);
    // The agent working in the home folder, or above it.
    for cwd in [&p.home, &p.root] {
        assert_eq!(
            Rules::of(&none, cwd, &p.home, &[]).unwrap_err(),
            format!(
                "the sandbox: the agent's folder {} holds your home folder, which plugins could then write: run the agent in a folder of its own",
                cwd.display()
            )
        );
    }
    // A writable folder holding the home folder; one that is a file.
    let root = p.root.display().to_string();
    assert_eq!(
        Rules::of(&settings(&[&root], &[]), &p.cwd, &p.home, &[]).unwrap_err(),
        format!("[sandbox] write {root}: it holds your home folder: name the folders plugins need")
    );
    let netrc = p.home.join(".netrc").display().to_string();
    assert_eq!(
        Rules::of(&settings(&[&netrc], &[]), &p.cwd, &p.home, &[]).unwrap_err(),
        format!("[sandbox] write {netrc}: not a folder")
    );
    // Hiding the agent's folder, or a folder plugins may write.
    let cwd = p.cwd.display().to_string();
    assert_eq!(
        Rules::of(&settings(&[], &[&cwd]), &p.cwd, &p.home, &[]).unwrap_err(),
        format!(
            "[sandbox] hide {cwd}: it holds the agent's folder: hide what is inside it instead"
        )
    );
    let out = p.root.join("out");
    let shown = out.display().to_string();
    assert_eq!(
        Rules::of(&settings(&[&shown], &[&root]), &p.cwd, &p.home, &[]).unwrap_err(),
        format!(
            "[sandbox] hide {root}: it holds {shown}, which plugins may write: hide what is inside it instead"
        )
    );
}

/// Rules of one writable folder and one hidden folder and file.
fn small() -> Rules {
    Rules {
        write: vec!["/w/a \"b\"".into()],
        hide: vec![
            Hidden {
                path: "/h/.ssh".into(),
                folder: true,
            },
            Hidden {
                path: "/h/.netrc".into(),
                folder: false,
            },
        ],
        notes: Vec::new(),
    }
}

#[test]
fn bubblewraps_command_binds_the_writable_folders_then_covers_the_hidden() {
    let sandbox = Sandbox {
        tool: Tool::Bwrap("/usr/bin/bwrap".into()),
        rules: small(),
    };
    let (program, args) = sandbox.wrap("node".into(), vec!["x.js".into()]);
    assert_eq!(program, "/usr/bin/bwrap");
    assert_eq!(
        args,
        [
            "--die-with-parent", "--unshare-pid", "--ro-bind", "/", "/", "--dev", "/dev", "--proc",
            "/proc", "--bind", "/w/a \"b\"", "/w/a \"b\"", "--tmpfs", "/h/.ssh", "--remount-ro",
            "/h/.ssh", "--ro-bind", "/dev/null", "/h/.netrc", "--", "node", "x.js",
        ]
    );
}

#[test]
fn sandbox_execs_profile_allows_writing_only_where_it_says() {
    let sandbox = Sandbox {
        tool: Tool::SandboxExec,
        rules: small(),
    };
    let (program, args) = sandbox.wrap_text("node".into(), vec!["x.js".into()]);
    assert_eq!(program, "/usr/bin/sandbox-exec");
    assert_eq!(args.len(), 4);
    assert_eq!(
        (args[0].as_str(), &args[2..]),
        ("-p", &["node".to_owned(), "x.js".to_owned()][..])
    );
    assert_eq!(
        args[1],
        r#"(version 1)
(allow default)
(deny file-write*)
(allow file-write*
  (subpath "/w/a \"b\"")
  (literal "/dev/null")
  (literal "/dev/zero")
  (literal "/dev/random")
  (literal "/dev/urandom")
  (literal "/dev/tty")
  (literal "/dev/dtracehelper")
  (regex #"^/dev/ttys[0-9]+$")
  (regex #"^/dev/fd/[0-9]+$"))
(deny file-read* file-write*
  (subpath "/h/.ssh")
  (subpath "/h/.netrc"))
"#
    );
}

/// Whether this system's sandbox can be tried here: `false` (said) when
/// its tool is missing, unless `YOBUILD_REQUIRE_SANDBOX=1` (CI), which
/// fails instead.
pub(crate) fn available() -> bool {
    match Tool::here() {
        Ok(_) => true,
        Err(why) if std::env::var_os("YOBUILD_REQUIRE_SANDBOX").is_some_and(|v| v == "1") => {
            panic!("YOBUILD_REQUIRE_SANDBOX=1: {why}")
        }
        Err(why) => {
            eprintln!("skipped: {why}");
            false
        }
    }
}

#[cfg(unix)]
#[test]
fn a_confined_shell_writes_only_where_the_rules_say_and_reads_nothing_hidden() {
    if !available() {
        return;
    }
    let p = places();
    std::fs::write(p.home.join(".ssh/id"), "key\n").unwrap();
    let out = p.root.join("out");
    let sandbox = Sandbox {
        tool: Tool::here().unwrap(),
        rules: Rules::of(&settings(&[], &[]), &p.cwd, &p.home, &[]).unwrap(),
    };
    sandbox.tried().unwrap();
    let script = format!(
        "echo in > {cwd}/in.txt && echo wrote-in; echo out > {out}/out.txt && echo wrote-out; \
         cat {home}/.ssh/id && echo read-ssh; cat {home}/.netrc && echo read-netrc; \
         echo null > /dev/null && echo devnull",
        cwd = p.cwd.display(),
        out = out.display(),
        home = p.home.display(),
    );
    let (program, args) = sandbox.wrap("/bin/sh".into(), vec!["-c".into(), script.into()]);
    let output = std::process::Command::new(program)
        .args(args)
        .current_dir(&p.cwd)
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        said.lines().collect::<Vec<_>>(),
        ["wrote-in", "devnull"],
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(p.cwd.join("in.txt")).unwrap(),
        "in\n"
    );
    assert!(!out.join("out.txt").exists());
}
