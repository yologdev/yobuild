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
    // the gate on, a priced model (it sets a budget).
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
    for preset in yobuild::manifest::PRESETS {
        let config = preset();
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
        "grants", "workspace", "trust", "wake",
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
