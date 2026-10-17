//! A plugin's `allow` names, checked when its tools are known: they come
//! with each run, so not at start (an MCP server's are checked when it lists
//! its tools).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use yoagent::extension::{Extension, ExtensionError, RunContext, RunHooks, TurnDecision};
use yoagent::TurnContext;

use crate::frontend::{NoticeLevel, Session};

/// Warns once, at the session's first turn, of each plugin `allow` name no
/// tool of that name was offered for — a typo, or a tool the plugin offers
/// only in some runs (so a warning, not a refusal).
#[derive(Clone)]
pub(crate) struct AllowedCheck {
    session: Session,
    /// (plugin, tool) for each name a plugin allows.
    names: Arc<Vec<(String, String)>>,
    looked: Arc<AtomicBool>,
}

impl AllowedCheck {
    pub(crate) fn new(session: Session, names: Vec<(String, String)>) -> Self {
        Self {
            session,
            names: Arc::new(names),
            looked: Arc::default(),
        }
    }
}

#[async_trait::async_trait]
impl Extension for AllowedCheck {
    fn name(&self) -> &str {
        "allowed"
    }

    async fn start_run(&self, _run: &RunContext<'_>) -> Result<Box<dyn RunHooks>, ExtensionError> {
        Ok(Box::new(self.clone()))
    }
}

#[async_trait::async_trait]
impl RunHooks for AllowedCheck {
    async fn before_model(&mut self, turn: &TurnContext<'_>) -> TurnDecision {
        if !self.looked.swap(true, Ordering::SeqCst) {
            for (plugin, tool) in self.names.iter() {
                if !turn.tools.iter().any(|t| &t.name == tool) {
                    self.session.notice(
                        NoticeLevel::Warning,
                        format!(
                            "plugin {plugin} offered no tool `{tool}`, which its `allow` names \
                             (a typo, or a tool it offers only in some runs)"
                        ),
                    );
                }
            }
        }
        TurnDecision::Continue
    }
}
