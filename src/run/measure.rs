//! What each run cost the plugins (design §9: measure before promoting a
//! plugin to a Rust built-in): each handler's hook time, measured at the
//! bridge (for a language plugin, the round trip to its runtime), and each
//! plugin runtime's memory, read from the OS (rutis gives no pid: the
//! runtimes are this process's children, each named by its folder's
//! `package.json` as its last argument). After each run: an Info notice
//! when asked for (`--measure`, and with `--once`), else the log (info).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use yoagent::extension::{Extension, ExtensionError, RunContext, RunHooks, RunOutcome};
use yoagent_rutis::registry::{HookTiming, Registry};

use crate::frontend::{NoticeLevel, Session};

/// Says each run's plugin time and the runtimes' memory.
#[derive(Clone)]
pub(crate) struct Measure {
    /// Where to say it: a notice to every frontend, or the log only.
    session: Option<Session>,
    registry: Arc<Registry>,
    /// Each runtime's name and folder.
    runtimes: Arc<Vec<(String, PathBuf)>>,
    /// Handler names to say otherwise: the adapters' per-process names as
    /// their usual ones (`pi-extensions-<uuid>` → `pi-extensions`).
    names: Arc<Vec<(String, String)>>,
}

impl Measure {
    /// `session`: say it there (a notice); `None`: in the log.
    pub(crate) fn new(
        session: Option<Session>,
        registry: Arc<Registry>,
        runtimes: Vec<(String, PathBuf)>,
        names: Vec<(String, String)>,
    ) -> Self {
        // What came before the first run (loading) is not a run's.
        registry.take_timings();
        Self {
            session,
            registry,
            runtimes: Arc::new(runtimes),
            names: Arc::new(names),
        }
    }
}

impl Measure {
    /// How a handler is said: the adapters by their usual names; a manifest
    /// plugin's by its row (`plugin:<name>`, the owner the host saw), not
    /// the name it chose; any other as it is.
    fn said(&self, handler: &str, owners: &[(String, String)]) -> String {
        if let Some((_, said)) = self.names.iter().find(|(name, _)| name == handler) {
            return said.clone();
        }
        match owners.iter().find(|(name, _)| name == handler) {
            Some((_, owner)) if owner.starts_with("plugin:") => owner.clone(),
            _ => handler.to_owned(),
        }
    }
}

#[async_trait::async_trait]
impl Extension for Measure {
    fn name(&self) -> &str {
        "measure"
    }

    async fn start_run(&self, _run: &RunContext<'_>) -> Result<Box<dyn RunHooks>, ExtensionError> {
        Ok(Box::new(self.clone()))
    }
}

#[async_trait::async_trait]
impl RunHooks for Measure {
    async fn finish(&mut self, _outcome: &RunOutcome) {
        // Every plugin's `finish` is counted (the bridge's runs before this,
        // the last extension); the plugins' handling of this run's end event
        // (`on_event`, sent after the extensions' `finish`) counts in the
        // next run's.
        let owners: Vec<(String, String)> = self
            .registry
            .handlers()
            .iter()
            .map(|h| (h.name().to_owned(), h.owner().to_owned()))
            .collect();
        let spent: Vec<Spent> = self
            .registry
            .take_timings()
            .iter()
            .map(Spent::from)
            .map(|mut s| {
                s.handler = self.said(&s.handler, &owners);
                s
            })
            .collect();
        let memory = memory(&self.runtimes).await;
        let Some(text) = report(&spent, &memory) else {
            return;
        };
        match &self.session {
            Some(session) => session.notice(NoticeLevel::Info, text),
            None => tracing::info!("{text}"),
        }
    }
}

/// One handler's time in one kind of hook (or one of its tools).
#[derive(Debug, Clone, PartialEq)]
struct Spent {
    handler: String,
    /// `before_tool`, …, or `tool <name>`.
    what: String,
    calls: u64,
    failed: u64,
    cancelled: u64,
    total: Duration,
    max: Duration,
}

impl From<&HookTiming> for Spent {
    fn from(t: &HookTiming) -> Self {
        Spent {
            handler: t.handler.clone(),
            what: match &t.tool {
                Some(tool) => format!("tool {tool}"),
                None => t.hook.to_owned(),
            },
            calls: t.calls,
            failed: t.failed,
            cancelled: t.cancelled,
            total: t.total,
            max: t.max,
        }
    }
}

fn ms(d: Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    // Below 10 ms as written (9.96 is "10 ms", not "10.0 ms").
    match (ms * 10.0).round() < 100.0 {
        true => format!("{ms:.1} ms"),
        false => format!("{ms:.0} ms"),
    }
}

/// "Plugin time this run: <handler> <total> (<what> <calls>× <total>[ max
/// <max>][, <n> failed][, <n> cancelled]; …); …. Runtime memory: <name>
/// <MB>, …." — handlers by time spent, most first; `None` with nothing to
/// say.
fn report(spent: &[Spent], memory: &[(String, u64)]) -> Option<String> {
    let mut handlers: Vec<(&str, Duration, Vec<&Spent>)> = Vec::new();
    for s in spent.iter().filter(|s| s.calls > 0) {
        match handlers.iter_mut().find(|(h, _, _)| *h == s.handler) {
            Some((_, total, items)) => {
                *total += s.total;
                items.push(s);
            }
            None => handlers.push((&s.handler, s.total, vec![s])),
        }
    }
    handlers.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let mut parts = Vec::new();
    if !handlers.is_empty() {
        let each: Vec<String> = handlers
            .iter_mut()
            .map(|(handler, total, items)| {
                items.sort_by(|a, b| b.total.cmp(&a.total).then(a.what.cmp(&b.what)));
                let items: Vec<String> = items
                    .iter()
                    .map(|s| {
                        let mut line = format!("{} {}× {}", s.what, s.calls, ms(s.total));
                        if s.calls > 1 {
                            line.push_str(&format!(" max {}", ms(s.max)));
                        }
                        if s.failed > 0 {
                            line.push_str(&format!(", {} failed", s.failed));
                        }
                        if s.cancelled > 0 {
                            line.push_str(&format!(", {} cancelled", s.cancelled));
                        }
                        line
                    })
                    .collect();
                format!("{handler} {} ({})", ms(*total), items.join("; "))
            })
            .collect();
        parts.push(format!("Plugin time this run: {}.", each.join("; ")));
    }
    if !memory.is_empty() {
        let each: Vec<String> = memory
            .iter()
            .map(|(name, kb)| format!("{name} {} MB", kb.div_ceil(1024)))
            .collect();
        parts.push(format!("Runtime memory: {}.", each.join(", ")));
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// How long `ps` may take (it can block on a process stuck in the kernel).
#[cfg(unix)]
const PS_TIME: Duration = Duration::from_secs(2);

/// Each runtime's resident memory in KB: the processes this one started
/// for it (their last argument is its folder's `package.json`) and what
/// those started in turn — runtimes of one folder together ([`by_folder`]).
/// A runtime not found — or `ps` failing or slow — is left out.
#[cfg(unix)]
async fn memory(runtimes: &[(String, PathBuf)]) -> Vec<(String, u64)> {
    let ps = tokio::process::Command::new("ps")
        // `-ww`: never cut the command line (an exported COLUMNS would).
        .args(["-ww", "-A", "-o", "pid=,ppid=,rss=,command="])
        .kill_on_drop(true)
        .output();
    let out = match tokio::time::timeout(PS_TIME, ps).await {
        Ok(Ok(out)) if out.status.success() => out,
        other => {
            tracing::debug!("runtime memory not read: ps {other:?}");
            return Vec::new();
        }
    };
    let rows: Vec<(u32, u32, u64, String)> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            let pid = f.next()?.parse().ok()?;
            let ppid = f.next()?.parse().ok()?;
            let rss = f.next()?.parse().ok()?;
            Some((pid, ppid, rss, f.collect::<Vec<_>>().join(" ")))
        })
        .collect();
    let me = std::process::id();
    let tree = |root: u32| -> u64 {
        let mut total = 0;
        let mut next = vec![root];
        while let Some(pid) = next.pop() {
            for (p, parent, rss, _) in &rows {
                if *p == pid {
                    total += rss;
                }
                if *parent == pid {
                    next.push(*p);
                }
            }
        }
        total
    };
    by_folder(runtimes)
        .into_iter()
        .filter_map(|(dir, names)| {
            let anchor = format!(" {}", dir.join("package.json").display());
            let total: u64 = rows
                .iter()
                .filter(|(_, parent, _, command)| *parent == me && command.ends_with(&anchor))
                .map(|(pid, ..)| tree(*pid))
                .sum();
            (total > 0).then_some((names, total))
        })
        .collect()
}

/// The runtimes by folder, in order, each folder's names joined (`ui +
/// rutis-0`): runtimes started from one folder (the terminal UI's and a
/// rutis package's, both `frontend/plugins`) cannot be told apart from
/// outside, so they are said together rather than each given the other's.
// Memory is read on unix only (`ps`).
#[cfg_attr(not(unix), allow(dead_code))]
fn by_folder(runtimes: &[(String, PathBuf)]) -> Vec<(&PathBuf, String)> {
    let mut folders: Vec<(&PathBuf, Vec<&str>)> = Vec::new();
    for (name, dir) in runtimes {
        match folders.iter_mut().find(|(d, _)| *d == dir) {
            Some((_, names)) => names.push(name),
            None => folders.push((dir, vec![name])),
        }
    }
    folders
        .into_iter()
        .map(|(dir, names)| (dir, names.join(" + ")))
        .collect()
}

/// Elsewhere not read.
#[cfg(not(unix))]
async fn memory(_runtimes: &[(String, PathBuf)]) -> Vec<(String, u64)> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spent(handler: &str, what: &str, calls: u64, total_ms: u64, max_ms: u64) -> Spent {
        Spent {
            handler: handler.into(),
            what: what.into(),
            calls,
            failed: 0,
            cancelled: 0,
            total: Duration::from_millis(total_ms),
            max: Duration::from_millis(max_ms),
        }
    }

    #[test]
    fn the_report_names_each_handlers_time_and_each_runtimes_memory() {
        let mut failed = spent("pi-extensions", "before_tool", 2, 3, 2);
        failed.failed = 1;
        let lines = [
            spent("dsh-tools", "tools", 1, 1, 1),
            spent("pi-extensions", "tool pi_echo", 1, 12, 12),
            failed,
            spent("pi-extensions", "on_input", 0, 0, 0),
        ];
        assert_eq!(
            report(&lines, &[("pi".into(), 346_000), ("dsh".into(), 161_792)]).unwrap(),
            "Plugin time this run: pi-extensions 15 ms (tool pi_echo 1× 12 ms; \
             before_tool 2× 3.0 ms max 2.0 ms, 1 failed); dsh-tools 1.0 ms (tools 1× 1.0 ms). \
             Runtime memory: pi 338 MB, dsh 158 MB."
        );
        assert_eq!(report(&[], &[]), None);
        assert_eq!(
            report(&[], &[("ui".into(), 1)]).unwrap(),
            "Runtime memory: ui 1 MB."
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_runtime_not_running_is_left_out() {
        assert!(memory(&[("pi".into(), PathBuf::from("/nowhere"))])
            .await
            .is_empty());
    }

    #[test]
    fn handlers_are_said_by_the_adapters_names_or_their_plugins_row() {
        let measure = Measure::new(
            None,
            Arc::new(Registry::default()),
            Vec::new(),
            vec![("pi-extensions-x1".into(), "pi-extensions".into())],
        );
        let owners = [
            ("pi-extensions-x1".to_owned(), "pi".to_owned()),
            ("words".to_owned(), "plugin:words".to_owned()),
            ("loose".to_owned(), "language plugin (session 1)".to_owned()),
        ];
        assert_eq!(measure.said("pi-extensions-x1", &owners), "pi-extensions");
        assert_eq!(measure.said("words", &owners), "plugin:words");
        assert_eq!(measure.said("loose", &owners), "loose");
    }

    #[test]
    fn runtimes_of_one_folder_are_said_together() {
        let runtimes = [
            ("ui".to_owned(), PathBuf::from("/f")),
            ("pi".to_owned(), PathBuf::from("/p")),
            ("rutis-0".to_owned(), PathBuf::from("/f")),
        ];
        let said: Vec<(&PathBuf, String)> = by_folder(&runtimes);
        assert_eq!(
            said,
            [
                (&PathBuf::from("/f"), "ui + rutis-0".to_owned()),
                (&PathBuf::from("/p"), "pi".to_owned())
            ]
        );
    }

    #[test]
    fn times_below_ten_ms_keep_a_decimal() {
        assert_eq!(ms(Duration::from_micros(9_940)), "9.9 ms");
        assert_eq!(ms(Duration::from_micros(9_960)), "10 ms");
        assert_eq!(ms(Duration::from_micros(270)), "0.3 ms");
    }
}
