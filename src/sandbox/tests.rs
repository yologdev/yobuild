//! The sandbox's rules and command lines, and on a system that has the
//! tool, a shell confined by them.

use super::*;

/// A home folder, an agent's folder (a repository) and folders around
/// them, all real.
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
    for folder in [
        &home,
        &cwd,
        &home.join(".ssh"),
        &root.join("out"),
        &cwd.join(".git"),
    ] {
        std::fs::create_dir_all(folder).unwrap();
    }
    std::fs::write(home.join(".netrc"), "machine x password p\n").unwrap();
    std::fs::write(cwd.join(".git/config"), "[core]\n").unwrap();
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

/// Nothing around but these temp folders.
fn temp(folders: &[PathBuf]) -> Around {
    Around {
        temp: folders.to_vec(),
        ..Around::default()
    }
}

#[test]
fn the_agents_folder_and_temp_are_written_and_the_secrets_there_are_hidden() {
    let p = places();
    let temp_dir = p.root.join("tmp");
    std::fs::create_dir(&temp_dir).unwrap();
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
        &temp(&[temp_dir.clone(), inside, p.root.join("gone")]),
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
    std::fs::write(p.home.join(".zsh_history"), "export KEY=x\n").unwrap();
    let rules = Rules::of(
        &settings(
            &[&out.display().to_string(), "~/.cache"],
            &["~/.config/none"],
        ),
        &p.cwd,
        &p.home,
        &temp(&[temp_dir.clone(), p.cwd.join("sub"), temp_dir.clone()]),
    )
    .unwrap();
    // Each once, sorted; one inside another kept, so the note names it.
    let mut write = vec![
        p.cwd.clone(),
        p.cwd.join("sub"),
        temp_dir,
        out,
        p.home.join(".cache"),
    ];
    write.sort();
    assert_eq!(rules.write, write);
    // The repository's git folder, read-only though in the agent's folder.
    assert_eq!(rules.readonly, [p.cwd.join(".git")]);
    assert_eq!(rules.own_git, p.cwd.join(".git"));
    // The defaults that are there: a folder, a login file, a history.
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
            Hidden {
                path: p.home.join(".zsh_history"),
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
fn a_worktrees_git_file_its_folder_and_the_main_repository_are_read_only() {
    let p = places();
    // The agent works in a folder of a worktree whose git folder is in the
    // main repository's (`commondir`).
    let main = p.root.join("main/.git");
    let gitdir = main.join("worktrees/w");
    std::fs::create_dir_all(&gitdir).unwrap();
    std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
    let tree = p.root.join("w");
    std::fs::create_dir_all(tree.join("src")).unwrap();
    std::fs::write(tree.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
    assert_eq!(
        git_dirs(&tree.join("src")),
        [main.clone(), gitdir, tree.join(".git")]
    );
    // No repository: nothing.
    assert!(git_dirs(&p.root.join("out")).is_empty());
}

#[test]
fn what_would_undo_the_sandbox_is_refused() {
    let p = places();
    let none = settings(&[], &[]);
    let nothing = Around::default();
    // The agent working in the home folder, or above it.
    for cwd in [&p.home, &p.root] {
        assert_eq!(
            Rules::of(&none, cwd, &p.home, &nothing).unwrap_err(),
            format!(
                "the sandbox: the agent's folder {} holds your home folder, which plugins could then write: run the agent in a folder of its own",
                cwd.display()
            )
        );
    }
    // A writable folder holding the home folder; one that is a file.
    let root = p.root.display().to_string();
    assert_eq!(
        Rules::of(&settings(&[&root], &[]), &p.cwd, &p.home, &nothing).unwrap_err(),
        format!("[sandbox] write {root}: it holds your home folder, which plugins could then write: name a folder of its own")
    );
    let netrc = p.home.join(".netrc").display().to_string();
    assert_eq!(
        Rules::of(&settings(&[&netrc], &[]), &p.cwd, &p.home, &nothing).unwrap_err(),
        format!("[sandbox] write {netrc}: not a folder")
    );
    // `YOAGENT_DSH_HOME` checked as `write` is: holding the home folder, or
    // inside a hidden one.
    let dsh = |path: &Path| Around {
        dsh_home: Some(path.to_owned()),
        ..Around::default()
    };
    assert_eq!(
        Rules::of(&none, &p.cwd, &p.home, &dsh(&p.root)).unwrap_err(),
        format!("YOAGENT_DSH_HOME {root}: it holds your home folder, which plugins could then write: name a folder of its own")
    );
    let in_ssh = p.home.join(".ssh/dsh");
    std::fs::create_dir(&in_ssh).unwrap();
    assert_eq!(
        Rules::of(&none, &p.cwd, &p.home, &dsh(&in_ssh)).unwrap_err(),
        format!(
            "[sandbox] hide {}: it holds {}, which plugins may write: hide what is inside it instead",
            p.home.join(".ssh").display(),
            in_ssh.display()
        )
    );
    // Hiding the agent's folder, or a folder plugins may write.
    let cwd = p.cwd.display().to_string();
    assert_eq!(
        Rules::of(&settings(&[], &[&cwd]), &p.cwd, &p.home, &nothing).unwrap_err(),
        format!(
            "[sandbox] hide {cwd}: it holds the agent's folder: hide what is inside it instead"
        )
    );
    let out = p.root.join("out");
    let shown = out.display().to_string();
    assert_eq!(
        Rules::of(&settings(&[&shown], &[&root]), &p.cwd, &p.home, &nothing).unwrap_err(),
        format!(
            "[sandbox] hide {root}: it holds {shown}, which plugins may write: hide what is inside it instead"
        )
    );
}

#[test]
fn the_runtime_folder_is_covered_unless_something_writable_is_in_it() {
    let p = places();
    let runtime = p.root.join("run-user");
    std::fs::create_dir(&runtime).unwrap();
    let around = |temp: Vec<PathBuf>| Around {
        temp,
        runtime: Some(runtime.clone()),
        ..Around::default()
    };
    let rules = Rules::of(&settings(&[], &[]), &p.cwd, &p.home, &around(Vec::new())).unwrap();
    assert_eq!(rules.runtime.as_deref(), Some(runtime.as_path()));
    let rules = Rules::of(
        &settings(&[], &[]),
        &p.cwd,
        &p.home,
        &around(vec![runtime.clone()]),
    )
    .unwrap();
    assert_eq!(rules.runtime, None);
}

/// Rules of one writable folder, a git folder in it, the runtime folder,
/// and one hidden folder and file.
fn small() -> Rules {
    Rules {
        write: vec!["/w/a \"b\"".into()],
        readonly: vec!["/w/a \"b\"/.git".into()],
        own_git: "/w/a \"b\"/.git".into(),
        runtime: Some("/run/user/1000".into()),
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
fn bubblewraps_command_binds_the_writable_folders_then_covers_the_rest() {
    let sandbox = Sandbox {
        tool: Tool::Bwrap("/usr/bin/bwrap".into()),
        rules: small(),
    };
    let (program, args) = sandbox.wrap("node".into(), vec!["x.js".into()]);
    assert_eq!(program, "/usr/bin/bwrap");
    assert_eq!(
        args,
        [
            "--new-session", "--die-with-parent", "--unshare-pid", "--ro-bind", "/", "/", "--dev",
            "/dev", "--proc", "/proc", "--bind", "/w/a \"b\"", "/w/a \"b\"", "--ro-bind",
            "/w/a \"b\"/.git", "/w/a \"b\"/.git", "--tmpfs", "/run/user/1000", "--tmpfs",
            "/h/.ssh", "--remount-ro", "/h/.ssh", "--ro-bind", "/dev/null", "/h/.netrc", "--",
            "node", "x.js",
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
  (literal "/dev/dtracehelper")
  (regex #"^/dev/fd/[0-9]+$"))
(deny file-write*
  (subpath "/w/a \"b\"/.git"))
(deny file-read* file-write* file-ioctl
  (literal "/dev/tty")
  (regex #"^/dev/ttys[0-9]+$"))
(deny file-read* file-write*
  (subpath "/h/.ssh")
  (subpath "/h/.netrc"))
(deny network-outbound
  (remote unix-socket (path-regex #"^/private/tmp/com\.apple\.launchd\.[^/]+/Listeners$")))
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
#[tokio::test]
async fn a_confined_shell_writes_only_where_the_rules_say_and_reaches_nothing_hidden() {
    if !available() {
        return;
    }
    let p = places();
    std::fs::write(p.home.join(".ssh/id"), "key\n").unwrap();
    let out = p.root.join("out");
    let sandbox = Sandbox {
        tool: Tool::here().unwrap(),
        rules: Rules::of(&settings(&[], &[]), &p.cwd, &p.home, &Around::default()).unwrap(),
    };
    sandbox.tried().await.unwrap();
    let script = format!(
        "echo in > {cwd}/in.txt && echo wrote-in; echo out > {out}/out.txt && echo wrote-out; \
         echo '[core] fsmonitor = ./x.sh' >> {cwd}/.git/config && echo wrote-git-config; \
         echo h > {cwd}/.git/hooks-new && echo wrote-git; \
         cat {home}/.ssh/id && echo read-ssh; cat {home}/.netrc && echo read-netrc; \
         (: < /dev/tty) && echo read-tty; \
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
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        said.lines().collect::<Vec<_>>(),
        ["wrote-in", "devnull"],
        "{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(p.cwd.join("in.txt")).unwrap(),
        "in\n"
    );
    assert!(!out.join("out.txt").exists());
    assert_eq!(
        std::fs::read_to_string(p.cwd.join(".git/config")).unwrap(),
        "[core]\n"
    );
    // The terminal refused by the sandbox (macOS), or not there in a
    // session of its own (bubblewrap).
    if cfg!(target_os = "macos") {
        assert!(
            stderr.contains("/dev/tty: Operation not permitted"),
            "{stderr}"
        );
    }
}
