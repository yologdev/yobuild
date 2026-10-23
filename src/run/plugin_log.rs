//! What the pi adapter and the DSH host only log, said to the owner (design
//! §12, "Fixed answers, said"): a fixed answer a grant would change, a
//! `pi.exec` refused, a wake the host never got, the DSH host's trouble.
//! They write it through `yoagent.log`, which yoagent-rutis turns into
//! `tracing` events (target `yoagent_rutis::plugin`), the fixed answers at
//! info — below `RUST_LOG`'s default — so the binary adds [`layer`] beside
//! its log, seeing them whatever the filter, and a running session
//! watches (`watch`): each line a `Said` knows becomes a Warning notice, once a
//! session. A line counts only from the row that writes it — the pi
//! adapter's (`pi`), the DSH host's (`dsh-host`) — read from the call it is
//! logged in (`caller_row`): any plugin may call `yoagent.log`, and a line
//! it forged would be yobuild's advice to grant more.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::filter::{Filtered, Targets};
use tracing_subscriber::layer::{Context, Layer};

/// A plugin line: who wrote it (the calling row), at what level, the text.
pub(crate) struct Line<'a> {
    pub row: Option<&'a str>,
    pub level: Level,
    pub text: &'a str,
}

/// Where the lines go: the session's watcher, while one runs, by the id of
/// its guard.
type Sink = Arc<dyn Fn(&Line) + Send + Sync>;
static SINK: Mutex<Option<(u64, Sink)>> = Mutex::new(None);
static WATCHES: AtomicU64 = AtomicU64::new(0);

/// The sink, whatever a panic elsewhere left: a tracing layer never panics.
fn sink() -> MutexGuard<'static, Option<(u64, Sink)>> {
    SINK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The layer to add beside the log: plugin lines at info and above, to the
/// running session's watcher.
pub fn layer<S: Subscriber>() -> Filtered<impl Layer<S>, Targets, S> {
    PluginLines.with_filter(Targets::new().with_target("yoagent_rutis::plugin", Level::INFO))
}

/// [`layer`]'s inner layer.
struct PluginLines;

impl<S: Subscriber> Layer<S> for PluginLines {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let Some((_, sink)) = sink().clone() else {
            return;
        };
        let mut message = Message(String::new());
        event.record(&mut message);
        let row = rutis_bridge::session::caller_row();
        sink(&Line {
            row: row.as_ref().map(|r| r.id()),
            level: *event.metadata().level(),
            text: &message.0,
        });
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

/// Watching while it is kept: plugin lines go to its sink.
#[must_use = "watching stops when the guard drops"]
pub(crate) struct Watching(u64);

impl Drop for Watching {
    fn drop(&mut self) {
        // Only its own: a later watch may have replaced it. Dropped outside
        // the lock (the sink holds a session).
        let old = {
            let mut sink = sink();
            match sink.as_ref().is_some_and(|(id, _)| *id == self.0) {
                true => sink.take(),
                false => None,
            }
        };
        drop(old);
    }
}

/// Sends each plugin line to `sink` until the guard drops. One session a
/// process: a later watch replaces an earlier, whose guard then clears
/// nothing.
pub(crate) fn watch(sink_: impl Fn(&Line) + Send + Sync + 'static) -> Watching {
    let id = WATCHES.fetch_add(1, Ordering::Relaxed);
    let old = sink().replace((id, Arc::new(sink_)));
    drop(old);
    Watching(id)
}

/// The rows whose lines are read: the pi adapter's and the DSH host's.
const PI_ROW: &str = "pi";
const DSH_HOST_ROW: &str = super::host::DSH_HOST_ROW;

/// What a session's pi plugins were given, to tell which lines a grant
/// would change, each said once.
pub(crate) struct Said {
    /// Each pi extension file — as planned and canonical — and its
    /// plugin's name.
    files: Vec<(PathBuf, String)>,
    /// The pi plugins are told the model (`model = "info"`, on all or none).
    model: bool,
    /// `check`'s reading ([`Said::checking`]): each pi plugin's grants.
    checking: Option<Vec<PiGrants>>,
    said: Mutex<HashSet<String>>,
}

/// A pi plugin's grants that change what its lines say: `trust`, and an
/// `exec` other than `"off"`.
pub(crate) struct PiGrants {
    pub plugin: String,
    pub trust: bool,
    pub exec: bool,
}

/// A note: the plugin it is about, when a pi line names one's file; the
/// text.
pub(crate) type Note = (Option<String>, String);

impl Said {
    pub(crate) fn new(pi_files: &[(String, Vec<PathBuf>)], model: bool) -> Self {
        // Each file as given and canonical: a line names it either way.
        let files = pi_files
            .iter()
            .flat_map(|(name, files)| {
                files.iter().flat_map(|f| {
                    let canonical = std::fs::canonicalize(f).ok().filter(|c| c != f);
                    std::iter::once(f.clone())
                        .chain(canonical)
                        .map(|f| (f, name.clone()))
                })
            })
            .collect();
        Said {
            files,
            model,
            checking: None,
            said: Mutex::default(),
        }
    }

    /// As `check` reads lines (design §12 "Fixed answers in `check`"): its
    /// pi probe alone runs with no grants, so what it says for want of one
    /// the plugin has (`grants`) is dropped; and what only `check`'s setting
    /// makes — the gate's refusals of a granted `pi.exec` with nobody there
    /// to answer, a wake — is left out.
    pub(crate) fn checking(
        pi_files: &[(String, Vec<PathBuf>)],
        model: bool,
        grants: Vec<PiGrants>,
    ) -> Self {
        Said {
            checking: Some(grants),
            ..Said::new(pi_files, model)
        }
    }

    /// The notice for `line`, once, with the plugin it is about: `None` for
    /// a line from another row, one no grant changes or nobody needs told,
    /// or one already said.
    pub(crate) fn note(&self, line: &Line) -> Option<Note> {
        // Said once by its key: a fixed answer once per extension and
        // question, whatever the answer (`ctx.model` is `undefined` as an
        // extension starts, the model in a run); anything else by its text.
        let (note, key) = match line.row {
            Some(PI_ROW) => self.pi(line.text),
            Some(DSH_HOST_ROW) if line.level <= Level::WARN => {
                dsh_host(line.text).and_then(|text| keyed(None, text))
            }
            _ => None,
        }?;
        self.said
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key)
            .then_some(note)
    }

    /// `plugin`'s grants as `check` reads them: `None` in `run`.
    fn granted(&self, plugin: &Option<String>) -> Option<(bool, bool)> {
        let grants = self.checking.as_ref()?;
        let found = plugin
            .as_ref()
            .and_then(|name| grants.iter().find(|g| g.plugin == *name));
        Some(found.map_or((false, false), |g| (g.trust, g.exec)))
    }

    /// The note for a pi line, and the key it is said once by.
    fn pi(&self, line: &str) -> Option<(Note, String)> {
        if let Some(rest) = line.strip_prefix("[pi] fixed answer") {
            // ` (<file>): <question> → <answer>`, or with no file.
            let ((plugin, who), rest) = match rest.strip_prefix(" (") {
                Some(rest) => {
                    let (file, rest) = rest.split_once("): ")?;
                    (self.who(file), rest)
                }
                None => (
                    (None, "a pi extension".to_owned()),
                    rest.strip_prefix(": ")?,
                ),
            };
            let (question, answer) = rest.rsplit_once(" → ")?;
            let trusted = self.granted(&plugin).is_some_and(|(trust, _)| trust);
            let grant = match question {
                "ctx.isProjectTrusted()" if answer == "false" && !trusted => "trust = true",
                "ctx.model" | "ctx.modelRegistry" if !self.model => "model = \"info\"",
                _ => return None,
            };
            let note =
                format!("{who} asked {question}: answered {answer} (grant `{grant}` to change it)");
            return Some(((plugin, note), format!("{who}\0{question}")));
        }
        let rest = line.strip_prefix("[pi] ")?;
        // `<file>: refused pi.exec("<command>")<why>`
        if let Some((file, rest)) = rest.split_once(": refused pi.exec(") {
            let (command, why) = rest.split_once(')')?;
            let (plugin, who) = self.who(file);
            let granted = self.granted(&plugin);
            if why.starts_with(" is not allowed by the host") {
                // `check`'s probe alone: refused whatever the grant.
                if granted.is_some_and(|(_, exec)| exec) {
                    return None;
                }
                return keyed(
                    plugin,
                    format!("{who} ran pi.exec({command}): refused (grant `exec = \"gate\"` or `\"free\"` to allow it)"),
                );
            }
            // The owner's own "Block", or a cancel: nothing to tell. In
            // `check`, what only its setting makes — nobody there to answer,
            // the judge or the ask failing — is left out; a bad `cwd` or a
            // missing approval service is said as in `run`.
            let checks_own = why.contains(" was not approved by the host (")
                || why.starts_with(": the host could not judge it: ")
                || why.starts_with(": the host's approval failed: ");
            if why.contains("(block)")
                || why.contains(" was cancelled")
                || (granted.is_some() && checks_own)
            {
                return None;
            }
            let why = why.trim_start_matches(':').trim();
            return keyed(
                plugin,
                format!("{who} ran pi.exec({command}): refused, {why}"),
            );
        }
        // `<file>: wake refused: <why>` — yobuild's own refusals are said
        // where it refuses (`host_session.wake…`). In `check` a wake
        // reaches no one: left out.
        let (file, why) = rest.split_once(": wake refused: ")?;
        if why.contains("host_session.wake") || self.checking.is_some() {
            return None;
        }
        let (plugin, who) = self.who(file);
        keyed(
            plugin,
            format!("{who}'s wake did not reach the agent: {why}"),
        )
    }

    /// The plugin a file is, as planned or canonical, and how it is said;
    /// else no plugin, and the file.
    fn who(&self, file: &str) -> (Option<String>, String) {
        let canonical = std::fs::canonicalize(file).ok();
        let found = self
            .files
            .iter()
            .find(|(f, _)| f == Path::new(file) || Some(f) == canonical.as_ref());
        match found {
            Some((_, name)) => (Some(name.clone()), format!("pi extension {name}")),
            None => (None, format!("pi extension {file}")),
        }
    }
}

/// A note said once by its text.
fn keyed(plugin: Option<String>, text: String) -> Option<(Note, String)> {
    let key = text.clone();
    Some(((plugin, text), key))
}

/// The DSH host's warnings, but its refusals yobuild made and says itself.
fn dsh_host(line: &str) -> Option<String> {
    let text = line.strip_prefix("[dsh-host] ")?;
    (!text.contains("host_session.wake")).then(|| format!("The DSH host: {text}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pi(text: &str) -> Line<'_> {
        Line {
            row: Some(PI_ROW),
            level: Level::INFO,
            text,
        }
    }

    #[test]
    fn a_fixed_answer_a_grant_changes_and_a_refused_exec_are_said_once() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lens.ts");
        std::fs::write(&file, "").unwrap();
        let canonical = std::fs::canonicalize(&file).unwrap();
        let said = Said::new(&[("lens".to_owned(), vec![canonical])], false);
        let at = file.display();
        let trusted = format!("[pi] fixed answer ({at}): ctx.isProjectTrusted() → false");
        assert_eq!(
            said.note(&pi(&trusted)),
            Some((Some("lens".into()), "pi extension lens asked ctx.isProjectTrusted(): answered false (grant `trust = true` to change it)".into()))
        );
        // Once.
        assert_eq!(said.note(&pi(&trusted)), None);
        // Granted already, or no grant changes it: nothing.
        assert_eq!(
            said.note(&pi(&format!(
                "[pi] fixed answer ({at}): ctx.isProjectTrusted() → true"
            ))),
            None
        );
        assert_eq!(
            said.note(&pi(&format!(
                "[pi] fixed answer ({at}): ctx.ui.confirm() (no user to ask) → false"
            ))),
            None
        );
        assert!(said
            .note(&pi(&format!(
                "[pi] fixed answer ({at}): ctx.model → yoagent/x"
            )))
            .unwrap()
            .1
            .contains("grant `model = \"info\"`"));
        // No file: some extension.
        assert_eq!(
            said.note(&pi("[pi] fixed answer: ctx.modelRegistry → a read-only list of 0 model(s)")),
            Some((None, "a pi extension asked ctx.modelRegistry: answered a read-only list of 0 model(s) (grant `model = \"info\"` to change it)".into()))
        );
        // Once per question, whatever the answer: the first said.
        assert_eq!(
            said.note(&pi(&format!(
                "[pi] fixed answer ({at}): ctx.model → deepseek/x"
            ))),
            None
        );
        let told = Said::new(&[], true);
        assert_eq!(
            told.note(&pi("[pi] fixed answer (/x.ts): ctx.model → deepseek/x")),
            None
        );
        assert_eq!(
            said.note(&pi(&format!(
                "[pi] {at}: refused pi.exec(\"git\") is not allowed by the host (the adapter's `exec` / `execFor` setting)"
            ))),
            Some((Some("lens".into()), "pi extension lens ran pi.exec(\"git\"): refused (grant `exec = \"gate\"` or `\"free\"` to allow it)".into()))
        );
        assert_eq!(said.note(&pi("[pi] something else")), None);
    }

    #[test]
    fn the_gates_other_refusals_and_a_lost_wake_are_said_but_the_owners_block() {
        let said = Said::new(&[("g".to_owned(), vec![PathBuf::from("/x/g.ts")])], false);
        for (line, note) in [
            (
                "[pi] /x/g.ts: refused pi.exec(\"rm\"): the host could not judge it: host_approval.judge took longer than 30000 ms",
                "pi extension g ran pi.exec(\"rm\"): refused, the host could not judge it: host_approval.judge took longer than 30000 ms",
            ),
            (
                "[pi] /x/g.ts: refused pi.exec(\"rm\") was not approved by the host (unanswered)",
                "pi extension g ran pi.exec(\"rm\"): refused, was not approved by the host (unanswered)",
            ),
            (
                "[pi] /x/g.ts: wake refused: no answer within 30 s (it may still be delivered; later wakes go on)",
                "pi extension g's wake did not reach the agent: no answer within 30 s (it may still be delivered; later wakes go on)",
            ),
        ] {
            assert_eq!(
                said.note(&pi(line)),
                Some((Some("g".into()), note.into())),
                "{line}"
            );
        }
        for line in [
            "[pi] /x/g.ts: refused pi.exec(\"rm\") was not approved by the host (block)",
            "[pi] /x/g.ts: refused pi.exec(\"rm\") was cancelled while the host was asked",
            "[pi] /x/g.ts: wake refused: invalid binding value: host_session.wake(text, file): …",
        ] {
            assert_eq!(said.note(&pi(line)), None, "{line}");
        }
    }

    /// As `check` reads lines: the probe alone's answers for want of a
    /// grant the plugin has dropped, the gate's other refusals and wakes
    /// left out; a file no plugin loads names none.
    #[test]
    fn check_reads_lines_with_each_plugins_grants() {
        let files = [
            ("open".to_owned(), vec![PathBuf::from("/x/open.ts")]),
            ("granted".to_owned(), vec![PathBuf::from("/x/granted.ts")]),
        ];
        let grants = vec![
            PiGrants {
                plugin: "open".into(),
                trust: false,
                exec: false,
            },
            PiGrants {
                plugin: "granted".into(),
                trust: true,
                exec: true,
            },
        ];
        let said = Said::checking(&files, false, grants);
        let trusted =
            |file: &str| format!("[pi] fixed answer ({file}): ctx.isProjectTrusted() → false");
        let exec = |file: &str| {
            format!("[pi] {file}: refused pi.exec(\"git\") is not allowed by the host (the adapter's `exec` / `execFor` setting)")
        };
        assert_eq!(said.note(&pi(&trusted("/x/granted.ts"))), None);
        assert_eq!(said.note(&pi(&exec("/x/granted.ts"))), None);
        assert_eq!(
            said.note(&pi(&trusted("/x/open.ts"))).unwrap().0.as_deref(),
            Some("open")
        );
        assert_eq!(
            said.note(&pi(&exec("/x/open.ts"))).unwrap().0.as_deref(),
            Some("open")
        );
        // A file no plugin loads: no plugin, said as the file.
        assert_eq!(
            said.note(&pi(&trusted("/y/other.ts"))),
            Some((None, "pi extension /y/other.ts asked ctx.isProjectTrusted(): answered false (grant `trust = true` to change it)".into()))
        );
        for line in [
            "[pi] /x/granted.ts: refused pi.exec(\"rm\") was not approved by the host (unanswered)",
            "[pi] /x/open.ts: refused pi.exec(\"rm\") was not approved by the host (unanswered)",
            "[pi] /x/granted.ts: refused pi.exec(\"rm\"): the host could not judge it: host_approval.judge took longer than 30000 ms",
            "[pi] /x/granted.ts: refused pi.exec(\"rm\"): the host's approval failed: closed",
            "[pi] /x/open.ts: wake refused: no answer within 30 s (it may still be delivered; later wakes go on)",
        ] {
            assert_eq!(said.note(&pi(line)), None, "{line}");
        }
        // The extension's own mistake, or a service missing: said.
        for (line, why) in [
            (
                "[pi] /x/granted.ts: refused pi.exec(\"git\"): options.cwd must be a path string to be judged, not number",
                "options.cwd must be a path string to be judged, not number",
            ),
            (
                "[pi] /x/granted.ts: refused pi.exec(\"git\") needs the host's approval (`exec` \"gate\"): host_approval",
                "needs the host's approval (`exec` \"gate\"): host_approval",
            ),
        ] {
            assert_eq!(
                said.note(&pi(line)),
                Some((
                    Some("granted".into()),
                    format!("pi extension granted ran pi.exec(\"git\"): refused, {why}")
                )),
                "{line}"
            );
        }
        // The model: told with `model`, as `run` reads it.
        let told = Said::checking(&files, true, Vec::new());
        assert_eq!(
            told.note(&pi("[pi] fixed answer (/x/open.ts): ctx.model → yoagent/x")),
            None
        );
        assert_eq!(
            said.note(&pi("[pi] fixed answer (/x/open.ts): ctx.model → yoagent/x"))
                .unwrap()
                .0
                .as_deref(),
            Some("open")
        );
    }

    /// A line names the file as the adapter has it, planned or canonical:
    /// either is the plugin's.
    #[test]
    fn a_file_is_its_plugins_planned_or_canonical() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("lens.ts"), "").unwrap();
        let link = dir.path().join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).unwrap();
        #[cfg(not(unix))]
        return;
        let planned = link.join("lens.ts");
        let canonical = std::fs::canonicalize(&planned).unwrap();
        assert_ne!(planned, canonical);
        let line = |file: &Path| {
            format!(
                "[pi] fixed answer ({}): ctx.isProjectTrusted() → false",
                file.display()
            )
        };
        // Planned as spelled, the line canonical; and the other way.
        for (files, named) in [(&planned, &canonical), (&canonical, &planned)] {
            let said = Said::checking(
                &[("lens".to_owned(), vec![files.clone()])],
                false,
                Vec::new(),
            );
            assert_eq!(
                said.note(&pi(&line(named))).unwrap().0.as_deref(),
                Some("lens"),
                "{} as {}",
                files.display(),
                named.display()
            );
        }
    }

    #[test]
    fn a_line_counts_only_from_the_row_that_writes_it() {
        let said = Said::new(&[], false);
        let forged = "[pi] fixed answer (/x.ts): ctx.isProjectTrusted() → false";
        for row in [None, Some("plugin:evil"), Some(DSH_HOST_ROW)] {
            let line = Line {
                row,
                level: Level::INFO,
                text: forged,
            };
            assert_eq!(said.note(&line), None, "{row:?}");
        }
        // The DSH host's warnings, from its row only, but its info lines.
        let dsh = |level, text| Line {
            row: Some(DSH_HOST_ROW),
            level,
            text,
        };
        assert_eq!(
            said.note(&dsh(
                Level::WARN,
                "[dsh-host] writing the DSH session log failed: EACCES"
            )),
            Some((
                None,
                "The DSH host: writing the DSH session log failed: EACCES".into()
            ))
        );
        assert_eq!(
            said.note(&dsh(Level::INFO, "[dsh-host] turn 1 opened")),
            None
        );
        assert_eq!(
            said.note(&dsh(
                Level::WARN,
                "[dsh-host] the host refused a DSH plugin's steering: host_session.wake(text): …"
            )),
            None
        );
    }

    #[test]
    fn the_layer_hands_plugin_lines_at_info_and_up_to_the_watcher_and_a_guard_clears_its_own() {
        use tracing_subscriber::prelude::*;
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let subscriber = tracing_subscriber::registry().with(layer());
        tracing::subscriber::with_default(subscriber, || {
            let into = |seen: &Arc<Mutex<Vec<String>>>, tag: &'static str| {
                let seen = seen.clone();
                move |line: &Line| seen.lock().unwrap().push(format!("{tag} {}", line.text))
            };
            let first = watch(into(&seen, "first"));
            let second = watch(into(&seen, "second"));
            // The first guard's drop leaves the second's sink.
            drop(first);
            tracing::info!(target: "yoagent_rutis::plugin", "[pi] fixed answer (/x.ts): q → a");
            tracing::debug!(target: "yoagent_rutis::plugin", "not this");
            tracing::warn!(target: "elsewhere", "nor this");
            drop(second);
            tracing::warn!(target: "yoagent_rutis::plugin", "nor after");
        });
        assert_eq!(
            *seen.lock().unwrap(),
            ["second [pi] fixed answer (/x.ts): q → a"]
        );
    }
}
