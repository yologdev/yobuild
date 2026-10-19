//! The model's replies from a file, for runs without a provider (CI, demos):
//! a JSON list, each reply either `{ "text": "…" }` or `{ "tools": [{
//! "name": "…", "arguments": { … } }] }`. `{cwd}` in an argument's text is
//! the agent's folder, as in the prompt: for a tool that takes absolute
//! paths, a reply file runs in any folder.

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
            "{}: {e} (a list of {{ \"text\": … }} or {{ \"tools\": [{{ \"name\", \"arguments\" }}] }})",
            path.display()
        )
    })?;
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
                            provider_metadata: None,
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
}
