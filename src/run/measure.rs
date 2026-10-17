//! What each run cost the plugins (design §9: measure before promoting a
//! plugin to a Rust built-in): each handler's hook time, measured at the
//! bridge (for a language plugin, the round trip to its runtime), and each
//! plugin runtime's memory, read from the OS (rutis gives no pid: the
//! runtimes are this process's children, each named by its folder's
//! `package.json` on its command line). Said after each run as an Info
//! notice.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use yoagent::extension::{Extension, ExtensionError, RunContext, RunHooks, RunOutcome};
use yoagent_rutis::registry::{HookTiming, Registry};

use crate::frontend::{NoticeLevel, Session};

/// Says each run's plugin time and the runtimes' memory.
#[derive(Clone)]
pub(crate) struct Measure {
    session: Session,
    registry: Arc<Registry>,
    /// Each runtime's name and folder.
    runtimes: Arc<Vec<(String, PathBuf)>>,
}

impl Measure {
    pub(crate) fn new(
        session: Session,
        registry: Arc<Registry>,
        runtimes: Vec<(String, PathBuf)>,
    ) -> Self {
        // What came before the first run (loading) is not a run's.
        registry.take_timings();
        Self {
            session,
            registry,
            runtimes: Arc::new(runtimes),
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
        // A hook still to come in this run's `finish` counts in the next.
        let spent: Vec<Spent> = self
            .registry
            .take_timings()
            .iter()
            .map(Spent::from)
            .collect();
        let memory = memory(&self.runtimes);
        if let Some(text) = report(&spent, &memory) {
            self.session.notice(NoticeLevel::Info, text);
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
    match ms < 10.0 {
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

/// Each runtime's resident memory in KB: the process this one started for
/// it (its command line names its folder's `package.json`) and what that
/// one started in turn. A runtime not found is left out.
#[cfg(unix)]
fn memory(runtimes: &[(String, PathBuf)]) -> Vec<(String, u64)> {
    let Ok(out) = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,rss=,command="])
        .output()
    else {
        return Vec::new();
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
    runtimes
        .iter()
        .filter_map(|(name, dir)| {
            let manifest = dir.join("package.json").display().to_string();
            let (pid, ..) = rows
                .iter()
                .find(|(_, parent, _, command)| *parent == me && command.contains(&manifest))?;
            Some((name.clone(), tree(*pid)))
        })
        .collect()
}

/// Elsewhere not read.
#[cfg(not(unix))]
fn memory(_runtimes: &[(String, PathBuf)]) -> Vec<(String, u64)> {
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
    #[test]
    fn a_runtime_not_running_is_left_out() {
        assert!(memory(&[("pi".into(), PathBuf::from("/nowhere"))]).is_empty());
    }
}
