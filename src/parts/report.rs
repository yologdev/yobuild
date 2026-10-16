//! `report`: every run ends with the required sections.

use std::sync::Arc;

use yoagent::extension::{
    Extension, ExtensionError, ExtensionMode, RunContext, RunHooks, StopContext, StopDecision,
};
use yoagent::{Content, Message};

/// An extension that sends the agent back (`StopDecision::Continue`) until
/// its last message names every required section. A section counts when its
/// name appears anywhere in the message, ignoring case; the check is meant
/// to get a report written, not to grade it.
///
/// It is a required extension: when the agent still has not written the
/// report after yoagent's limit of send-backs (`max_stop_continues`, 3 by
/// default), the run fails — its end carries yoagent's error, "[Extension
/// failed: report] the answer was still not accepted after 3 continues" —
/// rather than ending quietly without one.
///
/// Put [`Report::instruction`] in the system prompt so the agent knows the
/// sections before it is sent back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    sections: Arc<[String]>,
}

impl Report {
    /// At least one section; names are trimmed, and a blank or repeated one
    /// is an error.
    pub fn new<S: Into<String>>(sections: impl IntoIterator<Item = S>) -> Result<Self, String> {
        let mut names: Vec<String> = Vec::new();
        for section in sections {
            let section = section.into().trim().to_owned();
            if section.is_empty() {
                return Err("a report section needs a name".into());
            }
            if names.iter().any(|s| s.eq_ignore_ascii_case(&section)) {
                return Err(format!("the report section {section:?} is given twice"));
            }
            names.push(section);
        }
        if names.is_empty() {
            return Err("a report needs at least one section".into());
        }
        Ok(Self {
            sections: names.into(),
        })
    }

    /// The handoff report: Done, Waiting on you, Decisions, Next.
    pub fn handoff() -> Self {
        Self::new(["Done", "Waiting on you", "Decisions", "Next"]).expect("valid sections")
    }

    pub fn sections(&self) -> &[String] {
        &self.sections
    }

    /// A line for the system prompt.
    pub fn instruction(&self) -> String {
        format!(
            "End every run with a report in Markdown, with these sections: {}.",
            headings(&self.sections)
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
}

fn headings(sections: &[String]) -> String {
    sections
        .iter()
        .map(|s| format!("## {s}"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[async_trait::async_trait]
impl Extension for Report {
    fn name(&self) -> &str {
        "report"
    }
    fn mode(&self) -> ExtensionMode {
        ExtensionMode::Required
    }
    async fn start_run(&self, _run: &RunContext<'_>) -> Result<Box<dyn RunHooks>, ExtensionError> {
        Ok(Box::new(ReportRun(self.clone())))
    }
}

struct ReportRun(Report);

#[async_trait::async_trait]
impl RunHooks for ReportRun {
    async fn on_stop(&mut self, stop: &StopContext<'_>) -> StopDecision {
        // yoagent asks only about a finished assistant message.
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
        let missing = self.0.missing(&text);
        if missing.is_empty() {
            StopDecision::Accept
        } else {
            StopDecision::Continue(format!(
                "End with the report: sections {}. Missing: {}.",
                headings(&self.0.sections),
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
        // Lenient on purpose: a name anywhere counts.
        assert!(Report::new(["Done"])
            .unwrap()
            .missing("all done")
            .is_empty());
    }

    #[test]
    fn the_instruction_lists_the_headings() {
        assert_eq!(
            Report::new(["A", "B"]).unwrap().instruction(),
            "End every run with a report in Markdown, with these sections: ## A, ## B."
        );
    }

    #[test]
    fn sections_must_be_named_once() {
        assert!(Report::new(Vec::<String>::new()).is_err());
        assert!(Report::new([" "]).is_err());
        assert!(Report::new(["Done", "done "]).is_err());
        assert_eq!(Report::new([" Done "]).unwrap().sections(), ["Done"]);
    }
}
