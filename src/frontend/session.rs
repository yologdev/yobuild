//! The session: one agent, any number of frontends.
//!
//! [`Session`] is the handle frontends and plugins use (cheap to clone);
//! [`Driver`] owns the command queue and runs the agent. Every
//! [`ServerMessage`] goes to every connected frontend over its own unbounded
//! channel — in order, none dropped — so `RunEnd` arrives even when a slow
//! frontend fell behind on streamed text.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value as Json;
use tokio::sync::{mpsc, oneshot};
use yoagent::agent_loop::AGENT_STOPPED_PREFIX;
use yoagent::{Agent, AgentEvent, AgentMessage, Message, SessionStats, StopReason, StreamDelta};

use crate::frontend::protocol::{
    ClientMessage, NoticeLevel, PendingUiRequest, ResolveReason, RunOutcome, ServerMessage,
    UiPlugin, UiRequest,
};

/// Flush period for merged text deltas: a held delta waits at most this long.
const COALESCE: Duration = Duration::from_millis(30);

/// How long a plugin's question waits for an answer before the safe default.
pub const UI_TIMEOUT: Duration = Duration::from_secs(300);

/// How many of the latest runs' messages (`runStart`, `event`, `runEnd`,
/// `notice`) the session keeps to replay to a frontend that connects later,
/// so a page opened mid-run or after it shows what happened.
pub const HISTORY: usize = 5000;

/// The error of a run whose agent task failed (no `AgentEnd`).
pub(crate) const TASK_FAILED: &str = "the run ended unexpectedly: the agent task failed";

/// How long a withdrawal for a question not asked yet is remembered: the
/// asker's `withdraw` can overtake its own `request` on the way to the host.
const EARLY_WITHDRAWAL: Duration = Duration::from_secs(60);

#[derive(Default)]
struct UiState {
    next_id: u64,
    /// Open questions: who waits for the answer, and the question (replayed
    /// to frontends that connect while it is open).
    pending: HashMap<u64, (oneshot::Sender<Json>, UiRequest)>,
    /// Asker-chosen keys of open questions, for [`Session::withdraw`].
    keys: HashMap<String, u64>,
    /// Keys withdrawn before their question was asked.
    early: HashMap<String, Instant>,
    /// Questions asked with [`Session::ask_later`]: they belong to the
    /// conversation, so a reset withdraws them.
    held: HashSet<u64>,
}

/// An attached frontend, or an observer ([`Session::observe`]).
struct Client {
    tx: mpsc::UnboundedSender<ServerMessage>,
    /// Watching only ([`Session::observe`]): not a frontend a question can
    /// be put to.
    observer: bool,
}

struct Inner {
    clients: Mutex<HashMap<u64, Client>>,
    next_client: AtomicU64,
    ui: Mutex<UiState>,
    /// Offered browser components, with their module source.
    ui_plugins: Mutex<Vec<(UiPlugin, Arc<str>)>>,
    plugin_versions: AtomicU64,
    running: AtomicBool,
    /// The driver ended (or failed): nothing sent is heard any more.
    closed: AtomicBool,
    /// The latest [`HISTORY`] run messages, oldest first. Written under the
    /// `clients` lock (then this one), so a frontend connecting gets each
    /// message once: in its replay, or live.
    history: Mutex<VecDeque<ServerMessage>>,
    /// The session's id, made once (a uuid): its identity for plugins'
    /// hosts (design §12, `host_session.info()`).
    id: String,
    /// Bumped by each Reset: what a host keeps per conversation (a DSH
    /// stand-in agent) starts again at a new generation.
    generation: AtomicU64,
    /// A run has started: the owner's first task, or a prompt of theirs.
    started: AtomicBool,
    /// The runs plugins' wakes may still start ([`Session::wake`]).
    wakes_left: AtomicU32,
    /// `allow_wakes` was called (it sets the allowance once).
    wakes_set: AtomicBool,
}

/// What the driver reads: a frontend's message, or a plugin's wake.
enum Command {
    Client(ClientMessage),
    Wake { text: String },
}

/// The session handle.
#[derive(Clone)]
pub struct Session {
    inner: Arc<Inner>,
    /// Only the handles hold it: once they are all gone, the driver ends.
    commands: mpsc::UnboundedSender<Command>,
}

/// A frontend's connection: its id and the messages for it. Dropping
/// `messages` detaches the frontend at the next broadcast.
pub struct Connection {
    pub id: u64,
    pub messages: mpsc::UnboundedReceiver<ServerMessage>,
}

/// Runs the agent for a [`Session`]; see [`Driver::run`].
pub struct Driver {
    /// Closes the session when the driver goes, run or not.
    ending: Ending,
    commands: mpsc::UnboundedReceiver<Command>,
    accept_quit: bool,
}

impl Session {
    /// A session and its driver. With `accept_quit`, a frontend's
    /// [`ClientMessage::Quit`] ends the session (a terminal UI); without, it
    /// is ignored (a server with browser frontends coming and going).
    pub fn new(accept_quit: bool) -> (Session, Driver) {
        let (tx, rx) = mpsc::unbounded_channel();
        let session = Session {
            inner: Arc::new(Inner {
                clients: Mutex::default(),
                next_client: AtomicU64::new(1),
                ui: Mutex::default(),
                ui_plugins: Mutex::default(),
                plugin_versions: AtomicU64::new(0),
                running: AtomicBool::new(false),
                closed: AtomicBool::new(false),
                history: Mutex::default(),
                id: uuid::Uuid::new_v4().to_string(),
                generation: AtomicU64::new(0),
                started: AtomicBool::new(false),
                wakes_left: AtomicU32::new(0),
                wakes_set: AtomicBool::new(false),
            }),
            commands: tx,
        };
        let driver = Driver {
            ending: Ending {
                inner: session.inner.clone(),
                open_run: None,
            },
            commands: rx,
            accept_quit,
        };
        (session, driver)
    }

    /// Attach a frontend. Its first message is [`ServerMessage::Hello`], then
    /// the session's recent runs as they were broadcast (up to [`HISTORY`]
    /// messages, oldest first: a page opened late shows what happened), then
    /// [`ServerMessage::Closed`] at once if the session already ended.
    pub fn connect(&self) -> Connection {
        self.attach(false)
    }

    /// Watch the session as a frontend would — the same messages — without
    /// being one: a question asked while only observers are attached gets
    /// its default answer at once, as with nobody there (a host waiting for
    /// a run to end, a log). A question held for later ([`Session::ask_later`])
    /// stays open, as it does with nobody there.
    pub fn observe(&self) -> Connection {
        self.attach(true)
    }

    fn attach(&self, observer: bool) -> Connection {
        let id = self.inner.next_client.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel();
        // Under the clients lock, every later broadcast reaches this frontend
        // after its Hello; one already reflected in Hello (`running`, an open
        // question) may still arrive after it, and frontends tolerate that.
        let mut clients = self.inner.clients.lock().unwrap();
        let mut ui_requests: Vec<PendingUiRequest> = {
            let ui = self.inner.ui.lock().unwrap();
            ui.pending
                .iter()
                .map(|(id, (_, request))| PendingUiRequest {
                    id: *id,
                    request: request.clone(),
                })
                .collect()
        };
        ui_requests.sort_by_key(|r| r.id);
        let _ = tx.send(ServerMessage::Hello {
            running: self.inner.running.load(Ordering::SeqCst),
            ui_plugins: self.ui_plugins(),
            ui_requests,
        });
        for message in self.inner.history.lock().unwrap().iter() {
            let _ = tx.send(message.clone());
        }
        if self.inner.closed.load(Ordering::SeqCst) {
            let _ = tx.send(ServerMessage::Closed);
        }
        clients.insert(id, Client { tx, observer });
        Connection { id, messages: rx }
    }

    /// Detach a frontend.
    pub fn disconnect(&self, id: u64) {
        self.inner.clients.lock().unwrap().remove(&id);
    }

    /// Number of attached frontends (observers not counted).
    pub fn frontends(&self) -> usize {
        let clients = self.inner.clients.lock().unwrap();
        clients.values().filter(|c| !c.observer).count()
    }

    /// Whether a run is in progress (as the driver sees it).
    pub fn is_running(&self) -> bool {
        self.inner.running.load(Ordering::SeqCst)
    }

    /// The session's id (a uuid, made once).
    pub fn id(&self) -> &str {
        &self.inner.id
    }

    /// How many times the conversation was reset (0 before the first Reset).
    pub fn generation(&self) -> u64 {
        self.inner.generation.load(Ordering::SeqCst)
    }

    /// Whether a run has started in this session (so a plugin's wake may
    /// start one: never before the owner's own).
    pub fn has_started(&self) -> bool {
        self.inner.started.load(Ordering::SeqCst)
    }

    /// Sets how many runs plugins' wakes may start in this session
    /// ([`Session::wake`]): once — a later call changes nothing and returns
    /// `false` (the cap is the session's, not refilled). None until set.
    pub fn allow_wakes(&self, runs: u32) -> bool {
        if self.inner.wakes_set.swap(true, Ordering::SeqCst) {
            tracing::warn!("the session's wake allowance was set already: kept");
            return false;
        }
        self.inner.wakes_left.store(runs, Ordering::SeqCst);
        true
    }

    /// The runs plugins' wakes may still start.
    pub fn wakes_left(&self) -> u32 {
        self.inner.wakes_left.load(Ordering::SeqCst)
    }

    /// A plugin's words for the agent (`text` already tagged as the
    /// plugin's). The driver decides what they become: steering, read by a
    /// run going on; else a run of their own — only once a run has started
    /// (the owner's first), and counted against [`Session::allow_wakes`]
    /// whether the agent was idle or the run ended before reading them
    /// (unless the owner's own late steering starts that run anyway, which
    /// they join). One that cannot be delivered is said as a Warning notice,
    /// as is one a run stopped (aborted, reset, or failed) before reading.
    /// `false` once the session ended.
    pub fn wake(&self, text: String) -> bool {
        !self.is_closed() && self.commands.send(Command::Wake { text }).is_ok()
    }

    /// Whether the session ended: the driver stopped, nothing sent is heard.
    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }

    /// A message from a frontend. Returns `false` when the session ended (the
    /// message is not heard) — a frontend should tell its user.
    ///
    /// An answer that does not fit its question ([`UiRequest::accepts`]) is
    /// ignored: the question stays open for another answer (or its timeout).
    pub fn send(&self, message: ClientMessage) -> bool {
        if self.is_closed() {
            tracing::warn!("a frontend message after the session ended was dropped");
            return false;
        }
        match message {
            ClientMessage::UiResponse { id, value } => {
                let mut ui = self.inner.ui.lock().unwrap();
                match ui.pending.get(&id) {
                    Some((_, request)) if !request.accepts(&value) => {
                        tracing::warn!(id, answer = %value, "an answer that does not fit its question was ignored");
                    }
                    Some(_) => {
                        if let Some((waiter, _)) = ui.pending.remove(&id) {
                            let _ = waiter.send(value);
                        }
                    }
                    None => tracing::debug!(id, "an answer to a question already closed"),
                }
                true
            }
            other => self.commands.send(Command::Client(other)).is_ok(),
        }
    }

    /// Tell every frontend something outside a run's events.
    pub fn notice(&self, level: NoticeLevel, message: impl Into<String>) {
        self.broadcast(ServerMessage::Notice {
            level,
            message: message.into(),
        });
    }

    /// Send one frontend a message (a reply to something only it sent).
    pub(crate) fn send_to(&self, id: u64, message: ServerMessage) {
        if let Some(Client { tx, .. }) = self.inner.clients.lock().unwrap().get(&id) {
            let _ = tx.send(message);
        }
    }

    /// Ask the user through whichever frontend answers first. With no
    /// frontend attached, or no fitting answer within `timeout`, the safe
    /// default ([`UiRequest::default_answer`]). A `notify` is shown and
    /// returns at once.
    ///
    /// Cancel-safe: dropping the future (the plugin's call was cancelled)
    /// withdraws the question, and frontends close its dialog.
    pub async fn ask(&self, request: UiRequest, timeout: Duration) -> Json {
        self.ask_keyed(request, timeout, None).await
    }

    /// [`Session::ask`], the question findable by `key` for
    /// [`Session::withdraw`] — for an asker that cannot drop this future
    /// when it gives up (a plugin in another process). Keys must be unique
    /// (a UUID); a key already in use is ignored.
    pub async fn ask_keyed(
        &self,
        request: UiRequest,
        timeout: Duration,
        key: Option<String>,
    ) -> Json {
        let default = request.default_answer();
        self.ask_inner(request, Some(timeout), key, false)
            .await
            .unwrap_or(default)
    }

    /// Ask, and keep the question open until it is answered — however long
    /// that takes, and with no frontend attached: one that connects later
    /// gets it in its `Hello`. For an agent working while its owner is away
    /// (see the handoff agent, `examples/handoff.toml`). `Some(answer)` when a frontend
    /// answered (`null` when the user dismissed it); `None` when nobody
    /// will: the question was withdrawn — a reset withdraws these, since
    /// they belong to the forgotten conversation — or the session ended. A
    /// `notify` is shown and returns `Some(null)` at once.
    pub async fn ask_later(&self, request: UiRequest) -> Option<Json> {
        self.ask_inner(request, None, None, true).await
    }

    async fn ask_inner(
        &self,
        request: UiRequest,
        timeout: Option<Duration>,
        key: Option<String>,
        hold: bool,
    ) -> Option<Json> {
        let request = request.normalized();
        let expects = request.expects_answer();
        let (tx, rx) = oneshot::channel();
        // Before the `ui` lock: `connect` takes `clients` then `ui`, so
        // taking them the other way round could deadlock.
        let attached = self.frontends() > 0;
        let (id, key) = {
            let mut ui = self.inner.ui.lock().unwrap();
            ui.early.retain(|_, at| at.elapsed() < EARLY_WITHDRAWAL);
            if let Some(key) = &key {
                if ui.early.remove(key).is_some() {
                    tracing::debug!(%key, "a question withdrawn before it was asked");
                    return None;
                }
            }
            // Checked under `ui`: the session's end clears `pending` under it
            // too, so a question is either refused here or cleared there.
            if self.is_closed() {
                return None;
            }
            if !attached && !hold {
                drop(ui);
                tracing::info!(
                    title = request.title(),
                    "no frontend to ask: the safe default"
                );
                return None;
            }
            ui.next_id += 1;
            let id = ui.next_id;
            let mut owned_key = None;
            if expects {
                ui.pending.insert(id, (tx, request.clone()));
                if hold {
                    ui.held.insert(id);
                }
                if let Some(key) = key {
                    if ui.keys.contains_key(&key) {
                        tracing::warn!(%key, "a question key already in use: this one cannot be withdrawn by key");
                    } else {
                        ui.keys.insert(key.clone(), id);
                        owned_key = Some(key);
                    }
                }
            }
            (id, owned_key)
        };
        let title = request.title().to_owned();
        self.broadcast(ServerMessage::UiRequest { id, request });
        if !expects {
            return Some(Json::Null);
        }
        /// Closes the question however `ask` ends — answered, timed out,
        /// withdrawn by key, or dropped — and tells the frontends why.
        struct Close<'a> {
            session: &'a Session,
            id: u64,
            key: Option<String>,
            reason: ResolveReason,
        }
        impl Drop for Close<'_> {
            fn drop(&mut self) {
                {
                    let mut ui = self.session.inner.ui.lock().unwrap();
                    ui.pending.remove(&self.id);
                    ui.held.remove(&self.id);
                    if let Some(key) = &self.key {
                        if ui.keys.get(key) == Some(&self.id) {
                            ui.keys.remove(key);
                        }
                    }
                }
                self.session.broadcast(ServerMessage::UiResolved {
                    id: self.id,
                    reason: self.reason,
                });
            }
        }
        let mut close = Close {
            session: self,
            id,
            key,
            reason: ResolveReason::Withdrawn,
        };
        let answered = match timeout {
            Some(limit) => tokio::time::timeout(limit, rx).await,
            None => Ok(rx.await),
        };
        match answered {
            Ok(Ok(value)) => {
                close.reason = ResolveReason::Answered;
                Some(value)
            }
            Ok(Err(_)) => None,
            Err(_) => {
                close.reason = ResolveReason::TimedOut;
                tracing::info!(%title, "no answer in time: the safe default");
                None
            }
        }
    }

    /// Withdraw the open question asked with `key`: its asker gets the safe
    /// default, frontends close it. A key not asked yet is remembered for a
    /// minute, so a withdrawal that overtakes its question still counts.
    pub fn withdraw(&self, key: &str) {
        let mut ui = self.inner.ui.lock().unwrap();
        ui.early.retain(|_, at| at.elapsed() < EARLY_WITHDRAWAL);
        match ui.keys.remove(key) {
            // Dropping the waiter ends `ask` with the default; its guard then
            // tells the frontends.
            Some(id) => {
                ui.pending.remove(&id);
            }
            None => {
                ui.early.insert(key.to_owned(), Instant::now());
            }
        }
    }

    /// Offer a browser component, returning the version it got; withdrawn
    /// with [`Session::remove_ui_plugin`]. Offering one under a name already
    /// taken replaces it. A plugin without a name is refused (`None`).
    pub fn add_ui_plugin(&self, mut plugin: UiPlugin, module: impl Into<Arc<str>>) -> Option<u64> {
        if plugin.name.trim().is_empty() {
            tracing::warn!("a UI plugin without a name was refused");
            return None;
        }
        let version = self.inner.plugin_versions.fetch_add(1, Ordering::Relaxed) + 1;
        plugin.version = version;
        {
            let mut plugins = self.inner.ui_plugins.lock().unwrap();
            plugins.retain(|(p, _)| p.name != plugin.name);
            plugins.push((plugin, module.into()));
        }
        self.broadcast(ServerMessage::UiPlugins {
            ui_plugins: self.ui_plugins(),
        });
        Some(version)
    }

    /// Withdraw a browser component. With `version`, only that offer: a
    /// plugin's own disposer must not remove an offer that replaced it.
    pub fn remove_ui_plugin(&self, name: &str, version: Option<u64>) {
        let removed = {
            let mut plugins = self.inner.ui_plugins.lock().unwrap();
            let before = plugins.len();
            plugins.retain(|(p, _)| p.name != name || version.is_some_and(|v| v != p.version));
            plugins.len() != before
        };
        if removed {
            self.broadcast(ServerMessage::UiPlugins {
                ui_plugins: self.ui_plugins(),
            });
        }
    }

    pub fn ui_plugins(&self) -> Vec<UiPlugin> {
        let plugins = self.inner.ui_plugins.lock().unwrap();
        plugins.iter().map(|(p, _)| p.clone()).collect()
    }

    /// A component's module source, to serve to a browser.
    pub fn ui_plugin_module(&self, name: &str) -> Option<Arc<str>> {
        let plugins = self.inner.ui_plugins.lock().unwrap();
        plugins
            .iter()
            .find(|(p, _)| p.name == name)
            .map(|(_, m)| m.clone())
    }

    fn broadcast(&self, message: ServerMessage) {
        self.inner.broadcast(message);
    }
}

impl Inner {
    /// Forget the conversation (a Reset): a new generation first, so a host
    /// asking meanwhile never pairs the old generation with a cleared state.
    fn reset(&self, agent: &mut Agent) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        agent.clear_messages();
        self.withdraw_held();
    }

    /// Take one of the runs plugins' wakes may start, if any is left.
    fn take_wake(&self) -> bool {
        // A compare-exchange loop (`fetch_update` is deprecated on newer
        // toolchains, `try_update` absent on older ones).
        let mut left = self.wakes_left.load(Ordering::SeqCst);
        while left > 0 {
            match self.wakes_left.compare_exchange_weak(
                left,
                left - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return true,
                Err(now) => left = now,
            }
        }
        false
    }

    /// Say a plugin's wake that reached no run.
    fn wake_dropped(&self, text: &str, why: &str) {
        tracing::warn!("a plugin's wake was not delivered ({why}): {text}");
        self.broadcast(ServerMessage::Notice {
            level: NoticeLevel::Warning,
            message: format!(
                "A plugin's wake was not delivered ({why}): {}",
                first_line(text, 120)
            ),
        });
    }

    /// Withdraw every question asked with [`Session::ask_later`] (a reset).
    fn withdraw_held(&self) {
        let mut ui = self.ui.lock().unwrap();
        let held: Vec<u64> = ui.held.drain().collect();
        for id in held {
            // Dropping the waiter ends its `ask_later` with `None`; its guard
            // tells the frontends.
            ui.pending.remove(&id);
        }
    }

    fn broadcast(&self, message: ServerMessage) {
        let mut clients = self.clients.lock().unwrap();
        // Kept for later frontends: what a run showed. Questions are not
        // (Hello carries the open ones), nor the session's own messages.
        if matches!(
            message,
            ServerMessage::RunStart { .. }
                | ServerMessage::Event { .. }
                | ServerMessage::RunEnd { .. }
                | ServerMessage::Notice { .. }
        ) {
            let mut history = self.history.lock().unwrap();
            if history.len() == HISTORY {
                history.pop_front();
            }
            history.push_back(message.clone());
        }
        clients.retain(|_, c| c.tx.send(message.clone()).is_ok());
    }
}

/// However the driver goes — [`Driver::run`] returning, its task failing, or
/// a driver never run — a run in progress gets its `RunEnd` and every
/// frontend `Closed`.
struct Ending {
    inner: Arc<Inner>,
    open_run: Option<u64>,
}

impl Drop for Ending {
    fn drop(&mut self) {
        if let Some(run) = self.open_run.take() {
            self.inner.running.store(false, Ordering::SeqCst);
            self.inner.broadcast(ServerMessage::RunEnd {
                run,
                outcome: RunOutcome::Error,
                error: Some("the session stopped during the run".into()),
                stats: Box::default(),
                total_cost_usd: None,
            });
        }
        // Under the clients lock: a frontend connecting now gets `Closed`
        // either from `connect` or from this broadcast, never both.
        let mut clients = self.inner.clients.lock().unwrap();
        self.inner.closed.store(true, Ordering::SeqCst);
        clients.retain(|_, c| c.tx.send(ServerMessage::Closed).is_ok());
        // Open questions end with their default (a question held for an
        // absent owner would otherwise wait forever). `clients` then `ui`.
        self.inner.ui.lock().unwrap().pending.clear();
    }
}

impl Driver {
    /// Run the agent until a frontend quits (with `accept_quit`) or every
    /// [`Session`] handle is gone. A prompt sent during a run is queued and
    /// runs next; steering and follow-ups go to the run in progress (or start
    /// one when idle); a plugin's wake as [`Session::wake`] says. Returns the
    /// agent with its history.
    pub async fn run(mut self, mut agent: Agent) -> Agent {
        let inner = self.ending.inner.clone();
        let mut queue: VecDeque<String> = VecDeque::new();
        let mut run = 0u64;
        'session: loop {
            let prompt = match queue.pop_front() {
                Some(text) => text,
                None => {
                    match self.commands.recv().await {
                        Some(Command::Client(
                            ClientMessage::Prompt { text }
                            | ClientMessage::Steer { text }
                            | ClientMessage::FollowUp { text },
                        )) => text,
                        Some(Command::Wake { text }) => {
                            if !inner.started.load(Ordering::SeqCst) {
                                inner.wake_dropped(&text, "the owner's task has not started");
                                continue;
                            }
                            if !inner.take_wake() {
                                inner.wake_dropped(&text, "the agent is idle and plugins may start no more runs this session");
                                continue;
                            }
                            text
                        }
                        Some(Command::Client(ClientMessage::Reset)) => {
                            inner.reset(&mut agent);
                            continue;
                        }
                        Some(Command::Client(ClientMessage::Quit)) if self.accept_quit => break,
                        Some(_) => continue,
                        None => break,
                    }
                }
            };
            run += 1;
            inner.started.store(true, Ordering::SeqCst);
            inner.running.store(true, Ordering::SeqCst);
            self.ending.open_run = Some(run);
            inner.broadcast(ServerMessage::RunStart {
                run,
                prompt: prompt.clone(),
            });
            // Where this run's messages begin (what it read is told from them).
            let before = agent.messages().len();
            let mut events = agent.prompt(prompt.clone()).await;
            let mut text = Coalesced::default();
            let mut end: Option<(SessionStats, Option<(RunOutcome, String)>)> = None;
            let mut rejected = None;
            let mut aborted = false;
            let mut reset = false;
            let mut quit = false;
            // The wakes this run was given as steering, to tell them from
            // the owner's among what it never read.
            let mut woke: Vec<String> = Vec::new();
            // The owner's steering this run: a task that fails loses what
            // its loop took (yoagent keeps the history only of a loop that
            // returns), and that is steered again.
            let mut steered: Vec<String> = Vec::new();
            let mut tick = tokio::time::interval(COALESCE);
            loop {
                tokio::select! {
                    event = events.recv() => {
                        let Some(event) = event else { break };
                        match &event {
                            AgentEvent::AgentEnd { messages, stats, .. } => {
                                end = Some((
                                    stats.clone(),
                                    stopped(messages)
                                        .map(|reason| (RunOutcome::Stopped, reason))
                                        .or_else(|| run_error(messages)),
                                ));
                            }
                            AgentEvent::InputRejected { reason } => {
                                rejected = Some(format!("input rejected: {reason}"));
                            }
                            _ => {}
                        }
                        for event in text.push(event) {
                            inner.broadcast(ServerMessage::Event { run, event: Box::new(event) });
                        }
                    }
                    _ = tick.tick() => {
                        if let Some(event) = text.flush() {
                            inner.broadcast(ServerMessage::Event { run, event: Box::new(event) });
                        }
                    }
                    Some(command) = self.commands.recv() => match command {
                        Command::Wake { text } => {
                            woke.push(text.clone());
                            agent.steer(user(text));
                        }
                        Command::Client(command) => match command {
                        ClientMessage::Prompt { text } => queue.push_back(text),
                        ClientMessage::Steer { text } => {
                            steered.push(text.clone());
                            agent.steer(user(text));
                        }
                        ClientMessage::FollowUp { text } => agent.follow_up(user(text)),
                        ClientMessage::Abort => {
                            agent.abort();
                            aborted = true;
                        }
                        // Start over: stop this run, drop what was queued,
                        // and forget the conversation once the run ends.
                        ClientMessage::Reset => {
                            queue.clear();
                            agent.abort();
                            aborted = true;
                            reset = true;
                        }
                        ClientMessage::Quit if self.accept_quit => {
                            agent.abort();
                            aborted = true;
                            quit = true;
                        }
                        _ => {}
                        },
                    },
                }
            }
            if let Some(event) = text.flush() {
                inner.broadcast(ServerMessage::Event {
                    run,
                    event: Box::new(event),
                });
            }
            agent.finish().await;
            // A run whose task failed (no `AgentEnd`) is one that stopped:
            // what it never read is told the same way.
            if reset || aborted || end.is_none() {
                // What is still queued was never read. What a stopping loop
                // took is kept in the history (yoagent 0.25.10), where the
                // next run reads it — except before a reset, which wipes the
                // history, and by a task that failed, which loses it: both
                // below. A plugin's wake still queued is dropped, said; the
                // owner's steering stays for the next run (a reset clears
                // it). Counted as multisets: one text may come more than once.
                let mut unread = woke;
                let mut queued_owner = Vec::new();
                for message in agent.take_steering_queue() {
                    let text = text_of(&message);
                    let wake = text
                        .as_ref()
                        .and_then(|t| unread.iter().position(|w| w == t));
                    if let Some(i) = wake {
                        let text = unread.swap_remove(i);
                        inner.wake_dropped(&text, "the run was stopped before reading it");
                        continue;
                    }
                    if let Some(i) = text
                        .as_ref()
                        .and_then(|t| steered.iter().position(|o| o == t))
                    {
                        steered.remove(i);
                    }
                    queued_owner.push(message);
                }
                // Taken by the loop: read if among this run's messages (the
                // prompt aside) — before a reset, only if the model answered
                // after it. A failed task's are not there. Compaction may
                // have shortened the history: then all of it is looked at.
                let messages = agent.messages();
                let from = if before <= messages.len() { before } else { 0 };
                let mut seen = &messages[from..];
                if reset {
                    let answered = seen
                        .iter()
                        .rposition(|m| matches!(m, AgentMessage::Llm(Message::Assistant { .. })))
                        .unwrap_or(0);
                    seen = &seen[..answered];
                }
                let mut read: Vec<String> = seen.iter().filter_map(text_of).collect();
                let mut was_read = |text: &str| match read.iter().position(|r| r == text) {
                    Some(i) => {
                        read.swap_remove(i);
                        true
                    }
                    None => false,
                };
                was_read(&prompt);
                for text in unread {
                    if !was_read(&text) {
                        inner.wake_dropped(&text, "the run was stopped before reading it");
                    }
                }
                if !reset {
                    // The owner's that a failed task lost, first (taken
                    // before what is still queued).
                    for text in steered {
                        if !was_read(&text) {
                            agent.steer(user(text));
                        }
                    }
                    for message in queued_owner {
                        agent.steer(message);
                    }
                }
                if reset {
                    inner.reset(&mut agent);
                    agent.clear_follow_up_queue();
                }
            } else {
                // Steering or a follow-up that arrived after the loop's last
                // look (while the answer streamed, or extensions finished):
                // the next run's prompt, not left for some later run. A
                // plugin's wake among it starts that run only as an idle one
                // would: counted, or dropped.
                let mut late: Vec<String> = Vec::new();
                let mut wakes: Vec<String> = Vec::new();
                for text in agent
                    .take_steering_queue()
                    .into_iter()
                    .chain(agent.take_follow_up_queue())
                    .filter_map(|m| text_of(&m))
                {
                    match woke.iter().position(|w| *w == text) {
                        Some(i) => {
                            woke.swap_remove(i);
                            wakes.push(text);
                        }
                        None => late.push(text),
                    }
                }
                if !wakes.is_empty() {
                    if late.is_empty() && !inner.take_wake() {
                        for text in &wakes {
                            inner.wake_dropped(text, "the run ended before reading it, and plugins may start no more runs this session");
                        }
                    } else {
                        late.extend(wakes);
                    }
                }
                if !late.is_empty() {
                    queue.push_front(late.join("\n\n"));
                }
            }
            let saw_end = end.is_some();
            let (stats, last) = end.unwrap_or_default();
            let (outcome, error) = match (rejected, saw_end, last) {
                (Some(reason), _, _) => (RunOutcome::Rejected, Some(reason)),
                (None, false, _) => {
                    // The agent's task failed: the events stopped without an
                    // AgentEnd (yoagent logs the panic).
                    inner.broadcast(ServerMessage::Notice {
                        level: NoticeLevel::Error,
                        message: "The agent's run failed unexpectedly (see the host's logs); later runs may be missing tools.".into(),
                    });
                    (RunOutcome::Error, Some(TASK_FAILED.into()))
                }
                (None, true, _) if aborted => (RunOutcome::Aborted, Some("aborted".into())),
                (None, true, Some((outcome, message))) => (outcome, Some(message)),
                (None, true, None) => (RunOutcome::Completed, None),
            };
            let total_cost_usd = stats.total_cost_usd();
            inner.running.store(false, Ordering::SeqCst);
            self.ending.open_run = None;
            inner.broadcast(ServerMessage::RunEnd {
                run,
                outcome,
                error,
                stats: Box::new(stats),
                total_cost_usd,
            });
            if quit {
                break 'session;
            }
        }
        // Wakes still in the channel are not delivered: the session is
        // ending, so a notice would reach nobody; the log says so.
        while let Ok(command) = self.commands.try_recv() {
            if let Command::Wake { text } = command {
                tracing::warn!("a plugin's wake was not delivered (the session ended): {text}");
            }
        }
        drop(self);
        agent
    }
}

fn user(text: String) -> AgentMessage {
    AgentMessage::Llm(Message::user(text))
}

/// A text's first line, at most `max` characters, `…` when cut.
fn first_line(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or("");
    match line.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &line[..at]),
        None if line.len() < text.trim_end().len() => format!("{line}…"),
        None => line.to_owned(),
    }
}

/// A queued user message's text (the session only queues text).
fn text_of(message: &AgentMessage) -> Option<String> {
    let AgentMessage::Llm(Message::User { content, .. }) = message else {
        return None;
    };
    let text: Vec<&str> = content
        .iter()
        .filter_map(|c| match c {
            yoagent::Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    (!text.is_empty()).then(|| text.join("\n"))
}

/// How the run ended, from its last assistant message only (an earlier
/// message's outcome says nothing about how the run ended).
fn run_error(messages: &[AgentMessage]) -> Option<(RunOutcome, String)> {
    let last = messages.iter().rev().find_map(|m| match m {
        AgentMessage::Llm(Message::Assistant {
            stop_reason,
            error_message,
            ..
        }) => Some((stop_reason, error_message)),
        _ => None,
    })?;
    match last {
        (StopReason::Error, message) => Some((
            RunOutcome::Error,
            message.clone().unwrap_or_else(|| "error".into()),
        )),
        (StopReason::Aborted, _) => Some((RunOutcome::Aborted, "aborted".into())),
        _ => None,
    }
}

/// The reason a run was stopped before it finished — a limit, loop
/// detection, an extension's `Stop` — from the stop marker yoagent ends it
/// with (its last message).
fn stopped(messages: &[AgentMessage]) -> Option<String> {
    let AgentMessage::Llm(Message::User { content, .. }) = messages.last()? else {
        return None;
    };
    match content.first()? {
        yoagent::Content::Text { text } if text.starts_with(AGENT_STOPPED_PREFIX) => {
            Some(text.clone())
        }
        _ => None,
    }
}

/// Merges consecutive text (and thinking) deltas into one `MessageUpdate`.
#[derive(Default)]
struct Coalesced {
    held: Option<AgentEvent>,
}

impl Coalesced {
    /// The events to send now, in order.
    fn push(&mut self, event: AgentEvent) -> Vec<AgentEvent> {
        if let (
            Some(AgentEvent::MessageUpdate {
                delta: held_delta,
                message: held_message,
            }),
            AgentEvent::MessageUpdate { delta, message },
        ) = (&mut self.held, &event)
        {
            let merged = match (held_delta, delta) {
                (StreamDelta::Text { delta: a }, StreamDelta::Text { delta: b })
                | (StreamDelta::Thinking { delta: a }, StreamDelta::Thinking { delta: b }) => {
                    a.push_str(b);
                    true
                }
                _ => false,
            };
            if merged {
                *held_message = message.clone();
                return Vec::new();
            }
        }
        let mut out: Vec<AgentEvent> = self.held.take().into_iter().collect();
        if matches!(
            event,
            AgentEvent::MessageUpdate {
                delta: StreamDelta::Text { .. } | StreamDelta::Thinking { .. },
                ..
            }
        ) {
            self.held = Some(event);
        } else {
            out.push(event);
        }
        out
    }

    fn flush(&mut self) -> Option<AgentEvent> {
        self.held.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(delta: &str) -> AgentEvent {
        AgentEvent::MessageUpdate {
            message: AgentMessage::Llm(Message::user("")),
            delta: StreamDelta::Text {
                delta: delta.into(),
            },
        }
    }

    fn delta_of(event: &AgentEvent) -> &str {
        match event {
            AgentEvent::MessageUpdate {
                delta: StreamDelta::Text { delta },
                ..
            } => delta,
            _ => panic!("a text update"),
        }
    }

    #[test]
    fn consecutive_text_deltas_merge_and_other_events_flush_them_first() {
        let mut c = Coalesced::default();
        assert!(c.push(text("Hel")).is_empty());
        assert!(c.push(text("lo")).is_empty());
        let out = c.push(AgentEvent::TurnStart);
        assert_eq!(out.len(), 2, "the merged text, then the event, in order");
        assert_eq!(delta_of(&out[0]), "Hello");
        assert!(matches!(out[1], AgentEvent::TurnStart));
        assert!(c.flush().is_none());
    }

    #[test]
    fn thinking_merges_apart_from_text() {
        let thinking = |d: &str| AgentEvent::MessageUpdate {
            message: AgentMessage::Llm(Message::user("")),
            delta: StreamDelta::Thinking { delta: d.into() },
        };
        let mut c = Coalesced::default();
        assert!(c.push(thinking("hm")).is_empty());
        assert!(c.push(thinking("m")).is_empty());
        let out = c.push(text("A"));
        assert_eq!(out.len(), 1, "the merged thinking; the text is held");
        assert!(matches!(
            &out[0],
            AgentEvent::MessageUpdate { delta: StreamDelta::Thinking { delta }, .. } if delta == "hmm"
        ));
        assert_eq!(delta_of(&c.flush().unwrap()), "A");
    }

    #[test]
    fn a_held_delta_is_flushed_on_the_tick() {
        let mut c = Coalesced::default();
        c.push(text("a"));
        assert_eq!(delta_of(&c.flush().unwrap()), "a");
        assert!(c.flush().is_none());
    }
}
