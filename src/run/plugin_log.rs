//! What the pi adapter only logs, said to the owner (design §12, "Fixed
//! answers, said"): a fixed answer a grant would change, and a `pi.exec`
//! the manifest does not allow. The adapter writes them through
//! `yoagent.log`, which yoagent-rutis turns into `tracing` events (target
//! `yoagent_rutis::plugin`), the fixed answers at info — below `RUST_LOG`'s
//! default — so the binary adds [`layer`] beside its log, seeing them
//! whatever the filter, and a running session [`watch`]es: each line a
//! [`Said`] knows becomes a Warning notice, once a session.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::filter::{Filtered, Targets};
use tracing_subscriber::layer::{Context, Layer};

/// Where the lines go: the session's watcher, while one runs.
type Sink = Arc<dyn Fn(&str) + Send + Sync>;
static SINK: Mutex<Option<Sink>> = Mutex::new(None);

/// The layer to add beside the log: plugin lines at info and above, to the
/// running session's watcher.
pub fn layer<S>() -> Filtered<PluginLines, Targets, S>
where
    S: Subscriber,
{
    PluginLines.with_filter(Targets::new().with_target("yoagent_rutis::plugin", Level::INFO))
}

/// [`layer`]'s inner layer.
pub struct PluginLines;

impl<S: Subscriber> Layer<S> for PluginLines {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let sink = SINK.lock().unwrap().clone();
        let Some(sink) = sink else {
            return;
        };
        let mut message = Message(String::new());
        event.record(&mut message);
        sink(&message.0);
    }
}

struct Message(String);

impl Visit for Message {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0 = value.to_owned();
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

/// Watching while it is kept: plugin lines go to `sink`.
pub(crate) struct Watching;

impl Drop for Watching {
    fn drop(&mut self) {
        *SINK.lock().unwrap() = None;
    }
}

/// Sends each plugin line to `sink` until the guard drops (one session a
/// process: a later watch replaces an earlier).
pub(crate) fn watch(sink: impl Fn(&str) + Send + Sync + 'static) -> Watching {
    *SINK.lock().unwrap() = Some(Arc::new(sink));
    Watching
}

/// What a session's pi plugins were given, to tell which lines a grant
/// would change, each said once.
pub(crate) struct Said {
    /// Each pi extension file (canonical) and its plugin's name.
    files: Vec<(PathBuf, String)>,
    /// The pi plugins are told the model (`model = "info"`, on all or none).
    model: bool,
    said: Mutex<HashSet<String>>,
}

impl Said {
    pub(crate) fn new(pi_files: &[(String, Vec<PathBuf>)], model: bool) -> Self {
        let files = pi_files
            .iter()
            .flat_map(|(name, files)| files.iter().map(|f| (f.clone(), name.clone())))
            .collect();
        Said {
            files,
            model,
            said: Mutex::default(),
        }
    }

    /// The notice for `line`, once: `None` for a line no grant changes, or
    /// one already said.
    pub(crate) fn note(&self, line: &str) -> Option<String> {
        let note = self.of(line)?;
        self.said
            .lock()
            .unwrap()
            .insert(note.clone())
            .then_some(note)
    }

    fn of(&self, line: &str) -> Option<String> {
        if let Some(rest) = line.strip_prefix("[pi] fixed answer (") {
            // `(<file>): <question> → <answer>`
            let (file, rest) = rest.split_once("): ")?;
            let (question, answer) = rest.rsplit_once(" → ")?;
            let grant = match question {
                "ctx.isProjectTrusted()" if answer == "false" => "trust = true",
                "ctx.model" | "ctx.modelRegistry" if !self.model => "model = \"info\"",
                _ => return None,
            };
            return Some(format!(
                "{} asked {question}: answered {answer} (grant `{grant}` to change it)",
                self.who(file)
            ));
        }
        // `[pi] <file>: refused pi.exec("<command>") is not allowed by the host …`
        let rest = line.strip_prefix("[pi] ")?;
        let (file, rest) = rest.split_once(": refused pi.exec(")?;
        let (command, _) = rest.split_once(") is not allowed by the host")?;
        Some(format!(
            "{} ran pi.exec({command}): refused (grant `exec = \"gate\"` or `\"free\"` to allow it)",
            self.who(file)
        ))
    }

    /// The plugin a file is, by its canonical path; else the file.
    fn who(&self, file: &str) -> String {
        let canonical = std::fs::canonicalize(file).unwrap_or_else(|_| PathBuf::from(file));
        match self.files.iter().find(|(f, _)| *f == canonical) {
            Some((_, name)) => format!("pi extension {name}"),
            None => format!("pi extension {file}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fixed_answer_a_grant_changes_and_a_refused_exec_are_said_once() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lens.ts");
        std::fs::write(&file, "").unwrap();
        let canonical = std::fs::canonicalize(&file).unwrap();
        let said = Said::new(&[("lens".to_owned(), vec![canonical])], false);
        let at = file.display();
        assert_eq!(
            said.note(&format!("[pi] fixed answer ({at}): ctx.isProjectTrusted() → false")),
            Some("pi extension lens asked ctx.isProjectTrusted(): answered false (grant `trust = true` to change it)".into())
        );
        // Once.
        assert_eq!(
            said.note(&format!(
                "[pi] fixed answer ({at}): ctx.isProjectTrusted() → false"
            )),
            None
        );
        // Granted already, or no grant changes it: nothing.
        assert_eq!(
            said.note(&format!(
                "[pi] fixed answer ({at}): ctx.isProjectTrusted() → true"
            )),
            None
        );
        assert_eq!(
            said.note(&format!(
                "[pi] fixed answer ({at}): ctx.ui.confirm() (no user to ask) → false"
            )),
            None
        );
        assert!(said
            .note(&format!("[pi] fixed answer ({at}): ctx.model → yoagent/x"))
            .unwrap()
            .contains("grant `model = \"info\"`"));
        let told = Said::new(&[], true);
        assert_eq!(
            told.note("[pi] fixed answer (/x.ts): ctx.model → deepseek/x"),
            None
        );
        assert_eq!(
            said.note(&format!(
                "[pi] {at}: refused pi.exec(\"git\") is not allowed by the host (the adapter's `exec` / `execFor` setting)"
            )),
            Some("pi extension lens ran pi.exec(\"git\"): refused (grant `exec = \"gate\"` or `\"free\"` to allow it)".into())
        );
        assert_eq!(said.note("[pi] something else"), None);
    }

    #[test]
    fn the_layer_hands_plugin_lines_to_the_watcher_whatever_the_level() {
        use tracing_subscriber::prelude::*;
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let subscriber = tracing_subscriber::registry().with(layer());
        tracing::subscriber::with_default(subscriber, || {
            let into = seen.clone();
            let _watching = watch(move |line| into.lock().unwrap().push(line.to_owned()));
            tracing::info!(target: "yoagent_rutis::plugin", "[pi] fixed answer (/x.ts): q → a");
            tracing::debug!(target: "yoagent_rutis::plugin", "not this");
            tracing::warn!(target: "elsewhere", "nor this");
        });
        assert_eq!(*seen.lock().unwrap(), ["[pi] fixed answer (/x.ts): q → a"]);
    }
}
