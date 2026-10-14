//! The gate's decisions on real command lines: the cases of
//! `confirm-dangerous.test.mjs`, so the Rust port and the pi extension agree.
//! Each test works in a fresh project: owner files exist, a scratch folder
//! is named.

use super::*;
use serde_json::json;

struct Project {
    dir: tempfile::TempDir,
    scratch: PathBuf,
    rules: Rules,
}

impl Project {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join("sub")).unwrap();
        for f in [
            "owner.txt",
            "report.md",
            "existing.txt",
            "sub/only.txt",
            "main",
            "my file.txt",
            ".env",
            "a.md",
        ] {
            std::fs::write(root.join(f), "the owner's\n").unwrap();
        }
        let scratch = real(&root.join("scratch"));
        std::fs::create_dir(&scratch).unwrap();
        let rules = Rules {
            cwd: real(root),
            scratch: Some(scratch.clone()),
            created: Mutex::default(),
        };
        Self {
            dir,
            scratch,
            rules,
        }
    }

    /// Whether `command` is refused (nobody to ask); a free one's files
    /// become the agent's, as `before_tool` does.
    fn run(&self, command: &str) -> bool {
        self.call("bash", json!({ "command": command }))
    }

    fn write(&self, tool: &str, file: &str) -> bool {
        let path = self.dir.path().join(file);
        self.call(tool, json!({ "path": path }))
    }

    fn call(&self, tool: &str, args: Json) -> bool {
        let check = self.rules.check(tool, &args);
        if check.ask.is_none() {
            self.rules.created.lock().unwrap().extend(check.creates);
        }
        check.ask.is_some()
    }
}

fn expect(project: &Project, commands: &[String], asks: bool) {
    let wrong: Vec<&String> = commands.iter().filter(|c| project.run(c) != asks).collect();
    assert!(
        wrong.is_empty(),
        "should {}: {wrong:#?}",
        if asks { "ask" } else { "be free" }
    );
}

fn lines(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn destructive_or_overwriting_commands_ask() {
    let p = Project::new();
    let asks = lines(&[
        "/bin/rm -f owner.txt",
        "git restore owner.txt",
        "git -C . reset --hard",
        "printf changed 1>owner.txt",
        "rm -f report.md",
        "command rm owner.txt",
        "env FOO=1 rm owner.txt",
        "git reset --hard HEAD~1",
        "git clean -fd",
        "git checkout -- owner.txt",
        "git checkout .",
        "git -c core.x=y restore .",
        "git branch -D old",
        "git push --force-with-lease",
        "git push -f origin main",
        "git stash drop",
        "echo x 2>owner.txt",
        "echo x &> owner.txt",
        "echo hi | tee owner.txt",
        "mv a.md owner.txt",
        "cp a.md owner.txt",
        "rm *.html",
        "sudo ls",
        "find . -name x -delete",
        "chmod -R 777 .",
        "cd sub && echo x > only.txt",
        "cd sub && cat > only.txt <<EOF\nhello\nEOF",
        "cd sub && mv /etc/hosts only.txt",
        "echo x > ~/.zshrc",
        "echo x > $HOME/.zshrc",
        "echo x > \"my file.txt\"",
        "mv existing.txt \"my file.txt\"",
        "bash -c \"rm existing.txt\"",
        "sh -c 'rm existing.txt'",
        "echo f | xargs rm",
        "xargs rm < list",
        "find . -name x -exec rm {} \\;",
        "git rm existing.txt",
        "timeout 5 rm existing.txt",
        "nice rm existing.txt",
        "env -i rm existing.txt",
        "\\rm existing.txt",
        "echo x >| existing.txt",
        "dd of=existing.txt if=/dev/zero",
        "git push origin +main",
        "git push origin :main",
        "git checkout -f main",
        "git switch --discard-changes main",
        "cd $DIR && rm x.txt",
        "echo x >existing.txt",
        ">existing.txt",
        "echo \"unclosed > existing.txt",
        "cp a.md sub/only.txt",
    ]);
    expect(&p, &asks, true);
}

#[test]
fn everyday_commands_do_not() {
    let p = Project::new();
    let s = p.scratch.display();
    let mut free = lines(&[
        "ls -la && cat owner.txt",
        "echo hi >> owner.txt",
        "python3 -m unittest 2>/dev/null",
        "make 2>&1 | tail",
        "git status",
        "git checkout -b feature",
        "git restore --staged owner.txt",
        "git push origin main",
        "git diff",
        "echo a > new.md",
        "echo hi | tee -a owner.txt",
        "cargo test truncate",
        "grep -r truncate src",
        "git commit -m \"Use sudo-less install\"",
        "git clean -n",
        "cat > INSTALL.md <<EOF\nrun: sudo apt install x\nrm -rf /\nEOF",
        "mkdir -p out && mv a.md out/",
        "echo \"a > b\" > fresh.txt",
        "cp sub/only.txt .",
    ]);
    free.push(format!("rm -rf {s}/pages"));
    free.push(format!("cd {s} && rm -rf pages"));
    expect(&p, &free, false);
}

#[test]
fn the_agents_own_files_stay_free_and_a_refused_commands_are_not_its_own() {
    let p = Project::new();
    let root = p.dir.path();
    assert!(!p.run("cargo test 2>&1 | tee test.log"));
    std::fs::write(root.join("test.log"), "output\n").unwrap();
    assert!(
        !p.run("cargo test 2>&1 | tee test.log"),
        "its own log, again"
    );
    assert!(p.run("echo > a1.txt; echo > existing.txt"));
    std::fs::write(root.join("a1.txt"), "the owner made it\n").unwrap();
    assert!(p.run("rm a1.txt"), "a1.txt was never the agent's");
}

#[test]
fn a_cd_is_followed_only_where_it_surely_applies() {
    let p = Project::new();
    let s = p.scratch.display();
    let asks: Vec<String> = vec![
        format!("(cd {s}); rm -f owner.txt"),
        format!("(cd {s}) && rm -f pages"),
        format!("cd {s} | true; rm -f pages"),
        format!("cd {s} || rm -f pages"),
        format!("cd {s} & rm -f pages"),
        "cd no-such-dir; rm -f pages".into(),
    ];
    expect(&p, &asks, true);
    let free: Vec<String> = vec![
        format!("(cd {s} && rm -rf pages)"),
        format!("cd {s}; rm -rf pages"),
        format!("cd {s} && rm -rf pages && cd -"),
    ];
    expect(&p, &free, false);
}

#[test]
fn heredocs_are_read_as_the_shell_reads_them() {
    let p = Project::new();
    expect(
        &p,
        &lines(&[
            "echo '<<EOF'\nrm -f owner.txt\nEOF",
            "echo \"<<EOF\"; rm -f owner.txt",
            "cat > x.md <<'EOF'\nsudo things\nEOF\nrm -f owner.txt",
            "cat <<EOF > owner.txt\nnew\nEOF",
        ]),
        true,
    );
    expect(
        &p,
        &lines(&[
            "cat > notes.md <<'EOF'\nrm -rf /\nsudo reboot\nEOF",
            "cat > n2.md <<-EOF\n\tgit reset --hard\n\tEOF",
        ]),
        false,
    );
}

#[test]
fn substitutions_run_commands_and_are_read_like_any_command() {
    let p = Project::new();
    let s = p.scratch.display();
    expect(
        &p,
        &lines(&[
            "cat <<EOF\n$(rm -f owner.txt)\nEOF",
            "cat <<EOF\n`rm -f owner.txt`\nEOF",
            "cat <<-EOF > fresh2.md\n\tline $(git reset --hard)\n\tEOF",
            "echo $(rm -f owner.txt)",
            "echo \"$(rm -f owner.txt)\"",
            "echo `rm -f owner.txt`",
            "diff <(rm -f owner.txt) a.md",
            "cat a.md > >(tee owner.txt)",
            "x=$(git reset --hard)",
            "echo $(( $(rm -f owner.txt) + 1 ))",
            "eval \"rm -f owner.txt\"",
            "eval $CMD",
            "cd sub && echo $(rm -f only.txt)",
            "echo \"$(echo \"nested $(rm -f owner.txt)\")\"",
        ]),
        true,
    );
    let mut free = lines(&[
        "cat <<'EOF'\n$(rm -f owner.txt)\nEOF",
        "cat <<\\EOF\n$(rm -f owner.txt)\nEOF",
        "echo \"built at $(date)\"",
        "echo $(git rev-parse HEAD) > rev.txt",
        "cat <<EOF > fresh.md\nrev: $(git rev-parse HEAD)\nEOF",
        "eval \"echo hi\"",
        "echo $(( 1 + 2 ))",
    ]);
    free.push(format!("cd {s} && echo $(rm -rf pages)"));
    expect(&p, &free, false);
}

#[test]
fn writes_and_edits() {
    let p = Project::new();
    assert!(
        p.write("write_file", "owner.txt"),
        "overwrite an existing file"
    );
    assert!(!p.write("write_file", "fresh.md"), "a new file");
    assert!(!p.write("write_file", "fresh.md"), "its own file again");
    assert!(!p.write("edit_file", "owner.txt"), "a targeted edit");
    assert!(p.write("edit_file", ".env"), "a protected file");
    assert!(p.write("edit", ".env.local"), "pi's name, a protected file");
    assert!(p.write("write", "owner.txt"), "pi's name, an overwrite");
    assert!(
        !p.call("read_file", json!({ "path": "owner.txt" })),
        "other tools"
    );
}

#[test]
fn the_question_names_what_it_asks_about() {
    let p = Project::new();
    let check = p.rules.check("bash", &json!({ "command": "rm owner.txt" }));
    assert_eq!(check.ask.as_deref(), Some("Run `rm owner.txt`?"));
    let check = p.rules.check("write_file", &json!({ "path": "owner.txt" }));
    assert_eq!(
        check.ask.as_deref(),
        Some("Overwrite the existing file owner.txt?")
    );
}
