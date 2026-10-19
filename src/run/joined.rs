//! A tool result's text blocks, joined into one: yoagent 0.25.7's providers
//! send the model only the first text block of a result without images
//! (`provider/openai_compat.rs` and four others), so a result in several
//! blocks — ESLint's MCP server answers an intro line, the findings, then a
//! note — reached the model as its intro alone, read as "no findings".
//! Joined (in order, a newline between), every provider sends it whole.
//! Remove once yobuild depends on a yoagent that joins them itself.

use yoagent::extension::{Extension, ExtensionError, RunContext, RunHooks, ToolOutput};
use yoagent::{Content, ToolCallRequest};

/// The extension (`joined`).
pub(crate) struct JoinedText;

#[async_trait::async_trait]
impl Extension for JoinedText {
    fn name(&self) -> &str {
        "joined"
    }

    async fn start_run(&self, _run: &RunContext<'_>) -> Result<Box<dyn RunHooks>, ExtensionError> {
        Ok(Box::new(JoinedText))
    }
}

#[async_trait::async_trait]
impl RunHooks for JoinedText {
    async fn after_tool(
        &self,
        _call: &ToolCallRequest<'_>,
        output: &mut ToolOutput,
    ) -> Result<(), ExtensionError> {
        join(&mut output.result.content);
        Ok(())
    }
}

/// Several text blocks and no image (with an image, the providers send
/// every block): one text block first, any other blocks after it.
fn join(content: &mut Vec<Content>) {
    let texts = content
        .iter()
        .filter(|c| matches!(c, Content::Text { .. }))
        .count();
    if texts < 2 || content.iter().any(|c| matches!(c, Content::Image { .. })) {
        return;
    }
    let (texts, rest): (Vec<Content>, Vec<Content>) = std::mem::take(content)
        .into_iter()
        .partition(|c| matches!(c, Content::Text { .. }));
    let joined: Vec<String> = texts
        .into_iter()
        .filter_map(|c| match c {
            Content::Text { text } => Some(text),
            _ => None,
        })
        .collect();
    content.push(Content::Text {
        text: joined.join("\n"),
    });
    content.extend(rest);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(t: &str) -> Content {
        Content::Text { text: t.into() }
    }

    #[test]
    fn several_text_blocks_become_one_in_order() {
        let mut content = vec![
            text("Here are the results:"),
            text("{\"errorCount\":3}"),
            text("Ask first."),
        ];
        join(&mut content);
        assert_eq!(
            content,
            vec![text(
                "Here are the results:\n{\"errorCount\":3}\nAsk first."
            )]
        );
        let mut one = vec![text("only")];
        join(&mut one);
        assert_eq!(one, vec![text("only")]);
        let mut none: Vec<Content> = Vec::new();
        join(&mut none);
        assert!(none.is_empty());
    }
}
