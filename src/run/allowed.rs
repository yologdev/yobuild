//! A plugin's `allow` names, checked when its tools are known: they come
//! with each run, so not at start (an MCP server's are checked when it lists
//! its tools). Also said: an allowed name offered only by another owner
//! (design §4, "Who a plugin is"), so a tool that keeps asking is
//! explained.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use yoagent::extension::{
    Extension, ExtensionError, RunContext, RunHooks, RunOutcome, TurnDecision,
};
use yoagent::TurnContext;

use crate::frontend::{NoticeLevel, Session};

/// For a run and a plugin's allowed tool offered in it: the owner that
/// offered it, when that is not one the plugin's entry accepts (`None`:
/// its own, or not known).
pub(crate) type OtherOwner = Arc<dyn Fn(&str, &str, &str) -> Option<String> + Send + Sync>;

/// Warns once, after the session's first run with a turn, of each plugin
/// `allow` name no tool of that name was offered for in any of its turns —
/// a typo, or a tool the plugin offers only in some runs (so a warning, not
/// a refusal) — and of each offered only by another owner (it asks).
/// Looking across the whole run, not at its first turn: a plugin still
/// starting then has its tools by a later turn.
#[derive(Clone)]
pub(crate) struct AllowedCheck {
    session: Session,
    /// (plugin, tool) for each name a plugin allows.
    names: Arc<Vec<(String, String)>>,
    other_owner: Option<OtherOwner>,
    looked: Arc<AtomicBool>,
}

impl AllowedCheck {
    pub(crate) fn new(session: Session, names: Vec<(String, String)>) -> Self {
        Self {
            session,
            names: Arc::new(names),
            other_owner: None,
            looked: Arc::default(),
        }
    }

    /// Also say an allowed name offered by an owner its plugin does not
    /// accept (`other_owner(run, plugin, tool)`).
    pub(crate) fn with_other_owner(mut self, other_owner: OtherOwner) -> Self {
        self.other_owner = Some(other_owner);
        self
    }
}

#[async_trait::async_trait]
impl Extension for AllowedCheck {
    fn name(&self) -> &str {
        "allowed"
    }

    async fn start_run(&self, run: &RunContext<'_>) -> Result<Box<dyn RunHooks>, ExtensionError> {
        Ok(Box::new(CheckRun {
            check: self.clone(),
            run: run.run_id.to_owned(),
            offered: HashSet::new(),
            others: Vec::new(),
            turns: 0,
        }))
    }
}

/// One run: the tool names offered in its turns, and for allowed ones
/// offered by an owner their plugin does not accept, that owner.
struct CheckRun {
    check: AllowedCheck,
    run: String,
    offered: HashSet<String>,
    /// (plugin, tool, owner).
    others: Vec<(String, String, String)>,
    turns: usize,
}

#[async_trait::async_trait]
impl RunHooks for CheckRun {
    async fn before_model(&mut self, turn: &TurnContext<'_>) -> TurnDecision {
        self.turns += 1;
        self.offered
            .extend(turn.tools.iter().map(|t| t.name.clone()));
        if let Some(other_owner) = &self.check.other_owner {
            for (plugin, tool) in self.check.names.iter() {
                let seen = self.others.iter().any(|(p, t, _)| p == plugin && t == tool);
                if seen || !turn.tools.iter().any(|t| &t.name == tool) {
                    continue;
                }
                if let Some(owner) = other_owner(&self.run, plugin, tool) {
                    self.others.push((plugin.clone(), tool.clone(), owner));
                }
            }
        }
        TurnDecision::Continue
    }

    async fn finish(&mut self, _outcome: &RunOutcome) {
        // A run with no turn (its input refused) offered nothing to judge:
        // the next one looks.
        if self.turns == 0 || self.check.looked.swap(true, Ordering::SeqCst) {
            return;
        }
        for (plugin, tool, owner) in &self.others {
            let by = match owner.starts_with("plugin:") || owner == "pi" || owner == "dsh" {
                true => format!("offered by `{owner}`, not by it"),
                false => {
                    "offered from no plugin row (does the plugin inject `yoagent`?)".to_owned()
                }
            };
            self.check.session.notice(
                NoticeLevel::Warning,
                format!("plugin {plugin} allows `{tool}`, but it was {by}: the tool asks"),
            );
        }
        for (plugin, tool) in self.check.names.iter() {
            if !self.offered.contains(tool) {
                self.check.session.notice(
                    NoticeLevel::Warning,
                    format!(
                        "plugin {plugin} offered no tool `{tool}`, which its `allow` names \
                         (a typo, or a tool it offers only in some runs)"
                    ),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::{ClientMessage, ServerMessage};
    use yoagent::provider::mock::MockResponse;
    use yoagent::provider::{MockProvider, ModelConfig};
    use yoagent::tools::ListFilesTool;
    use yoagent::Agent;

    /// The warnings the session gave over two runs, each with a turn.
    async fn warnings_over_two_runs(names: Vec<(String, String)>) -> Vec<String> {
        let (session, driver) = Session::new(true);
        let mut watching = session.observe();
        let agent = Agent::from_provider(
            MockProvider::new(vec![
                MockResponse::Text("one".into()),
                MockResponse::Text("two".into()),
            ]),
            ModelConfig::mock(),
        )
        .with_tools(vec![Box::new(ListFilesTool::default())])
        .with_extension(AllowedCheck::new(session.clone(), names));
        let running = tokio::spawn(driver.run(agent));
        let mut warnings = Vec::new();
        for text in ["a", "b"] {
            session.send(ClientMessage::Prompt { text: text.into() });
            loop {
                match watching.messages.recv().await.expect("the session") {
                    ServerMessage::RunEnd { .. } => break,
                    ServerMessage::Notice { message, .. } => warnings.push(message),
                    _ => {}
                }
            }
        }
        session.send(ClientMessage::Quit);
        running.await.unwrap();
        warnings
    }

    #[tokio::test]
    async fn a_name_no_tool_was_offered_for_is_said_once_a_session() {
        let names = vec![
            ("p".to_owned(), "list_files".to_owned()),
            ("p".to_owned(), "gone".to_owned()),
        ];
        assert_eq!(
            warnings_over_two_runs(names).await,
            ["plugin p offered no tool `gone`, which its `allow` names (a typo, or a tool it offers only in some runs)"]
        );
    }
}
