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
