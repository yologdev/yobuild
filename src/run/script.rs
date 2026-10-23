//! The model's replies from a file, for runs without a provider (CI, demos):
//! a JSON list, each reply either `{ "text": "…" }` or `{ "tools": [{
//! "name": "…", "arguments": { … } }] }`. `{cwd}` in an argument's text is
//! the agent's folder, as in the prompt: for a tool that takes absolute
//! paths, a reply file runs in any folder. A call's `expect` — a string or
//! a list — is what its result must contain (design §11 "pi-lens in the
//! gallery"): carried in the call's `provider_metadata` (the mock's ids
//! repeat each turn) and checked by `run/expect.rs`.

use std::path::Path;

use serde::Deserialize;
use yoagent::provider::mock::{MockResponse, MockToolCall};
use yoagent::provider::MockProvider;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, untagged)]
enum Reply {
    Text { text: String },
    Tools { tools: Vec<Call> },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Call {
    name: String,
    #[serde(default)]
    arguments: serde_json::Value,
    /// Absent, or given: `null` is refused, as `""` and `[]` are.
    #[serde(default, deserialize_with = "given")]
    expect: Option<Expect>,
}

/// An `expect` given: a text or a list, not `null`.
fn given<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Expect>, D::Error> {
    Expect::deserialize(d).map(Some)
}

/// What a call's result must contain: one text, or several.
#[derive(Deserialize)]
#[serde(untagged)]
enum Expect {
    One(String),
    All(Vec<String>),
}

impl Expect {
    fn texts(self) -> Vec<String> {
        match self {
            Expect::One(text) => vec![text],
            Expect::All(texts) => texts,
        }
    }
}

/// Every `{cwd}` in `value`'s text replaced by `cwd`.
fn fill(value: serde_json::Value, cwd: &str) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::String(text) => Value::String(text.replace("{cwd}", cwd)),
        Value::Array(items) => Value::Array(items.into_iter().map(|v| fill(v, cwd)).collect()),
        Value::Object(map) => {
            Value::Object(map.into_iter().map(|(k, v)| (k, fill(v, cwd))).collect())
        }
        other => other,
    }
}

/// A provider that answers with the file's replies, in order, `{cwd}` in
/// their arguments filled with the agent's folder.
pub(crate) fn provider(path: &Path, cwd: &Path) -> Result<MockProvider, String> {
    let cwd = cwd.display().to_string();
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let replies: Vec<Reply> = serde_json::from_str(&text).map_err(|e| {
        format!(
            "{}: {e} (a list of {{ \"text\": … }} or {{ \"tools\": [{{ \"name\", \"arguments\", \"expect\"? }}] }})",
            path.display()
        )
    })?;
    for reply in &replies {
        if let Reply::Tools { tools } = reply {
            for call in tools {
                let empty = match &call.expect {
                    Some(Expect::One(text)) => text.is_empty(),
                    Some(Expect::All(texts)) => {
                        texts.is_empty() || texts.iter().any(String::is_empty)
                    }
                    None => false,
                };
                if empty {
                    return Err(format!(
                        "{}: `{}`'s `expect` is empty: give the text its result must contain",
                        path.display(),
                        call.name
                    ));
                }
            }
        }
    }
    Ok(MockProvider::new(
        replies
            .into_iter()
            .map(|reply| match reply {
                Reply::Text { text } => MockResponse::Text(text),
                Reply::Tools { tools } => MockResponse::ToolCalls(
                    tools
                        .into_iter()
                        .map(|c| MockToolCall {
                            name: c.name,
                            arguments: fill(c.arguments, &cwd),
                            provider_metadata: c
                                .expect
                                .map(|e| serde_json::json!({ super::expect::KEY: e.texts() })),
                        })
                        .collect(),
                ),
            })
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cwd_in_arguments_is_the_agents_folder() {
        let filled = fill(
            serde_json::json!({ "paths": ["{cwd}/a.ts", "b"], "n": 1, "o": { "p": "{cwd}" } }),
            "/w",
        );
        assert_eq!(
            filled,
            serde_json::json!({ "paths": ["/w/a.ts", "b"], "n": 1, "o": { "p": "/w" } })
        );
    }

    #[test]
    fn the_skills_examples_are_replies() {
        let text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/skill/yobuild/reference/scripted.md"
        ))
        .unwrap();
        let mut seen = 0;
        for (at, _) in text.match_indices("```json\n") {
            let rest = &text[at + "```json\n".len()..];
            let block = &rest[..rest.find("```").unwrap()];
            let parsed: Result<Vec<Reply>, _> = serde_json::from_str(block);
            let one: Result<Reply, _> = serde_json::from_str(block);
            assert!(parsed.is_ok() || one.is_ok(), "{block}");
            seen += 1;
        }
        assert!(seen >= 3, "{seen}");
    }

    #[test]
    fn replies_are_read_strictly() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.json");
        std::fs::write(
            &good,
            r#"[{ "tools": [{ "name": "echo", "arguments": { "text": "hi" } }] }, { "text": "done" }]"#,
        )
        .unwrap();
        assert!(provider(&good, Path::new("/w")).is_ok());
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, r#"[{ "txt": "typo" }]"#).unwrap();
        let Err(e) = provider(&bad, Path::new("/w")) else {
            panic!("a typo is refused")
        };
        assert!(e.contains("a list of"), "{e}");
        assert!(provider(&dir.path().join("none.json"), Path::new("/w")).is_err());
    }

    #[test]
    fn a_calls_expect_is_one_text_or_several_and_never_empty() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("r.json");
        let read = |text: &str| {
            std::fs::write(&file, text).unwrap();
            provider(&file, Path::new("/w")).map(|_| ())
        };
        for good in [
            r#"[{ "tools": [{ "name": "a", "expect": "x" }] }]"#,
            r#"[{ "tools": [{ "name": "a", "expect": ["x", "y"] }] }]"#,
        ] {
            assert!(read(good).is_ok(), "{good}");
        }
        for (bad, says) in [
            (
                r#"[{ "tools": [{ "name": "a", "expect": 3 }] }]"#,
                "a list of",
            ),
            (
                r#"[{ "tools": [{ "name": "a", "expect": "" }] }]"#,
                "`a`'s `expect` is empty",
            ),
            (
                r#"[{ "tools": [{ "name": "a", "expect": [] }] }]"#,
                "`a`'s `expect` is empty",
            ),
            (
                r#"[{ "tools": [{ "name": "a", "expect": ["x", ""] }] }]"#,
                "`a`'s `expect` is empty",
            ),
            (
                r#"[{ "tools": [{ "name": "a", "expect": null }] }]"#,
                "a list of",
            ),
            (
                r#"[{ "tools": [{ "name": "a", "expects": "x" }] }]"#,
                "a list of",
            ),
        ] {
            let e = read(bad).unwrap_err();
            assert!(e.contains(says), "{bad}: {e}");
        }
        // Carried in the call's metadata.
        let Reply::Tools { tools } = serde_json::from_str::<Reply>(
            r#"{ "tools": [{ "name": "a", "expect": ["x", "y"] }] }"#,
        )
        .unwrap() else {
            panic!("tools")
        };
        let texts = tools.into_iter().next().unwrap().expect.unwrap().texts();
        assert_eq!(texts, ["x", "y"]);
    }
}
