//! The model's replies from a file, for runs without a provider (CI, demos):
//! a JSON list, each reply either `{ "text": "…" }` or `{ "tools": [{
//! "name": "…", "arguments": { … } }] }`.

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

/// A provider that answers with the file's replies, in order.
pub(crate) fn provider(path: &Path) -> Result<MockProvider, String> {
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
                            arguments: c.arguments,
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
    fn replies_are_read_strictly() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.json");
        std::fs::write(
            &good,
            r#"[{ "tools": [{ "name": "echo", "arguments": { "text": "hi" } }] }, { "text": "done" }]"#,
        )
        .unwrap();
        assert!(provider(&good).is_ok());
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, r#"[{ "txt": "typo" }]"#).unwrap();
        let Err(e) = provider(&bad) else {
            panic!("a typo is refused")
        };
        assert!(e.contains("a list of"), "{e}");
        assert!(provider(&dir.path().join("none.json")).is_err());
    }
}
