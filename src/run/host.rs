//! The host services (design §12, L1): what a plugin's own app would give
//! it, offered to every plugin runtime beside `frontend`, `ui` and
//! `ask_owner`, and answering `Err` for what the manifest did not grant.
//! Always provided: rutis starts no plugin whose injected service is
//! missing. A grant is found by the calling row (`caller_row`): a rutis
//! plugin's row is `plugin:<name>`; the `pi` and `dsh` rows get theirs with
//! their hosts (M3b's later steps), nothing until then.
//!
//! Named `host_…`: every shared service reaches every runtime, and DSH has
//! services of its own named `approval` and `sessions`.
//!
//! - `host_workspace.cwd()` (sync) — the agent's folder (`workspace`).
//! - `host_workspace.trusted()` (sync) — whether the project counts as
//!   trusted for this plugin (`trust`): `false` without the grant.
//! - `host_session.info()` (sync) — `{ id, generation }`: the session's id
//!   and how many times it was reset (any plugin).
//! - `host_session.model()` (sync) — `{ provider, id, contextWindow,
//!   maxTokens }` (`model = "info"`).
//! - `host_session.wake(text)` (sync) — the text reaches the agent as
//!   `[plugin <name>] <text>` (`wake`): steering a run going on; with the
//!   agent idle, a new run, at most [`IDLE_WAKES`] a session; under
//!   `--once`, only while the task's run goes on.
//! - `host_approval.judge(argv, cwd?)` (sync) — the gate's verdict on one
//!   command's words (`cwd`: the agent's folder if absent): `{ free: true }`
//!   or `{ free: false, question, details }`; with no gate, free.
//! - `host_approval.ask(question, details?)` (async) — Allow/Block, put to
//!   the owner naming the plugin; `true` only for "Allow" (nobody there:
//!   `false`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use rutis::{CordisError, Ctx};
use rutis_bridge::session::{host_key, Error, HostDispatch, Reply, Value};
use serde_json::{json, Value as Json};

use crate::builtins::Gate;
use crate::frontend::protocol::{ClientMessage, UiRequest};
use crate::frontend::session::{Session, UI_TIMEOUT};
use crate::manifest::{Grants, Manifest, ModelGrant, PluginKind};

/// The service names, for the loader's catalog (`share`).
pub const WORKSPACE: &str = "host_workspace";
pub const SESSION: &str = "host_session";
pub const APPROVAL: &str = "host_approval";

/// The runs plugins may start by waking an idle agent, in a session.
pub const IDLE_WAKES: u32 = 3;

/// What the host services know.
pub(crate) struct Host {
    session: Session,
    cwd: PathBuf,
    gate: Option<Gate>,
    /// A rutis plugin's grants, by its row (`plugin:<name>`).
    grants: HashMap<String, (String, Grants)>,
    model: Json,
    once: bool,
    idle_wakes: AtomicU32,
}

impl Host {
    pub(crate) fn new(
        session: Session,
        manifest: &Manifest,
        cwd: &Path,
        gate: Option<Gate>,
        once: bool,
    ) -> Self {
        let grants = manifest
            .plugins
            .iter()
            .filter(|p| matches!(p.kind, PluginKind::Rutis { .. }))
            .map(|p| {
                (
                    super::plugins::row_id(&p.name),
                    (p.name.clone(), p.grants.clone()),
                )
            })
            .collect();
        let config = manifest.agent.model.config();
        let model = json!({
            "provider": manifest.agent.model.provider(),
            "id": manifest.agent.model.id(),
            "contextWindow": config.context_window,
            "maxTokens": config.max_tokens,
        });
        Self {
            session,
            cwd: cwd.to_owned(),
            gate,
            grants,
            model,
            once,
            idle_wakes: AtomicU32::new(0),
        }
    }

    /// The calling plugin's name and grants: `None` for a row with none
    /// (the adapters' rows, or no row).
    fn caller(
        row: Option<&str>,
        grants: &HashMap<String, (String, Grants)>,
    ) -> Option<(String, Grants)> {
        // A plugin's own rows are `plugin:<name>` and `plugin:<name>:<row>`.
        let row = row?;
        let name = row.strip_prefix("plugin:")?.split(':').next()?;
        grants.get(&super::plugins::row_id(name)).cloned()
    }

    /// Who is asking, for the owner: a plugin by its name, else its row.
    fn asker(row: Option<&str>) -> String {
        match row {
            Some(row) => row
                .strip_prefix("plugin:")
                .map(|r| r.split(':').next().unwrap_or(r).to_owned())
                .unwrap_or_else(|| row.to_owned()),
            None => "a plugin".to_owned(),
        }
    }
}

/// Provide the three services on `root`.
pub(crate) fn provide(ctx: &Ctx, host: Host) -> Result<(), CordisError> {
    let host = Arc::new(host);
    ctx.provide_as::<dyn HostDispatch>(host_key(WORKSPACE), Arc::new(Workspace(host.clone())))?;
    ctx.provide_as::<dyn HostDispatch>(host_key(SESSION), Arc::new(SessionService(host.clone())))?;
    ctx.provide_as::<dyn HostDispatch>(host_key(APPROVAL), Arc::new(Approval(host)))?;
    Ok(())
}

fn invalid(message: impl Into<String>) -> Error {
    Error::Value(message.into())
}

/// Refused for want of a grant, saying which.
fn not_granted(service: &str, method: &str, grant: &str, row: Option<&str>) -> Error {
    invalid(format!(
        "{service}.{method}: not granted to {} (the manifest's `grants = {{ {grant} = … }}` on its [[plugin]])",
        Host::asker(row)
    ))
}

fn args(value: Value) -> Result<Vec<Json>, Error> {
    value.list()?.into_iter().map(Value::json).collect()
}

struct Workspace(Arc<Host>);

impl Workspace {
    fn invoke_from(&self, row: Option<&str>, method: &str, value: Value) -> Reply {
        let _ = args(value)?;
        let grants = Host::caller(row, &self.0.grants).map(|(_, g)| g);
        match method {
            "cwd" => match grants.is_some_and(|g| g.workspace) {
                true => Ok(Value::Data(json!(self.0.cwd.display().to_string()))),
                false => Err(not_granted(WORKSPACE, "cwd", "workspace", row)),
            },
            "trusted" => Ok(Value::Data(json!(grants.is_some_and(|g| g.trust)))),
            other => Err(invalid(format!("{WORKSPACE} has no method `{other}`"))),
        }
    }
}

impl HostDispatch for Workspace {
    fn invoke(&self, method: &str, value: Value) -> Reply {
        let row = rutis_bridge::session::caller_row();
        self.invoke_from(row.as_ref().map(|r| r.id()), method, value)
    }

    fn methods(&self) -> Option<Json> {
        Some(json!({ "cwd": "sync", "trusted": "sync" }))
    }
}

struct SessionService(Arc<Host>);

impl SessionService {
    fn invoke_from(&self, row: Option<&str>, method: &str, value: Value) -> Reply {
        let host = &self.0;
        let args = args(value)?;
        let caller = Host::caller(row, &host.grants);
        match method {
            "info" => Ok(Value::Data(json!({
                "id": host.session.id(),
                "generation": host.session.generation(),
            }))),
            "model" => match caller
                .as_ref()
                .is_some_and(|(_, g)| g.model == Some(ModelGrant::Info))
            {
                true => Ok(Value::Data(host.model.clone())),
                false => Err(not_granted(SESSION, "model", "model", row)),
            },
            "wake" => {
                let Some((name, _)) = caller.filter(|(_, g)| g.wake) else {
                    return Err(not_granted(SESSION, "wake", "wake", row));
                };
                let text = match args.first() {
                    Some(Json::String(text)) if !text.trim().is_empty() => text.trim().to_owned(),
                    _ => {
                        return Err(invalid(format!(
                            "{SESSION}.wake(text): text is a non-empty string"
                        )))
                    }
                };
                if host.session.is_closed() {
                    return Err(invalid(format!(
                        "{SESSION}.wake(text): the session has ended"
                    )));
                }
                let running = host.session.is_running();
                if !running {
                    if host.once {
                        return Err(invalid(format!(
                            "{SESSION}.wake(text): under --once, only while the task's run goes on"
                        )));
                    }
                    let started = host.idle_wakes.fetch_add(1, Ordering::SeqCst);
                    if started >= IDLE_WAKES {
                        return Err(invalid(format!(
                            "{SESSION}.wake(text): the agent is idle and plugins already started {IDLE_WAKES} runs this session"
                        )));
                    }
                }
                // Tagged as the plugin's: never the owner's words.
                let text = format!("[plugin {name}] {text}");
                if !host.session.send(ClientMessage::Steer { text }) {
                    return Err(invalid(format!(
                        "{SESSION}.wake(text): the session has ended"
                    )));
                }
                tracing::info!(
                    "plugin {name} woke the agent ({})",
                    if running { "steering" } else { "a new run" }
                );
                Ok(Value::Data(json!(if running {
                    "steered"
                } else {
                    "started"
                })))
            }
            other => Err(invalid(format!("{SESSION} has no method `{other}`"))),
        }
    }
}

impl HostDispatch for SessionService {
    fn invoke(&self, method: &str, value: Value) -> Reply {
        let row = rutis_bridge::session::caller_row();
        self.invoke_from(row.as_ref().map(|r| r.id()), method, value)
    }

    fn methods(&self) -> Option<Json> {
        Some(json!({ "info": "sync", "model": "sync", "wake": "sync" }))
    }
}

struct Approval(Arc<Host>);

impl Approval {
    fn invoke_from(&self, row: Option<&str>, method: &str, value: Value) -> Reply {
        let host = self.0.clone();
        let mut args = args(value)?.into_iter();
        match method {
            "judge" => {
                let argv: Vec<String> = match args.next() {
                    Some(Json::Array(words)) => words
                        .into_iter()
                        .map(|w| match w {
                            Json::String(w) => Ok(w),
                            _ => Err(invalid(format!(
                                "{APPROVAL}.judge(argv, cwd?): argv is a list of strings"
                            ))),
                        })
                        .collect::<Result<_, _>>()?,
                    _ => {
                        return Err(invalid(format!(
                            "{APPROVAL}.judge(argv, cwd?): argv is a list of strings"
                        )))
                    }
                };
                let cwd = match args.next() {
                    None | Some(Json::Null) => host.cwd.clone(),
                    Some(Json::String(cwd)) => host.cwd.join(cwd),
                    Some(_) => {
                        return Err(invalid(format!(
                            "{APPROVAL}.judge(argv, cwd?): cwd is a string"
                        )))
                    }
                };
                let Some(gate) = &host.gate else {
                    return Ok(Value::Data(json!({ "free": true })));
                };
                Ok(Value::Data(match gate.check_argv(&argv, &cwd) {
                    crate::builtins::Check::Ask { question, details } => {
                        json!({ "free": false, "question": question, "details": details })
                    }
                    _ => json!({ "free": true }),
                }))
            }
            "ask" => {
                let question = match args.next() {
                    Some(Json::String(q)) if !q.trim().is_empty() && !q.contains(['\n', '\r']) => q,
                    _ => {
                        return Err(invalid(format!(
                            "{APPROVAL}.ask(question, details?): question is one non-empty line"
                        )))
                    }
                };
                let details = match args.next() {
                    None | Some(Json::Null) => String::new(),
                    Some(Json::String(d)) => d,
                    Some(_) => {
                        return Err(invalid(format!(
                            "{APPROVAL}.ask(question, details?): details is a string"
                        )))
                    }
                };
                let asker = Host::asker(row);
                let request = UiRequest::Select {
                    title: format!("{question} (asked by plugin {asker})"),
                    message: details,
                    options: vec!["Allow".into(), "Block".into()],
                    multiple: false,
                };
                Ok(Value::future(async move {
                    let answer = host.session.ask(request, UI_TIMEOUT).await;
                    Ok(Value::Data(json!(answer.as_str() == Some("Allow"))))
                }))
            }
            other => Err(invalid(format!("{APPROVAL} has no method `{other}`"))),
        }
    }
}

impl HostDispatch for Approval {
    fn invoke(&self, method: &str, value: Value) -> Reply {
        let row = rutis_bridge::session::caller_row();
        self.invoke_from(row.as_ref().map(|r| r.id()), method, value)
    }

    fn methods(&self) -> Option<Json> {
        Some(json!({ "judge": "sync", "ask": "async" }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::session::Session;

    /// A manifest with a rutis plugin `p` granted `grants` (a TOML table's
    /// body), and one `q` granted nothing.
    fn make(grants: &str, gate: bool, once: bool) -> (Host, Session, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let text = format!(
            "[agent]\nname = \"a\"\nmodel = \"xai/grok-4.7\"\n\
             [[plugin]]\nrutis = \"./p.ts\"\nruntime = \"node\"\ngrants = {{ {grants} }}\n\
             [[plugin]]\nrutis = \"./q.ts\"\nruntime = \"node\"\n"
        );
        let manifest = Manifest::parse(&text, &dir.path().join("agent.toml"), dir.path()).unwrap();
        let (session, driver) = Session::new(false);
        // The driver stays unrun: the session is idle, open.
        std::mem::forget(driver);
        let gate = gate.then(|| Gate::new(session.clone(), dir.path()));
        let host = Host::new(session.clone(), &manifest, dir.path(), gate, once);
        (host, session, dir)
    }

    fn list(values: Vec<Json>) -> Value {
        Value::List(values.into_iter().map(Value::Data).collect())
    }

    fn data(reply: Reply) -> Json {
        match reply {
            Ok(Value::Data(json)) => json,
            Ok(_) => panic!("expected data"),
            Err(e) => panic!("{e}"),
        }
    }

    fn error(reply: Reply) -> String {
        match reply {
            Err(e) => e.to_string(),
            Ok(_) => panic!("expected an error"),
        }
    }

    const P: Option<&str> = Some("plugin:p");
    const Q: Option<&str> = Some("plugin:q");

    #[test]
    fn the_workspace_is_given_by_the_grant_to_the_plugin_granted() {
        let (host, _, dir) = make("workspace = true, trust = true", true, false);
        let w = Workspace(Arc::new(host));
        assert_eq!(
            data(w.invoke_from(P, "cwd", list(vec![]))),
            json!(dir.path().display().to_string())
        );
        assert_eq!(data(w.invoke_from(P, "trusted", list(vec![]))), json!(true));
        // Its own other rows too; never another plugin, an adapter's row or
        // no row.
        assert!(w
            .invoke_from(Some("plugin:p:2"), "cwd", list(vec![]))
            .is_ok());
        for row in [Q, Some("pi"), Some("dsh"), None] {
            let e = error(w.invoke_from(row, "cwd", list(vec![])));
            assert!(e.contains("not granted") && e.contains("workspace"), "{e}");
            assert_eq!(
                data(w.invoke_from(row, "trusted", list(vec![]))),
                json!(false)
            );
        }
    }

    #[test]
    fn the_session_says_its_id_to_any_plugin_and_the_model_only_when_granted() {
        let (host, session, _dir) = make("model = \"info\"", true, false);
        let s = SessionService(Arc::new(host));
        let info = data(s.invoke_from(Q, "info", list(vec![])));
        assert_eq!(info, json!({ "id": session.id(), "generation": 0 }));
        let model = data(s.invoke_from(P, "model", list(vec![])));
        assert_eq!(model["provider"], "xai");
        assert_eq!(model["id"], "grok-4.7");
        // The preset's window, not the generic one.
        assert!(
            model["contextWindow"].as_u64().unwrap() > 131_072,
            "{model}"
        );
        assert!(error(s.invoke_from(Q, "model", list(vec![]))).contains("not granted"));
    }

    #[test]
    fn a_wake_is_tagged_as_the_plugins_and_bounded() {
        let (host, _, _dir) = make("wake = true", true, false);
        let s = SessionService(Arc::new(host));
        // Not granted: refused.
        assert!(error(s.invoke_from(Q, "wake", list(vec![json!("go")]))).contains("not granted"));
        // Granted, the agent idle: a new run, up to the cap.
        for _ in 0..IDLE_WAKES {
            assert_eq!(
                data(s.invoke_from(P, "wake", list(vec![json!("go")]))),
                json!("started")
            );
        }
        let e = error(s.invoke_from(P, "wake", list(vec![json!("go")])));
        assert!(
            e.contains(&format!("already started {IDLE_WAKES} runs")),
            "{e}"
        );
        assert!(error(s.invoke_from(P, "wake", list(vec![json!(" ")]))).contains("non-empty"));
        // Under --once, idle: refused.
        let (host, _, _dir) = make("wake = true", true, true);
        let s = SessionService(Arc::new(host));
        assert!(error(s.invoke_from(P, "wake", list(vec![json!("go")]))).contains("--once"));
    }

    #[test]
    fn approval_judges_a_commands_words_and_asks_naming_the_plugin() {
        let (host, _, _dir) = make("", true, false);
        let a = Approval(Arc::new(host));
        let judge = |argv: Json| data(a.invoke_from(Q, "judge", list(vec![argv])));
        assert_eq!(judge(json!(["ls", "-la"])), json!({ "free": true }));
        let asks = judge(json!(["rm", "-rf", "x"]));
        assert_eq!(asks["free"], false, "{asks}");
        assert!(asks["question"].is_string(), "{asks}");
        assert!(
            error(a.invoke_from(Q, "judge", list(vec![json!("ls")]))).contains("list of strings")
        );
        assert!(error(a.invoke_from(Q, "ask", list(vec![json!("a\nb")])))
            .contains("one non-empty line"));
        // With the gate off, nothing judges: free.
        let (host, _, _dir) = make("", false, false);
        let a = Approval(Arc::new(host));
        assert_eq!(
            data(a.invoke_from(Q, "judge", list(vec![json!(["rm", "-rf", "x"])]))),
            json!({ "free": true })
        );
    }

    #[tokio::test]
    async fn an_approval_with_nobody_there_is_no() {
        let (host, _, _dir) = make("", true, false);
        let a = Approval(Arc::new(host));
        let Ok(value) = a.invoke_from(Q, "ask", list(vec![json!("Delete it?")])) else {
            panic!("ask refused")
        };
        let answer = rutis_bridge::session::settle(value).await.unwrap();
        assert!(matches!(answer, Value::Data(Json::Bool(false))));
    }
}
