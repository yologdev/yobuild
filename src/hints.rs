//! What to do about a failure (design §11, "Making failures actionable"):
//! one table, a pattern of the reason → the fix, read by `run` (a run that
//! did not complete: a `What to do: …` Notice, and the last line on
//! stderr) and `check` (each problem's `fix`). Patterns match the reason
//! itself, inside whatever wraps it (yoagent's `[Agent stopped: …]`,
//! `[Extension failed: …]`). Each hint has an id that the skill's
//! `reference/failures.md` names (`<!-- hint: id -->`); a test keeps the two
//! in step both ways.

/// One failure's fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Hint {
    /// Named in `skill/yobuild/reference/failures.md` as `<!-- hint: id -->`.
    pub id: &'static str,
    /// Text a message holds when this is its failure (matched without
    /// regard to case).
    pub pattern: &'static str,
    pub fix: &'static str,
}

/// The fixes, most specific first (the first match wins: "session budget
/// of" before "budget of").
pub(crate) const HINTS: &[Hint] = &[
    // How runs end.
    Hint {
        id: "max-turns",
        pattern: "Max turns reached",
        fix: "send \"continue\" (a new run, with fresh limits; not under --once, which ends after one run), or raise [limits] max_turns",
    },
    Hint {
        id: "max-tokens",
        pattern: "Max tokens reached",
        fix: "send \"continue\" (a new run, with fresh limits; not under --once), or raise [limits] max_tokens",
    },
    Hint {
        id: "max-duration",
        pattern: "Max duration reached",
        fix: "send \"continue\" (a new run, with fresh limits; not under --once), or raise [limits] max_minutes",
    },
    Hint {
        id: "session-budget",
        pattern: "session budget of",
        fix: "\"continue\" stops again at once (the session's total stays spent): restart, which starts a new conversation, or raise [limits] session_budget_usd",
    },
    Hint {
        id: "budget",
        pattern: "budget of",
        fix: "send \"continue\" (each run has its own budget; not under --once), or raise [limits] budget_usd",
    },
    Hint {
        id: "loop",
        pattern: "repeated tool call",
        fix: "change the prompt, or what the tool returns, so the agent does not call it the same way again; then send \"continue\"",
    },
    Hint {
        id: "report",
        pattern: "the answer was still not accepted after",
        fix: "the last answer must name every [builtins] report section (anywhere, any case): a model that follows the prompt better, fewer and plainer sections, or remove report; a scripted run's last reply must name them",
    },
    Hint {
        id: "no-policy",
        pattern: "no plugin policy is loaded to judge this call",
        fix: "the policy plugin went away mid-session: restart; the Error notice \"A plugin stopped: …\" and the log say why",
    },
    Hint {
        id: "handler-gone",
        pattern: "is no longer available: its plugin unloaded",
        fix: "its plugin's runtime died or it was unloaded: restart; the log says why",
    },
    Hint {
        id: "policy-timeout",
        pattern: "did not answer `before_tool` within",
        fix: "the policy plugin took longer than its wait: answer its question sooner; the plugin may hang",
    },
    Hint {
        id: "handler-failed",
        pattern: "a required plugin handler failed this run",
        fix: "a plugin's hook failed: the log says which; run yobuild check",
    },
    Hint {
        id: "host-down",
        pattern: "the plugin host is not running",
        fix: "the plugin host shut down: restart",
    },
    Hint {
        id: "run-failed",
        pattern: "The agent's run failed unexpectedly",
        fix: "the agent loop failed: the log says why; restart",
    },
    Hint {
        id: "no-more-replies",
        pattern: "(no more mock responses)",
        fix: "the scripted replies ran out: add replies, and with [builtins] report make the last one name every section",
    },
    Hint {
        id: "rate-limit",
        pattern: "rate limit",
        fix: "the provider is rate-limiting: wait, then send again",
    },
    Hint {
        id: "unauthorized",
        pattern: "401",
        fix: "the provider refused the key: check the key's variable (yobuild names it when missing)",
    },
    // What `check` finds.
    Hint {
        id: "shadowed-builtin",
        pattern: "is named like yoagent's built-in",
        fix: "this pi extension's tool cannot run beside yoagent's own of that name: choose another plugin",
    },
    Hint {
        id: "dsh-services",
        pattern: "DSH plugins wait for services nobody provides",
        fix: "set [dsh] host_services = true for DSH's own services, or add the DSH plugin the hint names as a [[plugin]]",
    },
    Hint {
        id: "nothing-provides",
        pattern: "nothing provides",
        fix: "the plugin injects a service no plugin provides: add the plugin that provides it, or remove the inject",
    },
    Hint {
        id: "not-together",
        pattern: "registered its handlers alone, but none with the other plugins",
        fix: "it conflicts with another plugin: check them in pairs to find which",
    },
    Hint {
        id: "stops-at-first",
        pattern: "a run would stop at its first request",
        fix: "a plugin stops every run before the model is asked: its reason says why",
    },
    Hint {
        id: "plugin-stopped",
        pattern: "stopped: plugin:",
        fix: "the plugin failed while loading or after: its error says why; the log has more",
    },
    Hint {
        id: "mcp-allow",
        pattern: "which it does not offer (it offers:",
        fix: "use the server's own tool names in its allow (it lists them)",
    },
];

/// The hint for `message`, if one fits.
pub(crate) fn hint(message: &str) -> Option<&'static Hint> {
    let lower = message.to_lowercase();
    HINTS
        .iter()
        .find(|h| lower.contains(&h.pattern.to_lowercase()))
}

/// What to do about a run that ended `Stopped` or `Error` with `error`:
/// its hint, or for any other stop the usual way on.
pub(crate) fn what_to_do(stopped: bool, error: &str) -> Option<&'static str> {
    match hint(error) {
        Some(h) => Some(h.fix),
        None if stopped => Some("send \"continue\" to pick up where it left off"),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Messages as the code makes them (each from where it is made), with
    /// the hint each must get.
    fn real() -> Vec<(String, &'static str)> {
        let stopped =
            |reason: &str| format!("{} {reason}]", yoagent::agent_loop::AGENT_STOPPED_PREFIX);
        vec![
            (stopped("Max turns reached (50/50)"), "max-turns"),
            (stopped("Max tokens reached (1000000/1000000)"), "max-tokens"),
            (stopped("Max duration reached (600s/600s)"), "max-duration"),
            (stopped("session budget of $2.00 spent ($2.0100)"), "session-budget"),
            (stopped("budget of $1.00 spent ($1.0012)"), "budget"),
            (
                format!(
                    "{} bash was called repeatedly with identical arguments after being asked to change approach.]",
                    yoagent::agent_loop::LOOP_ABORT_PREFIX
                ),
                "loop",
            ),
            (
                format!(
                    "{} report] the answer was still not accepted after 3 continues",
                    yoagent::extension::EXTENSION_FAILED_PREFIX
                ),
                "report",
            ),
            ("no plugin policy is loaded to judge this call".into(), "no-policy"),
            (
                "plugin handler `guard` is no longer available: its plugin unloaded".into(),
                "handler-gone",
            ),
            (
                "plugin handler `p` did not answer `before_tool` within 150s".into(),
                "policy-timeout",
            ),
            (
                "a required plugin handler failed this run: boom".into(),
                "handler-failed",
            ),
            (
                "input rejected: the plugin host is not running (shut down, disposed or restarted since the bridge was installed)".into(),
                "host-down",
            ),
            (
                "The agent's run failed unexpectedly (see the host's logs); later runs may be missing tools.".into(),
                "run-failed",
            ),
            ("(no more mock responses)".into(), "no-more-replies"),
            ("its tool `read_file` is named like yoagent's built-in: `run` refuses it".into(), "shadowed-builtin"),
            (
                "`run` would refuse it at start: DSH plugins wait for services nobody provides: fs".into(),
                "dsh-services",
            ),
            (
                "`run` would refuse it at start: plugin:needy (waiting): nothing provides nosuchService".into(),
                "nothing-provides",
            ),
            (
                "plugin:lib registered its handlers alone, but none with the other plugins".into(),
                "not-together",
            ),
            ("a run would stop at its first request: no".into(), "stops-at-first"),
            ("it stopped: plugin:late (failed): boom".into(), "plugin-stopped"),
            (
                "mcp files: its `allow` names x, which it does not offer (it offers: y)".into(),
                "mcp-allow",
            ),
            ("provider error: 429 Too Many Requests: rate limit exceeded".into(), "rate-limit"),
            ("provider error: 401 Unauthorized".into(), "unauthorized"),
        ]
    }

    #[test]
    fn each_real_message_gets_its_hint_and_each_hint_a_message() {
        let real = real();
        for (message, id) in &real {
            assert_eq!(hint(message).map(|h| h.id), Some(*id), "{message}");
        }
        for h in HINTS {
            assert!(
                real.iter().any(|(_, id)| *id == h.id),
                "no real message for {}",
                h.id
            );
        }
    }

    #[test]
    fn ids_are_unique_and_a_stop_without_a_hint_says_continue() {
        let mut ids: Vec<&str> = HINTS.iter().map(|h| h.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), HINTS.len());
        assert_eq!(
            what_to_do(true, "[Agent stopped: a plugin said so]"),
            Some("send \"continue\" to pick up where it left off")
        );
        assert_eq!(what_to_do(false, "something else"), None);
    }

    #[test]
    fn the_skills_failures_name_every_hint_and_only_these() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/skill/yobuild/reference/failures.md"
        );
        let text = std::fs::read_to_string(path).expect("the skill's failures.md");
        let named: Vec<&str> = text
            .match_indices("<!-- hint: ")
            .map(|(at, _)| {
                let rest = &text[at + "<!-- hint: ".len()..];
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
