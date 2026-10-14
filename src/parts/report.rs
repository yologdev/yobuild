//! `report`: every run ends with the required sections.

use yoagent::extension::{
    Extension, ExtensionError, RunContext, RunHooks, StopContext, StopDecision,
};
use yoagent::{Content, Message};

/// An extension that sends the agent back (`StopDecision::Continue`) until
/// its last message names every required section. A section counts when its
/// name appears anywhere in the message, ignoring case; the check is meant
/// to get a report written, not to grade it.
///
/// Put [`Report::instruction`] in the system prompt so the agent knows the
/// sections before it is sent back.
#[derive(Debug, Clone)]
pub struct Report {
    sections: Vec<String>,
}

impl Report {
    pub fn new<S: Into<String>>(sections: impl IntoIterator<Item = S>) -> Self {
        Self {
            sections: sections.into_iter().map(Into::into).collect(),
        }
    }

    /// The handoff report: Done, Waiting on you, Decisions, Next.
    pub fn handoff() -> Self {
        Self::new(["Done", "Waiting on you", "Decisions", "Next"])
    }

    pub fn sections(&self) -> &[String] {
        &self.sections
    }

    /// A line for the system prompt.
    pub fn instruction(&self) -> String {
        format!(
            "End every run with a report in Markdown, with these sections: {}.",
            self.headings()
        )
    }

    /// The sections `text` does not name.
    pub fn missing(&self, text: &str) -> Vec<&str> {
        let text = text.to_lowercase();
        self.sections
            .iter()
            .filter(|s| !text.contains(&s.to_lowercase()))
            .map(String::as_str)
            .collect()
    }

    fn headings(&self) -> String {
        self.sections
            .iter()
            .map(|s| format!("## {s}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[async_trait::async_trait]
impl Extension for Report {
    fn name(&self) -> &str {
        "report"
    }
    async fn start_run(&self, _run: &RunContext<'_>) -> Result<Box<dyn RunHooks>, ExtensionError> {
        Ok(Box::new(self.clone()))
    }
}

#[async_trait::async_trait]
impl RunHooks for Report {
    async fn on_stop(&mut self, stop: &StopContext<'_>) -> StopDecision {
        let Message::Assistant { content, .. } = stop.answer else {
            return StopDecision::Accept;
        };
        let text: String = content
            .iter()
            .filter_map(|c| match c {
                Content::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let missing = self.missing(&text);
        if missing.is_empty() {
            StopDecision::Accept
        } else {
            StopDecision::Continue(format!(
                "End with the report: sections {}. Missing: {}.",
                self.headings(),
                missing.join(", ")
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_names_the_sections_absent_ignoring_case() {
        let report = Report::handoff();
        assert_eq!(
            report.missing("## done\n- x\n## Next\n- y"),
            ["Waiting on you", "Decisions"]
        );
        assert!(report
            .missing("## Done\n## Waiting on you\n## Decisions\n## Next")
            .is_empty());
    }

    #[test]
    fn the_instruction_lists_the_headings() {
        assert_eq!(
            Report::new(["A", "B"]).instruction(),
            "End every run with a report in Markdown, with these sections: ## A, ## B."
        );
    }
}
