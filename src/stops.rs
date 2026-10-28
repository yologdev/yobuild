//! What stops a session from outside: Ctrl+C (SIGINT) and, on unix, the
//! terminal closing (SIGHUP) and SIGTERM — listened for from the moment
//! `run` or `check` begins. Every stdio MCP server runs in a process group
//! of its own (yoagent 0.25.16), so a terminal's signal no longer reaches
//! one: yobuild must catch the signal and drop its clients, whose drop
//! kills their groups. Killed by a signal's default instead, it would leave
//! a server that ignores its closed input running.

use std::future::Future;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// A signal that stops the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Stop {
    /// Ctrl+C.
    Interrupt,
    /// The terminal closed.
    Hangup,
    Terminate,
}

impl Stop {
    /// How it is said after "stopped ": "with Ctrl+C", "by SIGHUP (its
    /// terminal closed)", "by SIGTERM".
    pub fn said(self) -> &'static str {
        match self {
            Stop::Interrupt => "with Ctrl+C",
            Stop::Hangup => "by SIGHUP (its terminal closed)",
            Stop::Terminate => "by SIGTERM",
        }
    }
}

/// The signals, listened for from [`listen`](Stops::listen) on: from then
/// on their default (ending the process at once) no longer applies, so
/// whatever runs meanwhile must be raced against [`next`](Stops::next).
#[doc(hidden)]
pub struct Stops {
    #[cfg(unix)]
    signals: [(tokio::signal::unix::Signal, Stop); 3],
}

impl Stops {
    /// Listens now (on unix the handlers are installed at once, so a
    /// signal that comes before the first look is kept). Needs a Tokio
    /// runtime.
    pub fn listen() -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            Ok(Self {
                signals: [
                    (signal(SignalKind::interrupt())?, Stop::Interrupt),
                    (signal(SignalKind::hangup())?, Stop::Hangup),
                    (signal(SignalKind::terminate())?, Stop::Terminate),
                ],
            })
        }
        #[cfg(not(unix))]
        Ok(Self {})
    }

    /// The next signal.
    pub async fn next(&mut self) -> Stop {
        #[cfg(unix)]
        {
            let [(a, sa), (b, sb), (c, sc)] = &mut self.signals;
            tokio::select! {
                _ = a.recv() => *sa,
                _ = b.recv() => *sb,
                _ = c.recv() => *sc,
            }
        }
        #[cfg(not(unix))]
        {
            // Ctrl+C only; a failure to listen is taken for none coming.
            match tokio::signal::ctrl_c().await {
                Ok(()) => Stop::Interrupt,
                Err(e) => {
                    tracing::warn!("cannot listen for Ctrl+C: {e}");
                    std::future::pending().await
                }
            }
        }
    }

    /// `work`, unless a signal comes first: then it is dropped (and what it
    /// started with it) and the error says "stopped with Ctrl+C" (or the
    /// signal's words).
    pub async fn until<T, E: Into<BoxError>>(
        &mut self,
        work: impl Future<Output = Result<T, E>>,
    ) -> Result<T, BoxError> {
        tokio::select! {
            done = work => done.map_err(Into::into),
            stop = self.next() => Err(format!("stopped {}", stop.said()).into()),
        }
    }
}
