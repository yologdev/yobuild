//! What to do about a failure (design §11, "Making failures actionable"):
//! one table, a pattern of the message → the fix, each hint scoped to
//! where its message is seen, so a fix is never given for another kind of
//! failure. Patterns are the messages' own fixed parts (yoagent's
//! `[Agent stopped: …]` and `[Extension failed: …]` markers included),
//! matched as written. Each hint has an id that the skill's
//! `reference/failures.md` names in a hint comment; a test keeps the two
//! in step both ways, and each hint is tested against the message as the
//! code makes it.

/// Where a message is seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    /// How a run ended `Stopped` (its error).
    Stop,
    /// How a run ended `Error` or `Rejected` (its error).
    Fail,
    /// A tool call refused (its result), which does not end the run.
    Call,
    /// A plugin refused at start: `check`'s problems, `run`'s refusals and
    /// its "A plugin stopped" Notices.
    Start,
}

/// One failure's fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Hint {
    /// Named in `skill/yobuild/reference/failures.md`.
    pub id: &'static str,
    pub scope: Scope,
    /// The fixed text the message holds (matched as written).
    pub pattern: &'static str,
    pub fix: &'static str,
}

/// The fixes, most specific first within a scope (the first match wins).
pub(crate) const HINTS: &[Hint] = &[
    // How runs end: stopped.
    Hint {
        id: "max-turns",
        scope: Scope::Stop,
        pattern: "[Agent stopped: Max turns reached",
        fix: "send \"continue\" (a new run, with fresh limits; not under --once, which ends after one run), or raise [limits] max_turns",
    },
    Hint {
        id: "max-tokens",
        scope: Scope::Stop,
        pattern: "[Agent stopped: Max tokens reached",
        fix: "send \"continue\" (a new run, with fresh limits; not under --once), or raise [limits] max_tokens",
    },
    Hint {
        id: "max-duration",
        scope: Scope::Stop,
        pattern: "[Agent stopped: Max duration reached",
        fix: "send \"continue\" (a new run, with fresh limits; not under --once), or raise [limits] max_minutes",
    },
    Hint {
        id: "session-budget",
        scope: Scope::Stop,
        pattern: "[Agent stopped: session budget of",
        fix: "\"continue\" stops again at once (the session's total stays spent): restart, which starts a new conversation, or raise [limits] session_budget_usd",
    },
    Hint {
        id: "budget",
        scope: Scope::Stop,
        pattern: "[Agent stopped: budget of",
        fix: "send \"continue\" (each run has its own budget; not under --once), or raise [limits] budget_usd",
    },
    Hint {
        id: "loop",
        scope: Scope::Stop,
        pattern: "[Agent stopped: repeated tool call",
        fix: "change the prompt, or what the tool returns, so the agent does not call it the same way again; then send \"continue\"",
    },
    // How runs end: failed or rejected.
    Hint {
        id: "report",
        scope: Scope::Fail,
        pattern: "[Extension failed: report] the answer was still not accepted after",
        fix: "the last answer must name every [builtins] report section (anywhere, any case): a model that follows the prompt better, fewer and plainer sections, or remove report; a scripted run's last reply must name them",
    },
    Hint {
        id: "task-failed",
        scope: Scope::Fail,
        pattern: "the run ended unexpectedly: the agent task failed",
        fix: "the agent loop failed: the log says why; restart",
    },
    Hint {
        id: "host-down",
        scope: Scope::Fail,
        pattern: "input rejected: the plugin host is not running",
        fix: "the plugin host shut down: restart",
    },
    Hint {
        id: "auth",
        scope: Scope::Fail,
        pattern: "Auth error: ",
        fix: "the model's provider refused the key: check its variable (yobuild names it when missing) and that the key is valid for this model",
    },
    Hint {
        id: "rate-limited",
        scope: Scope::Fail,
        pattern: "Rate limited",
        fix: "the model's provider is rate-limiting (or out of capacity): wait, then send again",
    },
    // A tool call refused (the run goes on).
    Hint {
        id: "no-policy",
        scope: Scope::Call,
        pattern: "no plugin policy is loaded to judge this call",
        fix: "the policy plugin went away mid-session, so every call is denied: restart; the Error notice \"A plugin stopped: …\" and the log say why",
    },
    Hint {
        id: "handler-gone",
        scope: Scope::Call,
        pattern: "its plugin unloaded",
        fix: "a plugin's runtime died or it was unloaded: restart; the log says why",
    },
    Hint {
        id: "policy-timeout",
        scope: Scope::Call,
        pattern: "did not answer `before_tool` within",
        fix: "the policy plugin took longer than its wait (330 s): answer its question sooner; the plugin may hang",
    },
    Hint {
        id: "handler-failed",
        scope: Scope::Call,
        pattern: "a required plugin handler failed this run",
        fix: "a plugin's hook failed, so its calls are denied this run: the log says which; run yobuild check",
    },
    Hint {
        id: "host-down-call",
        scope: Scope::Call,
        pattern: "the plugin host is not running",
        fix: "the plugin host shut down: restart",
    },
    // Refused at start (check, run, and plugins stopping later).
    Hint {
        id: "shadowed-builtin",
        scope: Scope::Start,
        pattern: "is named like yoagent's built-in",
        fix: "this pi extension's tool cannot run beside yoagent's own of that name: choose another plugin",
    },
    Hint {
        id: "dsh-services",
        scope: Scope::Start,
        pattern: "DSH plugins wait for services nobody provides",
        fix: "set [dsh] host_services = true for DSH's own services, or add the DSH plugin the hint names as a [[plugin]]",
    },
    Hint {
        id: "dsh-host",
        scope: Scope::Start,
        pattern: "the DSH host did not start: ",
        fix: "the DSH host (loaded for DSH plugins' grants) waits for what it names: reinstall the DSH runtime's folder the error names (a `dsh` set's: delete it; adapters/dsh: `npm ci` there) and run again; else drop the DSH plugins' grants",
    },
    Hint {
        id: "nothing-provides",
        scope: Scope::Start,
        pattern: "nothing provides",
        fix: "the plugin injects a service no plugin provides: add the plugin that provides it, or remove the inject",
    },
    Hint {
        id: "no-judge",
        scope: Scope::Start,
        pattern: "from the policy plugin's row",
        fix: "the policy plugin never registered a handler judging calls: it must inject `yoagent` and register one with `before_tool`; or take `policy = true` off",
    },
    Hint {
        id: "not-together",
        scope: Scope::Start,
        pattern: "registered its handlers alone, but none with the other plugins",
        fix: "it conflicts with another plugin: check them in pairs to find which",
    },
    Hint {
        id: "stops-at-first",
        scope: Scope::Start,
        pattern: "a run would stop at its first request",
        fix: "a plugin stops every run before the model is asked: its reason says why",
    },
    Hint {
        id: "load-failed",
        scope: Scope::Start,
        pattern: "plugins failed to load",
        fix: "the plugin threw while it loaded: its error says why",
    },
    Hint {
        id: "row-failed",
        scope: Scope::Start,
        pattern: " (failed): plugin failed: ",
        fix: "the plugin threw while it loaded: its error says why",
    },
    Hint {
        id: "terminal-ui",
        scope: Scope::Start,
        pattern: "the terminal UI stopped",
        fix: "the terminal UI's runtime died: the log (named on the next line) says why",
    },
    Hint {
        id: "plugin-failed",
        scope: Scope::Start,
        pattern: "stopped: plugin `",
        fix: "the plugin failed, stopped or could not load: its error says why; the log has more",
    },
    Hint {
        id: "runtime-down",
        scope: Scope::Start,
        pattern: "runtime is not running (its plugins wait",
        fix: "the plugin runtime died: the log says why; restart",
    },
    Hint {
        id: "mcp-no-tools",
        scope: Scope::Start,
        pattern: "the server lists no tools",
        fix: "the MCP server offers nothing: its arguments or its version",
    },
    Hint {
        id: "mcp-allow",
        scope: Scope::Start,
        pattern: "which it does not offer (it offers:",
        fix: "use the server's own tool names in its allow (it lists them)",
    },
];

/// The hint of `scope` for `message`, if one fits.
pub(crate) fn hint(scope: Scope, message: &str) -> Option<&'static Hint> {
    HINTS
        .iter()
        .find(|h| h.scope == scope && message.contains(h.pattern))
}

/// The fix for a stop no hint names: a plugin or an extension stopped the
/// run (yoagent's own stops have hints).
const OTHER_STOP: &str =
    "a plugin stopped the run: its reason says why; send \"continue\" only once that cause is gone";

/// What to do about a run's end: `stopped` (else failed or rejected) with
/// `error`.
pub(crate) fn what_to_do(stopped: bool, error: &str) -> Option<&'static str> {
    match stopped {
        true => Some(hint(Scope::Stop, error).map_or(OTHER_STOP, |h| h.fix)),
        false => hint(Scope::Fail, error).map(|h| h.fix),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yoagent::agent_loop::{AGENT_STOPPED_PREFIX, LOOP_ABORT_PREFIX};
    use yoagent::extension::EXTENSION_FAILED_PREFIX;
    use yoagent::provider::ProviderError;

    /// Each hint's message as the code makes it: from the constant or the
    /// type that makes it where it is exported, else its format copied from
    /// the place named (whose text a test elsewhere checks too).
    fn real() -> Vec<(Scope, String, &'static str)> {
        let stopped = |reason: &str| format!("{AGENT_STOPPED_PREFIX} {reason}]");
        vec![
            // yoagent 0.25.7 context.rs: `Max … reached ({}/{})`.
            (Scope::Stop, stopped("Max turns reached (50/50)"), "max-turns"),
            (Scope::Stop, stopped("Max tokens reached (1000000/1000000)"), "max-tokens"),
            (Scope::Stop, stopped("Max duration reached (600s/600s)"), "max-duration"),
            // yoagent extension.rs: `{name} of ${:.2} spent (${:.4})`.
            (
                Scope::Stop,
                stopped(&format!("{} of $2.00 spent ($2.0100)", crate::run::SESSION_BUDGET)),
                "session-budget",
            ),
            (Scope::Stop, stopped("budget of $1.00 spent ($1.0012)"), "budget"),
            (
                Scope::Stop,
                format!("{LOOP_ABORT_PREFIX} bash was called repeatedly with identical arguments after being asked to change approach.]"),
                "loop",
            ),
            (
                Scope::Fail,
                format!("{EXTENSION_FAILED_PREFIX} report] the answer was still not accepted after 3 continues"),
                "report",
            ),
            (Scope::Fail, crate::frontend::session::TASK_FAILED.to_owned(), "task-failed"),
            // yoagent-rutis 0.7.0 host.rs `NOT_RUNNING`, extension.rs
            // `input rejected: {NOT_RUNNING}`.
            (
                Scope::Fail,
                "input rejected: the plugin host is not running (shut down, disposed or restarted since the bridge was installed)".into(),
                "host-down",
            ),
            (
                Scope::Fail,
                ProviderError::Auth("HTTP 401 Unauthorized: invalid api key".into()).to_string(),
                "auth",
            ),
            (
                Scope::Fail,
                ProviderError::RateLimited { retry_after_ms: Some(1500) }.to_string(),
                "rate-limited",
            ),
            // yoagent-rutis 0.7.0 extension.rs: `UNJUDGED`, the handler
            // gone (348), the wait (376), a failed handler (632).
            (Scope::Call, "no plugin policy is loaded to judge this call".into(), "no-policy"),
            (
                Scope::Call,
                "plugin handler `guard` is no longer available: its plugin unloaded".into(),
                "handler-gone",
            ),
            (
                Scope::Call,
                "plugin handler `p` did not answer `before_tool` within 330s".into(),
                "policy-timeout",
            ),
            (
                Scope::Call,
                "a required plugin handler failed this run: plugin handler `p` failed in `before_model`: boom".into(),
                "handler-failed",
            ),
            (
                Scope::Call,
                "the plugin host is not running (shut down, disposed or restarted since the bridge was installed)".into(),
                "host-down-call",
            ),
            // yobuild: run/check.rs, run.rs, frontend/host.rs.
            (
                Scope::Start,
                "its tool `read_file` is named like yoagent's built-in: `run` refuses it".into(),
                "shadowed-builtin",
            ),
            (
                Scope::Start,
                "`run` would refuse it at start: DSH plugins wait for services nobody provides: fs".into(),
                "dsh-services",
            ),
            (
                Scope::Start,
                "`run` would refuse it at start: the DSH host did not start: it waits for sessionPersistence (the DSH runtime's folder: /h/plugins/dsh/0123)".into(),
                "dsh-host",
            ),
            (
                Scope::Start,
                "`run` would refuse it at start: plugin rows not running: plugin:needy (waiting): nothing provides nosuchService".into(),
                "nothing-provides",
            ),
            (
                Scope::Start,
                "a call judge (before_tool) from the policy plugin's row plugin:lib (does it inject `yoagent`?)".into(),
                "no-judge",
            ),
            (
                Scope::Start,
                "plugin:lib registered its handlers alone, but none with the other plugins".into(),
                "not-together",
            ),
            (Scope::Start, "a run would stop at its first request: no".into(), "stops-at-first"),
            (
                Scope::Start,
                "plugins failed to load: [Failure { id: \"plugin:late\", error: \"plugin failed: Error: boom\" }]".into(),
                "load-failed",
            ),
            // yoagent-rutis 0.7.0 rows.rs `RowsNotRunning`: a row that
            // failed while `load` waited (`{id} ({state}): {error}`).
            (
                Scope::Start,
                "plugin rows not running: plugin:late (failed): plugin failed: Error: boom".into(),
                "row-failed",
            ),
            (
                Scope::Start,
                "it stopped: plugin `plugin:late` failed: Some(\"boom\")".into(),
                "plugin-failed",
            ),
            (
                Scope::Start,
                "the `dsh` runtime is not running (its plugins wait: plugin:x)".into(),
                "runtime-down",
            ),
            (Scope::Start, "mcp files: the server lists no tools".into(), "mcp-no-tools"),
            (
                Scope::Start,
                "mcp files: its `allow` names \"x\", which it does not offer (it offers: y)".into(),
                "mcp-allow",
            ),
            (
                Scope::Start,
                "the terminal UI stopped: plugin `terminal-ui` failed: … (the last run ended Completed)".into(),
                "terminal-ui",
            ),
        ]
    }

    #[test]
    fn each_real_message_gets_its_hint_and_each_hint_a_message() {
        let real = real();
        for (scope, message, id) in &real {
            assert_eq!(hint(*scope, message).map(|h| h.id), Some(*id), "{message}");
        }
        for h in HINTS {
            assert!(
                real.iter().any(|(_, _, id)| *id == h.id),
                "no real message for {}",
                h.id
            );
        }
    }

    #[test]
    fn a_fix_is_given_only_where_its_message_is_seen() {
        // Numbers that hold 401, a port, a stack position: no auth hint.
        for text in [
            "API error: HTTP 400: you requested 140100 tokens",
            "a plugin stopped: plugin `plugin:x` failed: at p.ts:401:3",
            "Network error: 127.0.0.1:40123 connection reset",
            "",
        ] {
            assert_ne!(
                hint(Scope::Fail, text).map(|h| h.id),
                Some("auth"),
                "{text}"
            );
        }
        // A provider's error text inside a check problem: not the model's key.
        assert!(hint(Scope::Start, "mcp api: Auth error: HTTP 401").is_none());
        // The session budget before the run's.
        assert_eq!(
            hint(
                Scope::Stop,
                "[Agent stopped: session budget of $2.00 spent ($2.01)]"
            )
            .map(|h| h.id),
            Some("session-budget")
        );
        // A plugin's stop reason that only mentions a budget: no budget hint.
        assert!(hint(
            Scope::Stop,
            "[Agent stopped: the budget of this plugin is low]"
        )
        .is_none());
        // The wrapped timeout: the timeout's fix, not the generic failure.
        assert_eq!(
            hint(Scope::Call, "a required plugin handler failed this run: plugin handler `p` did not answer `before_tool` within 330s").map(|h| h.id),
            Some("policy-timeout")
        );
        // A plugin failing while waiting for a service: the service's fix.
        assert_eq!(
            hint(
                Scope::Start,
                "a plugin stopped: plugin `plugin:x` failed: nothing provides y"
            )
            .map(|h| h.id),
            Some("nothing-provides")
        );
        assert!(hint(Scope::Fail, "Network error: connection reset").is_none());
    }

    #[test]
    fn a_stop_says_what_to_do_and_a_plugins_stop_does_not_say_continue() {
        assert_eq!(
            what_to_do(true, "[Agent stopped: Max turns reached (1/1)]"),
            Some(HINTS[0].fix)
        );
        assert_eq!(
            what_to_do(true, "[Agent stopped: the policy said no]"),
            Some(OTHER_STOP)
        );
        assert_eq!(what_to_do(true, ""), Some(OTHER_STOP));
        assert_eq!(what_to_do(false, "something else"), None);
        let ids: std::collections::HashSet<&str> = HINTS.iter().map(|h| h.id).collect();
        assert_eq!(ids.len(), HINTS.len(), "ids are unique");
    }

    #[test]
    fn the_skills_failures_name_every_hint_and_only_these() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/skill/yobuild/reference/failures.md"
        );
        let text = std::fs::read_to_string(path).expect("the skill's failures.md");
        let marker = "<!-- hint: ";
        let named: Vec<&str> = text
            .match_indices(marker)
            .map(|(at, _)| {
                let rest = &text[at + marker.len()..];
                rest[..rest.find(" -->").expect("a closed marker")].trim()
            })
            .collect();
        for h in HINTS {
            assert!(
                named.contains(&h.id),
                "failures.md does not name hint {}",
                h.id
            );
        }
        for id in &named {
            assert!(
                HINTS.iter().any(|h| h.id == *id),
                "failures.md names {id}, which is no hint"
            );
        }
    }
}
