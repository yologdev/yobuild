//! `yobuild new` (design §13 "New commands"): a new agent's folder, either a
//! commented starter to fill in, or a copy of an agent built into the
//! binary — the gallery's, built through the skill and checked in CI, and
//! the examples — so a working agent needs no repository.
//!
//! A copy is the agent as it was built (the gallery's are a coding agent's
//! work, never edited by hand), but for two things said when it ends: its
//! `[agent] name` becomes the new folder's name (its scratch and log are
//! its own), and its README's `path/to/gallery/<agent>/` names the new
//! folder.

use std::path::Path;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// One file of a built-in agent: where it goes in the new folder, its bytes.
type File = (&'static str, &'static [u8]);

/// An agent built into the binary.
#[derive(Debug)]
#[non_exhaustive]
pub struct Builtin {
    pub name: &'static str,
    /// `gallery` or `example`.
    pub kind: &'static str,
    /// One line on what it does.
    pub summary: &'static str,
    files: &'static [File],
}

macro_rules! file {
    ($to:expr, $from:expr) => {
        (
            $to,
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/", $from)) as &[u8],
        )
    };
}

/// A gallery agent's files: its folder as it is.
macro_rules! gallery {
    // An agent with files of its own beyond these (a rutis plugin's
    // package, `plugins/`), each named.
    (unlocked $name:literal, [$($script:literal),*], [$($own:literal),*]) => {
        &[
            file!("agent.toml", concat!("gallery/", $name, "/agent.toml")),
            file!("prompts/agent.md", concat!("gallery/", $name, "/prompts/agent.md")),
            file!("README.md", concat!("gallery/", $name, "/README.md")),
            $(
                file!(concat!("scripts/", $script, ".json"), concat!("gallery/", $name, "/scripts/", $script, ".json")),
                file!(concat!("scripts/", $script, ".task"), concat!("gallery/", $name, "/scripts/", $script, ".task")),
            )*
            $(
                file!($own, concat!("gallery/", $name, "/", $own)),
            )*
        ]
    };
    // An agent that pins nothing has no lock.
    (unlocked $name:literal, [$($script:literal),*]) => {
        &[
            file!("agent.toml", concat!("gallery/", $name, "/agent.toml")),
            file!("prompts/agent.md", concat!("gallery/", $name, "/prompts/agent.md")),
            file!("README.md", concat!("gallery/", $name, "/README.md")),
            $(
                file!(concat!("scripts/", $script, ".json"), concat!("gallery/", $name, "/scripts/", $script, ".json")),
                file!(concat!("scripts/", $script, ".task"), concat!("gallery/", $name, "/scripts/", $script, ".task")),
            )*
        ]
    };
    ($name:literal, [$($script:literal),*]) => {
        &[
            file!("agent.toml", concat!("gallery/", $name, "/agent.toml")),
            file!("agent.lock", concat!("gallery/", $name, "/agent.lock")),
            file!("prompts/agent.md", concat!("gallery/", $name, "/prompts/agent.md")),
            file!("README.md", concat!("gallery/", $name, "/README.md")),
            $(
                file!(concat!("scripts/", $script, ".json"), concat!("gallery/", $name, "/scripts/", $script, ".json")),
                file!(concat!("scripts/", $script, ".task"), concat!("gallery/", $name, "/scripts/", $script, ".task")),
            )*
        ]
    };
}

/// The agents `--from` copies.
pub const BUILTIN: &[Builtin] = &[
    Builtin {
        name: "web-research",
        kind: "gallery",
        summary: "researches a question on the web and reports with its sources (DSH free search)",
        files: gallery!("web-research", ["demo", "broad", "answer"]),
    },
    Builtin {
        name: "notes-keeper",
        kind: "gallery",
        summary: "answers from a folder of notes and tidies them, asking before each write (an MCP server)",
        files: gallery!("notes-keeper", ["demo", "question"]),
    },
    Builtin {
        name: "code-reviewer",
        kind: "gallery",
        summary: "reviews a TypeScript project's code quality, read-only (ESLint's MCP server)",
        files: gallery!("code-reviewer", ["demo", "no-writes"]),
    },
    Builtin {
        name: "spreadsheet-clerk",
        kind: "gallery",
        summary: "turns a bank's CSV export into a spreadsheet with totals (DSH univer office)",
        files: gallery!("spreadsheet-clerk", ["demo", "which", "answer", "exists"]),
    },
    Builtin {
        name: "code-health",
        kind: "gallery",
        summary: "finds a TypeScript project's type errors and outlines it, read-only (pi-lens)",
        files: gallery!("code-health", ["demo", "question"]),
    },
    Builtin {
        name: "self-extending-reviewer",
        kind: "gallery",
        summary: "reviews a JavaScript or TypeScript project, read-only, loading a linter it finds when you allow it ([extend])",
        files: gallery!(unlocked "self-extending-reviewer", ["demo", "blocked", "not-offered"]),
    },
    Builtin {
        name: "project-notebook",
        kind: "gallery",
        summary: "answers questions about a project and remembers your decisions, loading a pi memory package it finds (asks you first)",
        files: gallery!(unlocked "project-notebook", ["demo", "blocked", "not-offered"]),
    },
    Builtin {
        name: "docs-helper",
        kind: "gallery",
        summary: "answers how to use a library at the version a project depends on, from docs it connects to when you allow it (Context7, remote)",
        files: gallery!(unlocked "docs-helper", ["demo", "blocked", "not-offered"]),
    },
    Builtin {
        name: "release-notes",
        kind: "gallery",
        summary: "drafts release notes from the commits since the last version tag, reading git through a small tool of its own (a rutis plugin)",
        files: gallery!(
            unlocked "release-notes",
            ["demo", "no-tag"],
            [".gitignore", "plugins/package.json", "plugins/package-lock.json", "plugins/release-history.ts"]
        ),
    },
    Builtin {
        name: "handoff",
        kind: "example",
        summary: "takes a task and works on it alone, asking without waiting, ending with a report",
        files: &[
            file!("agent.toml", "examples/handoff.toml"),
            file!("agent.lock", "examples/handoff.lock"),
            file!("prompts/handoff.md", "examples/prompts/handoff.md"),
            file!("scripts/demo.json", "examples/scripts/handoff.json"),
            file!("scripts/demo.task", "examples/scripts/handoff.task"),
        ],
    },
    Builtin {
        name: "coding",
        kind: "example",
        summary: "a coding agent in the terminal, a pi policy judging its commands",
        files: &[
            file!("agent.toml", "examples/coding.toml"),
            file!("agent.lock", "examples/coding.lock"),
            file!("prompts/coding.md", "examples/prompts/coding.md"),
            file!("scripts/demo.json", "examples/scripts/coding.json"),
            file!("scripts/demo.task", "examples/scripts/coding.task"),
        ],
    },
    Builtin {
        name: "minimal",
        kind: "example",
        summary: "the smallest: reads its folder, no plugins, no Node",
        files: &[
            file!("agent.toml", "examples/minimal.toml"),
            file!("prompts/minimal.md", "examples/prompts/minimal.md"),
            file!("scripts/demo.json", "examples/scripts/minimal.json"),
            file!("scripts/demo.task", "examples/scripts/minimal.task"),
        ],
    },
];

/// The starter `yobuild new <name>` writes: a manifest whose every other
/// key is shown commented, a prompt to fill in, and a demo reply file.
const STARTER_MANIFEST: &str = r##"# NAME: an agent yobuild runs. Every key is explained in the yobuild
# skill's reference/manifest.md (`yobuild skill install` puts the skill
# where a coding agent reads it).
#
#   yobuild check agent.toml                                    # vet it
#   yobuild run agent.toml --scripted scripts/demo.json --once "$(cat scripts/demo.task)"
#   DEEPSEEK_API_KEY=… yobuild run agent.toml                   # live
#
# It works in the folder `yobuild run` starts in.

[agent]
name   = "NAME"
model  = "deepseek/deepseek-flash"   # needs DEEPSEEK_API_KEY; any provider/model yoagent knows
prompt = "prompts/agent.md"

[limits]
budget_usd         = 0.25   # dollars a run
session_budget_usd = 1.0    # dollars in all, until yobuild stops
# max_turns   = 30          # model turns a run
# max_minutes = 15          # minutes a run
# max_tokens  = 2000000     # tokens a run

[builtins]
tools     = "read-only"     # read, list and search only; "coding" adds bash and the file writers, which the gate judges
ask_owner = true            # it may ask you, without waiting for the answer
report    = "handoff"       # each run ends with Done, Waiting on you, Decisions, Next
# facts   = true            # after each run, what changed in the folder's git repository
# scratch = true            # a folder of its own for what is not a deliverable ({scratch} in the prompt)
# gate    = "off"           # every tool call runs unjudged (no `allow` lists then)

[frontend]
web = true                  # the web page, at 127.0.0.1:8787
# phone    = true           # the page on every interface, for a phone on the same Wi-Fi
# terminal = true           # the terminal UI instead of the page (then remove `web`)

# A plugin: a pi extension, a DSH plugin or a rutis plugin, pinned.
# [[plugin]]
# pi    = "@juicesharp/rpiv-todo@2.12.0"
# allow = ["todo"]          # its tools that run without asking

# An MCP server, from npm or a path.
# [[mcp]]
# name  = "files"
# npm   = "@modelcontextprotocol/server-filesystem@1.0.0"
# args  = ["."]
# allow = ["read_text_file"]

# The agent may search for an MCP server it lacks and load it for the
# session, asking you before it starts and again with its tools.
# [extend]
# max_loads    = 2          # servers a session
# min_age_days = 7          # only versions published at least this many days ago
"##;

const STARTER_PROMPT: &str = "You are NAME, an agent working in {cwd}.

Say here what it does, for whom, and how it should work: what to read
first, what to ask the owner (`ask_owner`) and what to decide itself, what
a good result looks like.
";

const STARTER_DEMO: &str = r###"[
  { "tools": [{ "name": "list_files", "arguments": { "path": "." } }] },
  { "text": "## Done\n- Looked at the folder.\n## Waiting on you\n- nothing\n## Decisions\n- none\n## Next\n- Fill in the prompt." }
]
"###;

const STARTER_TASK: &str = "Look at this folder.\n";

/// What `new` made: the files written, and what it changed in a copy.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct Made {
    pub files: Vec<String>,
    pub changed: Vec<String>,
}

/// A new agent's folder at `dir`, named after it: the starter, or a copy
/// of the built-in agent `from`. Refused when `dir` exists, when its name
/// cannot name an agent, or for an unknown agent (naming those there are).
pub fn new_agent(dir: &Path, from: Option<&str>) -> Result<Made, BoxError> {
    // `research/` is `research`: its name, and the paths said in a copy.
    let dir: std::path::PathBuf = dir.components().collect();
    let dir = dir.as_path();
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let fine = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    if dir.exists() {
        return Err(format!("{} exists: give a new folder", dir.display()).into());
    }
    if !fine {
        return Err(format!(
            "{}: an agent's folder is named after it: letters, digits, `-`, `_`, `.`",
            dir.display()
        )
        .into());
    }
    let files: Vec<(String, Vec<u8>)> = match from {
        None => vec![
            (
                "agent.toml".into(),
                STARTER_MANIFEST.replace("NAME", &name).into_bytes(),
            ),
            (
                "prompts/agent.md".into(),
                STARTER_PROMPT.replace("NAME", &name).into_bytes(),
            ),
            ("scripts/demo.json".into(), STARTER_DEMO.as_bytes().to_vec()),
            ("scripts/demo.task".into(), STARTER_TASK.as_bytes().to_vec()),
        ],
        Some(from) => {
            let builtin = BUILTIN.iter().find(|b| b.name == from).ok_or_else(|| {
                let names: Vec<&str> = BUILTIN.iter().map(|b| b.name).collect();
                format!(
                    "no built-in agent `{from}`: there are {} (`yobuild new --list`)",
                    names.join(", ")
                )
            })?;
            builtin
                .files
                .iter()
                .map(|(path, bytes)| (path.to_string(), bytes.to_vec()))
                .collect()
        }
    };
    let mut made = Made::default();
    for (path, mut bytes) in files {
        if let Some(from) = from {
            match path.as_str() {
                "agent.toml" => {
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    let (renamed, old) = rename_agent(&text, &name)
                        .ok_or_else(|| format!("built-in agent `{from}`: no [agent] name"))?;
                    if old != name {
                        made.changed
                            .push(format!("agent.toml: [agent] name `{old}` → `{name}`"));
                    }
                    bytes = renamed.into_bytes();
                }
                "README.md" => {
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    let at = format!("path/to/gallery/{from}/");
                    if text.contains(&at) {
                        // Absolute: its commands run from the folder the
                        // agent works in, often another.
                        let to = format!("{}/", std::path::absolute(dir)?.display());
                        bytes = text.replace(&at, &to).into_bytes();
                        made.changed.push(format!("README.md: `{at}` → `{to}`"));
                    }
                }
                _ => {}
            }
        }
        let file = dir.join(&path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(&file, bytes).map_err(|e| format!("{}: {e}", file.display()))?;
        made.files.push(path);
    }
    Ok(made)
}

/// `text` with its `[agent]` table's `name` set to `name`, and the name it
/// had; `None` without one. Only the quoted value changes: the line keeps
/// its alignment and comment.
fn rename_agent(text: &str, name: &str) -> Option<(String, String)> {
    let mut in_agent = false;
    let mut old = None;
    let lines: Vec<String> = text
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with('[') {
                in_agent = trimmed.starts_with("[agent]");
            } else if in_agent && old.is_none() {
                if let Some(rest) = trimmed.strip_prefix("name") {
                    if rest.trim_start().starts_with('=') {
                        let open = line.find('"')?;
                        let close = open + 1 + line[open + 1..].find('"')?;
                        old = Some(line[open + 1..close].to_owned());
                        return Some(format!("{}\"{name}\"{}", &line[..open], &line[close + 1..]));
                    }
                }
            }
            Some(line.to_owned())
        })
        .collect::<Option<_>>()?;
    let mut out = lines.join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    Some((out, old?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;

    /// Each built-in agent holds exactly the files its folder has (the
    /// gallery's) or its pieces (an example's), as they are on disk.
    #[test]
    fn the_built_in_agents_are_the_repositorys() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        for agent in BUILTIN.iter().filter(|a| a.kind == "gallery") {
            let dir = repo.join("gallery").join(agent.name);
            let mut on_disk: Vec<String> = walk(&dir)
                .into_iter()
                .map(|p| p.strip_prefix(&dir).unwrap().display().to_string())
                .collect();
            on_disk.sort();
            let mut listed: Vec<String> = agent.files.iter().map(|(p, _)| p.to_string()).collect();
            listed.sort();
            assert_eq!(listed, on_disk, "{}", agent.name);
            for (path, bytes) in agent.files {
                assert_eq!(std::fs::read(dir.join(path)).unwrap(), *bytes, "{path}");
            }
        }
        let mut gallery: Vec<String> = std::fs::read_dir(repo.join("gallery"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        gallery.sort();
        let mut listed: Vec<String> = BUILTIN
            .iter()
            .filter(|a| a.kind == "gallery")
            .map(|a| a.name.to_owned())
            .collect();
        listed.sort();
        assert_eq!(listed, gallery, "every gallery agent is built in");
    }

    fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            // A plugin's package installed in place (`npm ci`) is not the
            // agent's.
            if path.ends_with("node_modules") {
                continue;
            }
            match path.is_dir() {
                true => out.extend(walk(&path)),
                false => out.push(path),
            }
        }
        out
    }

    /// The starter reads, named after its folder, and so does each key it
    /// shows commented, uncommented (the plugin and server blocks whole).
    #[test]
    fn the_starter_reads_with_each_commented_key() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("my-agent");
        let made = new_agent(&dir, None).unwrap();
        assert_eq!(
            made.files,
            ["agent.toml", "prompts/agent.md", "scripts/demo.json", "scripts/demo.task"]
        );
        let path = dir.join("agent.toml");
        let text = std::fs::read_to_string(&path).unwrap();
        let read = |text: &str| Manifest::parse(text, &path, &dir);
        let m = read(&text).unwrap();
        assert_eq!(m.agent.name, "my-agent");
        assert!(std::fs::read_to_string(dir.join("prompts/agent.md"))
            .unwrap()
            .starts_with("You are my-agent, an agent working in {cwd}."));
        let lines: Vec<&str> = text.lines().collect();
        let mut tried = 0;
        for (at, line) in lines.iter().enumerate() {
            // The blocks below are tried whole.
            if line.starts_with("# [[") {
                break;
            }
            let Some(key) = line.strip_prefix("# ") else {
                continue;
            };
            if !key.contains(" = ") || key.starts_with('[') {
                continue;
            }
            let mut changed: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
            changed[at] = key.to_owned();
            if key.starts_with("terminal") {
                changed.retain(|l| !l.starts_with("web = true"));
            }
            read(&changed.join("\n")).unwrap_or_else(|e| panic!("{key}: {e}"));
            tried += 1;
        }
        assert!(tried >= 8, "{tried}");
        // The blocks, whole.
        for block in ["# [[plugin]]", "# [[mcp]]", "# [extend]"] {
            let start = lines.iter().position(|l| *l == block).unwrap();
            let mut changed: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
            for line in changed.iter_mut().skip(start) {
                match line.strip_prefix("# ") {
                    Some(rest) if !line.is_empty() => *line = rest.to_owned(),
                    _ => break,
                }
            }
            // `[extend]`'s keys are its defaults: set others, to see them read.
            if block == "# [extend]" {
                for line in changed.iter_mut() {
                    if line.starts_with("max_loads") {
                        *line = "max_loads = 3".into();
                    } else if line.starts_with("min_age_days") {
                        *line = "min_age_days = 9".into();
                    }
                }
            }
            let m = read(&changed.join("\n")).unwrap_or_else(|e| panic!("{block}: {e}"));
            if block == "# [extend]" {
                let extend = m.extend.expect("[extend]");
                assert_eq!((extend.max_loads, extend.min_age_days), (3, 9));
            }
        }
        // Its demo is a reply file.
        let demo = std::fs::read_to_string(dir.join("scripts/demo.json")).unwrap();
        assert!(serde_json::from_str::<serde_json::Value>(&demo).is_ok());
    }

    /// A copy: its files, its name the folder's, its README's paths the
    /// folder's; each built-in reads as a manifest from the copy.
    #[test]
    fn a_copy_is_the_agent_named_after_its_folder() {
        let parent = tempfile::tempdir().unwrap();
        for agent in BUILTIN {
            let dir = parent.path().join(format!("my-{}", agent.name));
            let made = new_agent(&dir, Some(agent.name)).unwrap();
            assert_eq!(made.files.len(), agent.files.len());
            let path = dir.join("agent.toml");
            let m = Manifest::load(&path).unwrap_or_else(|e| panic!("{}: {e}", agent.name));
            assert_eq!(m.agent.name, format!("my-{}", agent.name));
            let built =
                Manifest::parse(std::str::from_utf8(agent.files[0].1).unwrap(), &path, &dir)
                    .unwrap();
            assert_eq!(
                made.changed[0],
                format!(
                    "agent.toml: [agent] name `{}` → `my-{}`",
                    built.agent.name, agent.name
                )
            );
            if agent.kind == "gallery" {
                // Its README names the copy (absolutely) and no repository.
                let readme = std::fs::read_to_string(dir.join("README.md")).unwrap();
                for repository in ["path/to/gallery/", "path/to/yobuild", "../../", "checkout"] {
                    assert!(!readme.contains(repository), "{}: {repository}", agent.name);
                }
                assert!(
                    readme.contains(&format!("{}/agent.toml", dir.display())),
                    "{}",
                    agent.name
                );
            }
        }
    }

    #[test]
    fn new_refuses_a_folder_there_a_bad_name_or_an_unknown_agent() {
        let parent = tempfile::tempdir().unwrap();
        let e = new_agent(parent.path(), None).unwrap_err().to_string();
        assert!(e.ends_with("exists: give a new folder"), "{e}");
        let e = new_agent(&parent.path().join("my agent"), None)
            .unwrap_err()
            .to_string();
        assert!(e.contains("letters, digits"), "{e}");
        let e = new_agent(&parent.path().join("a"), Some("nope"))
            .unwrap_err()
            .to_string();
        assert!(
            e.starts_with("no built-in agent `nope`: there are web-research, notes-keeper,"),
            "{e}"
        );
        assert!(!parent.path().join("a").exists());
    }

    /// `research/` names the folder `research`, the README's paths with one
    /// `/`.
    #[test]
    fn a_trailing_slash_is_the_folder() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("research");
        let made = new_agent(
            Path::new(&format!("{}/", dir.display())),
            Some("web-research"),
        )
        .unwrap();
        assert!(
            made.changed[0].ends_with("→ `research`"),
            "{:?}",
            made.changed
        );
        let readme = std::fs::read_to_string(dir.join("README.md")).unwrap();
        assert!(!readme.contains("research//"), "{readme}");
    }

    #[test]
    fn renaming_keeps_the_line() {
        let (out, old) = rename_agent(
            "# x\n[agent]\nname   = \"old\"   # its name\nmodel = \"m\"\n[[plugin]]\nname = \"p\"\n",
            "new",
        )
        .unwrap();
        assert_eq!(old, "old");
        assert_eq!(
            out,
            "# x\n[agent]\nname   = \"new\"   # its name\nmodel = \"m\"\n[[plugin]]\nname = \"p\"\n"
        );
        assert!(rename_agent("[agent]\nmodel = \"m\"\n", "x").is_none());
    }
}
