//! The skill (`skill/yobuild/`, design §11): what Claude Code needs to load
//! it, and links that lead somewhere.

use std::path::Path;

const SKILL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/skill/yobuild");

#[test]
fn the_skill_has_its_frontmatter_and_every_link_leads_to_a_file() {
    let text = std::fs::read_to_string(Path::new(SKILL).join("SKILL.md")).unwrap();
    // Claude Code reads `name` and `description` from the frontmatter.
    let front = text
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .map(|(front, _)| front)
        .expect("frontmatter between --- lines");
    assert!(front.lines().any(|l| l == "name: yobuild"), "{front}");
    let description = front
        .lines()
        .find_map(|l| l.strip_prefix("description: "))
        .expect("a description");
    assert!(
        description.len() <= 1024 && description.contains("Use when"),
        "{description}"
    );
    // Each relative link, from SKILL.md and from the references, exists;
    // references are one level deep.
    for file in [
        "SKILL.md", "reference/manifest.md", "reference/parts.md", "reference/scripted.md",
        "reference/failures.md",
    ] {
        let path = Path::new(SKILL).join(file);
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{file}: {e}"));
        for (at, _) in text.match_indices("](") {
            let target = &text[at + 2..];
            let target = &target[..target.find(')').unwrap()];
            if target.starts_with("http") || target.starts_with('#') {
                continue;
            }
            let base = path.parent().unwrap();
            assert!(
                base.join(target).is_file(),
                "{file} links {target}, which is not there"
            );
        }
    }
    for linked in [
        "reference/manifest.md", "reference/parts.md", "reference/scripted.md",
        "reference/failures.md",
    ] {
        assert!(
            text.contains(&format!("]({linked})")),
            "SKILL.md links {linked}"
        );
    }
}

fn reference(name: &str) -> String {
    std::fs::read_to_string(Path::new(SKILL).join("reference").join(name)).unwrap()
}

/// The fenced blocks of `lang` in `text`.
fn blocks(text: &str, lang: &str) -> Vec<String> {
    let open = format!("```{lang}\n");
    text.match_indices(&open)
        .map(|(at, _)| {
            let rest = &text[at + open.len()..];
            rest[..rest.find("```").unwrap()].to_owned()
        })
        .collect()
}

#[test]
fn the_whole_manifest_in_manifest_md_is_one_yobuild_reads() {
    use yobuild::manifest::{Gate, Manifest, PluginKind, Transport};
    let text = reference("manifest.md");
    let toml = &blocks(&text, "toml")[0];
    let dir = Path::new("/tmp/agent");
    let manifest = Manifest::parse(toml, &dir.join("agent.toml"), dir)
        .unwrap_or_else(|e| panic!("manifest.md's manifest: {e}"));
    // What it shows: three kinds of plugin, a stdio and an HTTP server,
    // the gate on, a priced model (it sets a budget), `[extend]`.
    let kinds: Vec<&str> = manifest
        .plugins
        .iter()
        .map(|p| match p.kind {
            PluginKind::Pi(_) => "pi",
            PluginKind::Dsh(_) => "dsh",
            PluginKind::Rutis { .. } => "rutis",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["pi", "dsh", "rutis"]);
    assert!(matches!(manifest.mcp[0].transport, Transport::Stdio { .. }));
    assert!(matches!(manifest.mcp[1].transport, Transport::Http { .. }));
    assert_eq!(manifest.mcp[0].name, "files");
    assert_eq!(manifest.builtins.gate, Gate::AllowList);
    assert!(manifest.limits.budget_usd.is_some());
    assert!(manifest.extend.is_some());
}

#[test]
fn manifest_md_names_every_provider_and_key() {
    let text = reference("manifest.md");
    for (provider, vars) in yobuild::manifest::PROVIDERS {
        assert!(
            text.contains(&format!("`{provider}`")),
            "manifest.md lacks {provider}"
        );
        // A variable it names is one yobuild reads (it need not name all).
        let _ = vars;
    }
    for var in [
        "ANTHROPIC_API_KEY", "OPENAI_API_KEY", "DEEPSEEK_API_KEY", "GEMINI_API_KEY",
        "GOOGLE_API_KEY", "DASHSCOPE_API_KEY",
    ] {
        assert!(
            yobuild::manifest::PROVIDERS
                .iter()
                .any(|(_, vars)| vars.contains(&var)),
            "manifest.md names {var}, which no provider reads"
        );
        assert!(text.contains(var));
    }
    // Every model yoagent has a preset for (priced, its real limits).
    for config in yoagent::provider::ModelConfig::presets() {
        let id = format!("`{}/{}`", config.provider, config.id);
        assert!(text.contains(&id), "manifest.md lacks {id}");
    }
    for local in yobuild::manifest::LOCAL_PROVIDERS {
        assert!(
            text.contains(&format!("`{local}`")),
            "manifest.md lacks {local}"
        );
    }
    // Every key a manifest takes (the tables' fields), named.
    for key in [
        "name", "model", "prompt", "base_url", "api_key_env", "max_turns", "max_tokens",
        "max_minutes", "budget_usd", "session_budget_usd", "pi", "dsh", "rutis", "runtime",
        "config", "allow", "policy", "npm", "bin", "url", "args", "env", "headers", "ask_owner",
        "report", "facts", "gate", "scratch", "tools", "host_services", "web", "phone", "terminal",
        "grants", "workspace", "trust", "wake", "exec", "kinds", "max_loads", "max_attempts",
        "min_age_days",
    ] {
        let in_toml = text.lines().any(|l| {
            let l = l.trim_start();
            l.starts_with(&format!("{key} ")) || l.starts_with(&format!("{key}="))
        });
        assert!(
            in_toml || text.contains(&format!("`{key}")),
            "manifest.md does not name `{key}`"
        );
    }
}

/// The packages the gallery's agents pin (pi, DSH, stdio servers), read as
/// `run` reads their manifests.
fn gallery_pins() -> Vec<String> {
    use yobuild::manifest::{Manifest, PluginKind, Source, Transport};
    let gallery = Path::new(env!("CARGO_MANIFEST_DIR")).join("gallery");
    let mut pins = Vec::new();
    for entry in std::fs::read_dir(gallery).unwrap() {
        let manifest = entry.unwrap().path().join("agent.toml");
        if !manifest.is_file() {
            continue;
        }
        let manifest = Manifest::load(&manifest).unwrap();
        for plugin in &manifest.plugins {
            if let PluginKind::Pi(Source::Package(p)) | PluginKind::Dsh(Source::Package(p)) =
                &plugin.kind
            {
                pins.push(p.spec());
            }
        }
        for server in &manifest.mcp {
            if let Transport::Stdio {
                source: Source::Package(p),
                ..
            } = &server.transport
            {
                pins.push(p.spec());
            }
        }
    }
    assert!(!pins.is_empty(), "no gallery agent pins a package");
    pins
}

#[test]
fn parts_md_ci_pins_are_the_examples_and_gallerys_pins() {
    let text = reference("parts.md");
    let examples: String = ["handoff.toml", "coding.toml"]
        .iter()
        .map(|f| {
            std::fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("examples")
                    .join(f),
            )
            .unwrap()
        })
        .collect();
    let gallery = gallery_pins();
    let mut ci = Vec::new();
    for row in text.lines().filter(|l| l.contains("**CI**")) {
        for (at, _) in row.match_indices('`').step_by(2) {
            let rest = &row[at + 1..];
            let pin = &rest[..rest.find('`').unwrap()];
            // A pin: `name@1.2.3` or `@scope/name@1.2.3` (an `@` after the
            // first character), one word.
            if pin.chars().skip(1).any(|c| c == '@') && !pin.contains(' ') {
                assert!(
                    examples.contains(pin) || gallery.iter().any(|g| g == pin),
                    "parts.md marks {pin} CI, but no example or gallery agent pins it"
                );
                ci.push(pin.to_owned());
            }
        }
    }
    assert!(ci.len() >= 3, "{ci:?}");
    // The gallery runs in CI: each of its pins is a CI row.
    for pin in &gallery {
        assert!(
            ci.contains(pin),
            "a gallery agent pins {pin}: parts.md marks it CI"
        );
    }
}

/// The skill is installed without the repository (`yobuild skill
/// install`, design §13): it names none of the repository's folders, and
/// each agent it points `yobuild new --from` at is built into yobuild.
#[test]
fn the_skill_needs_no_repository() {
    for file in [
        "SKILL.md", "reference/manifest.md", "reference/parts.md", "reference/scripted.md",
        "reference/failures.md",
    ] {
        let text = std::fs::read_to_string(Path::new(SKILL).join(file)).unwrap();
        for path in [
            "examples/", "adapters/", "frontend/", "gallery/", "scripts/try-", "target/debug",
            "checkout", "path/to/yobuild",
        ] {
            assert!(!text.contains(path), "{file} names {path:?}");
        }
        for (at, _) in text.match_indices("--from ") {
            let name: String = text[at + "--from ".len()..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect();
            if name.is_empty() || name == "agent" {
                continue;
            }
            assert!(
                yobuild::agents::BUILTIN.iter().any(|a| a.name == name),
                "{file}: --from {name}"
            );
        }
    }
}

/// GitHub's anchor for a heading: lowercase, spaces made `-`, everything
/// but letters, digits, `-` and `_` dropped.
fn anchor(heading: &str) -> String {
    heading
        .trim()
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect()
}

/// `text` without its fenced code blocks: a `#` comment there is no
/// heading, and a `](` there no link.
fn prose(text: &str) -> String {
    let mut fenced = false;
    text.lines()
        .filter(|l| {
            if l.trim_start().starts_with("```") {
                fenced = !fenced;
                return false;
            }
            !fenced
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every relative link in `doc` (a path from the repository's root) leads
/// to a file, and each `#fragment` to a heading there.
fn links_lead_somewhere(doc: &str) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let doc = root.join(doc);
    let text = prose(&std::fs::read_to_string(&doc).unwrap());
    let headings = |path: &Path| -> Vec<String> {
        prose(&std::fs::read_to_string(path).unwrap())
            .lines()
            .filter_map(|l| {
                l.trim_start_matches('#')
                    .strip_prefix(' ')
                    .filter(|_| l.starts_with('#'))
            })
            .map(anchor)
            .collect()
    };
    let name = doc.display();
    for (at, _) in text.match_indices("](") {
        let target = &text[at + 2..];
        let target = &target[..target.find(')').unwrap()];
        if target.starts_with("http") {
            continue;
        }
        let (file, fragment) = target.split_once('#').unwrap_or((target, ""));
        let path = if file.is_empty() {
            doc.clone()
        } else {
            doc.parent().unwrap().join(file)
        };
        assert!(path.exists(), "{name} links {target}, which is not there");
        if !fragment.is_empty() {
            assert!(
                headings(&path).iter().any(|h| h == fragment),
                "{name} links {target}: no such heading"
            );
        }
    }
}

/// `docs/guide.md`, the person's manual: every link leads to a file (and
/// a heading there), its manifest is one yobuild reads, and the versions
/// it names are the code's.
#[test]
fn the_guide_links_lead_somewhere_and_its_facts_are_the_codes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    links_lead_somewhere("docs/guide.md");
    let text = std::fs::read_to_string(root.join("docs/guide.md")).unwrap();
    let toml = &blocks(&text, "toml")[0];
    let dir = Path::new("/tmp/agent");
    yobuild::manifest::Manifest::parse(toml, &dir.join("agent.toml"), dir)
        .unwrap_or_else(|e| panic!("the guide's manifest: {e}"));
    let read = |p: &str| std::fs::read_to_string(root.join(p)).unwrap();
    assert!(text.contains("Node 22.19+ and npm 11.19+"));
    assert!(read("src/runtime.rs").contains("Node 22.19+ and npm 11.19+"));
    assert!(text.contains("@arcships/rutis-runtime@0.12.0"));
    assert!(
        read("frontend/plugins/package.json").contains("\"@arcships/rutis-runtime\": \"0.12.0\"")
    );
    let rust = read("Cargo.toml");
    assert!(rust.contains("rust-version = \"1.88\"") && text.contains("1.88+"));
}

/// The cost of a gallery agent's live run, as its README gives it: the
/// first dollar amount under `## A live run`.
fn live_cost(agent: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let readme =
        std::fs::read_to_string(root.join("gallery").join(agent).join("README.md")).unwrap();
    let run = &readme[readme
        .find("## A live run")
        .unwrap_or_else(|| panic!("{agent}'s README has no live run"))..];
    let at = run
        .find('$')
        .unwrap_or_else(|| panic!("{agent}'s live run has no cost"));
    run[at..]
        .chars()
        .take_while(|c| *c == '$' || *c == '.' || c.is_ascii_digit())
        .collect()
}

/// `docs/use-cases.md` and the README, the pitch: their links lead
/// somewhere, every agent they list or copy `--from` is built in, and each
/// gallery agent is listed at its live run's cost — `deepseek-flash`, under
/// two cents, in an agent that caps dollars per run and per session, as
/// the page says.
#[test]
fn the_pitch_names_agents_that_are_built_in_at_their_real_costs() {
    use yobuild::manifest::Manifest;
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let built_in: Vec<&str> = yobuild::agents::BUILTIN.iter().map(|a| a.name).collect();
    let mut gallery: Vec<String> = std::fs::read_dir(root.join("gallery"))
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| e.path().join("agent.toml").exists())
        .map(|e| e.file_name().into_string().unwrap())
        .collect();
    gallery.sort();
    assert!(!gallery.is_empty());
    for agent in &gallery {
        let cost = live_cost(agent);
        let dollars: f64 = cost[1..].parse().unwrap();
        assert!(
            dollars < 0.02,
            "{agent}'s live run cost {cost}: not under two cents"
        );
        let manifest = Manifest::load(root.join("gallery").join(agent).join("agent.toml")).unwrap();
        let model = &manifest.agent.model;
        assert_eq!(
            (model.provider(), model.id()),
            ("deepseek", "deepseek-flash"),
            "{agent}"
        );
        assert!(
            manifest.limits.budget_usd.is_some() && manifest.limits.session_budget_usd.is_some(),
            "{agent} does not cap dollars per run and per session"
        );
    }
    for doc in ["docs/use-cases.md", "README.md"] {
        links_lead_somewhere(doc);
        let text = prose(&std::fs::read_to_string(root.join(doc)).unwrap());
        let lines: Vec<&str> = text.lines().collect();
        // An entry is a table row (`| `x` |`) or a bullet (`- **`x`**`).
        let entry = |l: &str| -> Option<String> {
            let rest = l.strip_prefix("| `").or_else(|| l.strip_prefix("- **`"))?;
            Some(rest[..rest.find('`')?].to_owned())
        };
        let entries: Vec<(usize, String)> = lines
            .iter()
            .enumerate()
            .filter_map(|(i, l)| Some((i, entry(l)?)))
            .collect();
        for (_, name) in &entries {
            assert!(
                built_in.contains(&name.as_str()),
                "{doc} lists `{name}`, which is not built in"
            );
        }
        for agent in &gallery {
            let cost = live_cost(agent);
            let (i, _) = entries
                .iter()
                .find(|(_, name)| name == agent)
                .unwrap_or_else(|| panic!("{doc} does not list {agent}"));
            // On its line, or a bullet's next line (which is no entry).
            let next = lines
                .get(i + 1)
                .filter(|l| entry(l).is_none() && lines[*i].starts_with("- "));
            let quoted = |l: &str| {
                l.match_indices(cost.as_str())
                    .any(|(at, _)| !l[at + cost.len()..].starts_with(|c: char| c.is_ascii_digit()))
            };
            assert!(
                quoted(lines[*i]) || next.is_some_and(|l| quoted(l)),
                "{doc}: {agent} is not listed at its live run's {cost}"
            );
        }
        let words: Vec<&str> = text.split_whitespace().collect();
        for pair in words
            .windows(2)
            .filter(|w| w[0].trim_start_matches('`') == "--from")
        {
            let name: String = pair[1]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect();
            assert!(
                pair[1].starts_with('<') || built_in.contains(&name.as_str()),
                "{doc}: `--from {}` is not built in",
                pair[1]
            );
        }
    }
    let readme = std::fs::read_to_string(root.join("README.md")).unwrap();
    let readme = readme.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(readme.contains("Node 22.19+ and npm 11.19+") && readme.contains("Rust 1.88+"));
    let page = std::fs::read_to_string(root.join("docs/use-cases.md")).unwrap();
    assert!(page.contains("under two cents"));
}
