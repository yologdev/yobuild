//! The session's spend, carried across a resume (design §15): yoagent's
//! `Budget` keeps its total private and cannot start from a sum, so
//! [`SessionSpend`] holds one across runs with no limit — counting as it
//! always does — adds what was spent before the resume, and stops a turn
//! when the sum reaches the session budget, in yoagent's words.

use std::sync::Arc;

use yoagent::extension::{
    Budget, Extension, ExtensionError, RunContext, RunHooks, RunOutcome, TurnDecision,
};
use yoagent::provider::CostConfig;
use yoagent::{AgentEvent, TurnContext};

/// The session budget (`session_budget_usd`, or none: then it only counts),
/// starting from `before`, what a saved session had spent.
#[derive(Clone)]
pub(crate) struct SessionSpend {
    counted: Arc<Budget>,
    before: f64,
    max: Option<f64>,
    name: String,
}

impl SessionSpend {
    pub(crate) fn new(cost: CostConfig, max: Option<f64>, before: f64, name: &str) -> Self {
        Self {
            counted: Arc::new(
                Budget::usd(f64::INFINITY, cost)
                    .across_runs()
                    .with_name(name),
            ),
            before: before.max(0.0),
            max,
            name: name.to_owned(),
        }
    }

    /// What the session has spent: before the resume and since.
    pub(crate) fn spent(&self) -> f64 {
        self.before + self.counted.spent_usd().unwrap_or_default()
    }
}

#[async_trait::async_trait]
impl Extension for SessionSpend {
    fn name(&self) -> &str {
        &self.name
    }

    async fn start_run(&self, run: &RunContext<'_>) -> Result<Box<dyn RunHooks>, ExtensionError> {
        Ok(Box::new(SpendRun {
            counting: self.counted.start_run(run).await?,
            spend: self.clone(),
        }))
    }
}

struct SpendRun {
    counting: Box<dyn RunHooks>,
    spend: SessionSpend,
}

#[async_trait::async_trait]
impl RunHooks for SpendRun {
    async fn before_model(&mut self, turn: &TurnContext<'_>) -> TurnDecision {
        // Counts what the run spent outside its turns; never stops (no limit).
        let decision = self.counting.before_model(turn).await;
        let Some(max) = self.spend.max else {
            return decision;
        };
        let spent = self.spend.spent();
        match spent >= max {
            // yoagent's words, so a run it stops reads as a budget's stop.
            true => TurnDecision::Stop(format!(
                "{} of ${max:.2} spent (${spent:.4})",
                self.spend.name
            )),
            false => decision,
        }
    }

    fn on_event(&self, event: &AgentEvent) {
        self.counting.on_event(event);
    }

    async fn finish(&mut self, outcome: &RunOutcome) {
        self.counting.finish(outcome).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;
    use yoagent::extension::RunEnd;
    use yoagent::{AgentMessage, Message, StopReason, Usage};

    fn cost() -> CostConfig {
        CostConfig::new(1_000_000.0, 0.0)
    }

    /// A turn that read `tokens` input tokens, at $1 a token.
    fn spent(tokens: u64) -> AgentEvent {
        AgentEvent::MessageEnd {
            message: AgentMessage::Llm(Message::assistant(
                Vec::new(),
                StopReason::Stop,
                "m",
                "p",
                Usage {
                    input: tokens,
                    ..Usage::default()
                },
            )),
        }
    }

    async fn turn(run: &mut Box<dyn RunHooks>) -> TurnDecision {
        run.before_model(&TurnContext::new("", &[], &[], "m")).await
    }

    #[tokio::test]
    async fn it_stops_at_the_sum_of_what_was_saved_and_what_it_counts() {
        let spend = SessionSpend::new(cost(), Some(5.0), 3.0, "session budget");
        let cancel = CancellationToken::new();
        let mut run = spend
            .start_run(&RunContext::new("r1", &[], &cancel))
            .await
            .unwrap();
        assert!(matches!(turn(&mut run).await, TurnDecision::Continue));
        run.on_event(&spent(1));
        assert_eq!(spend.spent(), 4.0);
        assert!(matches!(turn(&mut run).await, TurnDecision::Continue));
        run.on_event(&spent(1));
        let TurnDecision::Stop(why) = turn(&mut run).await else {
            panic!("stopped at $5")
        };
        assert_eq!(why, "session budget of $5.00 spent ($5.0000)");
        run.finish(&RunOutcome::new(RunEnd::Completed, None)).await;
        // Across runs, as yoagent's session budget.
        let mut next = spend
            .start_run(&RunContext::new("r2", &[], &cancel))
            .await
            .unwrap();
        assert!(matches!(turn(&mut next).await, TurnDecision::Stop(_)));
    }

    #[tokio::test]
    async fn with_no_limit_it_only_counts() {
        let spend = SessionSpend::new(cost(), None, 2.5, "session budget");
        let cancel = CancellationToken::new();
        let mut run = spend
            .start_run(&RunContext::new("r", &[], &cancel))
            .await
            .unwrap();
        run.on_event(&spent(100));
        assert!(matches!(turn(&mut run).await, TurnDecision::Continue));
        assert_eq!(spend.spent(), 102.5);
        // A saved spend below zero (a file edited by hand) counts as none.
        assert_eq!(SessionSpend::new(cost(), None, -1.0, "s").spent(), 0.0);
    }
}
