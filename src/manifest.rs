//! The agent manifest: one TOML file per agent (design §2). Read strictly —
//! an unknown key, an unpinned plugin, a budget for an unpriced model, a
//! secret written into the file are errors, each naming the file, line and
//! column (a file that cannot be read names the file) — into typed parts
//! `yobuild run` wires up.
//!
//! ```toml
//! [agent]
//! name  = "handoff"
//! model = "deepseek/deepseek-flash"
//!
//! [[plugin]]
//! pi = "@juicesharp/rpiv-todo@2.12.0"
//!
//! [builtins]
//! report = "handoff"
//! ```
//!
//! Paths in the manifest are relative to its folder. Reading a manifest has
//! no effect on the process: the budget's price is looked up in yoagent's
//! bundled table directly.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::Range;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use toml::Spanned;
use yoagent::provider::model::OpenAiCompat;
use yoagent::provider::{ModelConfig, PriceTable};

use crate::builtins::Report;

/// A manifest, read and checked.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Manifest {
    /// The manifest's file as given (perhaps relative): for its name, which
    /// names its lock (`agent.toml` → `agent.lock` in `dir`).
    pub path: PathBuf,
    /// The manifest's folder (absolute when read by [`Manifest::load`]):
    /// what its paths are relative to.
    pub dir: PathBuf,
    pub agent: Agent,
    pub limits: Limits,
    pub plugins: Vec<Plugin>,
    pub mcp: Vec<Mcp>,
    pub builtins: Builtins,
    /// `[extend]`: the agent may find plugins (design §14); `None` when
    /// the manifest has no such table.
    pub extend: Option<Extend>,
    pub dsh: Dsh,
    pub frontend: Frontend,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Agent {
    /// One plain path component (it names the agent's scratch folder).
    pub name: String,
    pub model: Model,
    /// The system prompt's file; `yobuild run` fills in `{cwd}` and
    /// `{scratch}`.
    pub prompt: Option<PathBuf>,
}

/// `provider/id`: a provider yoagent has a constructor for, or an
/// OpenAI-compatible endpoint (`base_url`, `api_key_env`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    provider: String,
    id: String,
    base_url: Option<String>,
    key_env: Vec<String>,
}

/// Providers whose generic constructor carries their models' own limits
/// (their models share one context window and output limit): no preset is
/// needed there.
const OWN_LIMITS: [&str; 2] = ["deepseek", "meta"];

/// The providers yoagent has a constructor for, and the variables its key
/// is read from, first set first (yoagent 0.25's `resolve_api_key`).
pub const PROVIDERS: [(&str, &[&str]); 11] = [
    ("anthropic", &["ANTHROPIC_API_KEY"]),
    ("openai", &["OPENAI_API_KEY"]),
    ("deepseek", &["DEEPSEEK_API_KEY"]),
    ("google", &["GEMINI_API_KEY", "GOOGLE_API_KEY"]),
    ("groq", &["GROQ_API_KEY"]),
    ("mistral", &["MISTRAL_API_KEY"]),
    ("xai", &["XAI_API_KEY"]),
    ("qwen", &["DASHSCOPE_API_KEY"]),
    ("zai", &["ZAI_API_KEY"]),
    ("minimax", &["MINIMAX_API_KEY"]),
    ("meta", &["META_API_KEY", "MODEL_API_KEY"]),
];

/// Every variable yoagent reads a credential from, for any provider or
/// decision model it knows: yoagent 0.25.12's `api_key_variables` as it is
/// (unit-tested), which `resolve_api_key` reads — [`PROVIDERS`]' and those
/// of the providers yobuild does not offer, Bedrock's (AWS's own
/// credentials among them), its fallbacks `YOAGENT_API_KEY` and `API_KEY`,
/// TypeSafe's and Cloudflare's. Plugins and stdio MCP servers have those
/// set here taken out of their environment unless their entry hands one
/// over (design §12, "Step 4").
pub const KEY_VARIABLES: [&str; 26] = [
    "ANTHROPIC_API_KEY", "OPENAI_API_KEY", "DEEPSEEK_API_KEY", "GEMINI_API_KEY", "GOOGLE_API_KEY",
    "GROQ_API_KEY", "MISTRAL_API_KEY", "XAI_API_KEY", "DASHSCOPE_API_KEY", "ZAI_API_KEY",
    "MINIMAX_API_KEY", "META_API_KEY", "MODEL_API_KEY", "OPENROUTER_API_KEY", "CEREBRAS_API_KEY",
    "OPENCODE_API_KEY", "AZURE_OPENAI_API_KEY", "AWS_BEARER_TOKEN_BEDROCK", "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY", "AWS_SESSION_TOKEN", "YOAGENT_API_KEY", "API_KEY", "TYPESAFE_API_KEY",
    "CLOUDFLARE_API_TOKEN", "CLOUDFLARE_AUTH_TOKEN",
];

/// Local servers yoagent has a constructor for, which take no key: `ollama`
/// (`base_url` defaults to [`OLLAMA`]) and `local` (any OpenAI-compatible
/// server on this machine: LM Studio, llama.cpp, …; `base_url` required).
pub const LOCAL_PROVIDERS: [&str; 2] = ["ollama", "local"];

/// Ollama's OpenAI-compatible endpoint, `ollama`'s default `base_url`.
pub const OLLAMA: &str = "http://localhost:11434/v1";

impl Model {
    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// The OpenAI-compatible endpoint of a provider yoagent does not know.
    pub fn base_url(&self) -> Option<&str> {
        self.base_url.as_deref()
    }

    /// The variables the API key is read from, first set first (none for a
    /// local provider).
    pub fn key_env(&self) -> &[String] {
        &self.key_env
    }

    /// Whether the provider takes a key: not a local one ([`LOCAL_PROVIDERS`]).
    pub fn needs_key(&self) -> bool {
        !self.key_env.is_empty()
    }

    /// The yoagent model configuration: yoagent's preset for this provider
    /// and id when it has one (`ModelConfig::preset`: the model's real
    /// context window, output limit and protocol), else the provider's
    /// generic constructor, whose limits are often smaller (about 128K in
    /// and 4K out for many providers; DeepSeek's and Meta's carry their
    /// models' own) — [`limits_known`](Self::limits_known) says which. It
    /// carries a price only once the process enabled yoagent's price table
    /// (`yoagent::provider::prices::enable_bundled`, done by `yobuild run`).
    pub fn config(&self) -> ModelConfig {
        ModelConfig::preset(&self.provider, &self.id).unwrap_or_else(|| self.generic())
    }

    /// Whether [`config`](Self::config)'s context window and output limit are
    /// this model's: a preset, or a provider whose generic constructor
    /// carries its models' limits (DeepSeek, Meta). Otherwise they are the
    /// provider's defaults, a guess.
    pub fn limits_known(&self) -> bool {
        OWN_LIMITS.contains(&self.provider.as_str())
            || ModelConfig::preset(&self.provider, &self.id).is_some()
    }

    /// The provider's generic constructor for this id (no preset).
    pub(crate) fn generic(&self) -> ModelConfig {
        // A provider yoagent knows: its own choice (an OpenAI reasoning
        // model on the Responses API).
        if let Some(config) = ModelConfig::generic(&self.provider, &self.id) {
            return config;
        }
        let id = self.id.clone();
        match self.provider.as_str() {
            "ollama" => ModelConfig::ollama(self.base_url.as_deref().unwrap_or(OLLAMA), id),
            "local" => ModelConfig::local(self.base_url.clone().unwrap_or_default(), id),
            provider => ModelConfig::openai_compat(
                self.base_url.clone().unwrap_or_default(),
                id,
                provider,
                OpenAiCompat::default(),
            ),
        }
    }

    /// The API key: the first of [`key_env`](Self::key_env) set and not
    /// empty. `yobuild run` must pass it (or, when the provider
    /// [needs one](Self::needs_key), refuse to start without one; for a
    /// local provider, pass an empty key): with none, yoagent would fall
    /// back to `API_KEY`, and send that to the endpoint.
    pub fn api_key(&self) -> Option<String> {
        self.api_key_with(|name| std::env::var(name).ok())
    }

    /// [`api_key`](Self::api_key), from `lookup` instead of the environment.
    pub fn api_key_with(&self, lookup: impl Fn(&str) -> Option<String>) -> Option<String> {
        self.key_env
            .iter()
            .find_map(|name| lookup(name).filter(|k| !k.is_empty()))
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct Limits {
    pub max_turns: Option<usize>,
    /// Per run.
    pub max_tokens: Option<u64>,
    pub max_minutes: Option<u64>,
    /// Checked when read: positive, and the model has a price. Per run.
    pub budget_usd: Option<f64>,
    /// The same, for all the session's runs together.
    pub session_budget_usd: Option<f64>,
}

/// Where a plugin or an MCP server comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Source {
    /// A registry package at an exact version.
    Package(Package),
    /// A local file or folder, absolute or relative to the manifest (`./`
    /// components removed).
    Path(PathBuf),
    /// One of yobuild's own plugins, `yobuild:<name>` ([`BUILTIN_PLUGINS`]):
    /// a pi plugin's, until `run`/`check` resolve it to its file
    /// ([`Manifest::with_builtins`]).
    Builtin(String),
}

/// yobuild's own plugins a manifest names as `yobuild:<name>` (design §13
/// "Built-in plugins, by name"): name, kind (`pi`/`rutis`), file under
/// yobuild's frontend folder (`Layout::frontend`).
pub const BUILTIN_PLUGINS: &[(&str, &str, &str)] = &[
    ("todo-plan", "rutis", "plugins/todo-plan.ts"),
    ("search-links", "rutis", "plugins/search-links.ts"),
    (
        "confirm-dangerous",
        "pi",
        "plugins/pi-extensions/confirm-dangerous.ts",
    ),
];

/// The built-in `name` of `kind`, or why not.
fn builtin_of(name: &str, kind: &str) -> Result<(), String> {
    let known = |k: &str| -> Vec<String> {
        BUILTIN_PLUGINS
            .iter()
            .filter(|(_, kk, _)| *kk == k)
            .map(|(n, _, _)| format!("`yobuild:{n}`"))
            .collect()
    };
    match BUILTIN_PLUGINS.iter().find(|(n, _, _)| *n == name) {
        Some((_, k, _)) if *k == kind => Ok(()),
        Some((_, k, _)) => Err(format!(
            "`yobuild:{name}` is a {k} plugin, not a {kind} one"
        )),
        None if known(kind).is_empty() => Err(format!("yobuild has no built-in {kind} plugin")),
        None => Err(format!(
            "`yobuild:{name}` is not a built-in {kind} plugin (there are: {})",
            known(kind).join(", ")
        )),
    }
}

/// An npm package (`name`, `@scope/name`) at an exact semver version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    name: String,
    version: String,
}

impl Package {
    /// A package at an exact version, checked as a manifest's are.
    pub fn new(name: &str, version: &str) -> Result<Package, String> {
        match source(&format!("{name}@{version}"), Path::new("/"))? {
            Source::Package(package) if package.name == name => Ok(package),
            _ => Err(format!("{name:?} is not an npm package name")),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    /// `name@version`, as npm reads it.
    pub fn spec(&self) -> String {
        format!("{}@{}", self.name, self.version)
    }
}

/// A plugin: its name (unique among the manifest's plugins and MCP
/// servers), what it is, and its own configuration.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Plugin {
    /// `name` in the manifest, else the package's name (without its scope)
    /// or the file's stem.
    pub name: String,
    pub kind: PluginKind,
    /// The plugin's own configuration (a table), passed as it is — its
    /// strings' `${NAME}` references filled in at start
    /// ([`config_with`](Self::config_with)).
    pub config: Option<serde_json::Map<String, serde_json::Value>>,
    /// Its tools the gate lets run without asking (`allow`), as the plugin
    /// names them.
    pub allow: Vec<String>,
    /// It is the agent's approval policy: while no plugin judging calls
    /// (`before_tool`) is up, a run's tool calls are denied rather than run
    /// unjudged (yoagent-rutis's `require_policy`, for all plugins at
    /// once). A pi or rutis plugin; refused on a DSH plugin (DSH's adapter
    /// judges no calls).
    pub policy: bool,
    /// What its host gives it beyond what every plugin has (`grants`;
    /// design §12): all off unless the manifest grants them. A pi plugin
    /// takes `trust`, `model`, `wake` and `exec` (`model` on every pi plugin
    /// or none); a DSH plugin `workspace` and `wake`, the same on every DSH
    /// plugin (one DSH host serves them all).
    pub grants: Grants,
    /// The built-in it names (`yobuild:<name>`, [`BUILTIN_PLUGINS`]), kept
    /// after [`Manifest::with_builtins`] gave it its file: `check` says it
    /// by that name.
    pub builtin: Option<String>,
}

/// What a plugin's host gives it (design §12, "The manifest: `grants`"),
/// each off unless granted. yobuild enforces a rutis plugin's by its row,
/// the DSH plugins' by the DSH host's row (`dsh-host`, for all of them), a
/// pi extension's through the pi adapter's settings (`execFor`,
/// `projectTrustedFor`, `wakeFor`, `model`) and the extension's file the
/// adapter names when it calls.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Grants {
    /// The agent's folder (`host_workspace.cwd()`).
    pub workspace: bool,
    /// The project counts as trusted (`host_workspace.trusted()`; for a pi
    /// extension, `ctx.isProjectTrusted()`): the plugin may run the
    /// project's code — an honour-system grant.
    pub trust: bool,
    /// What it may know of the model: `"info"` (provider, id, limits).
    pub model: Option<ModelGrant>,
    /// It may wake the agent (`host_session.wake`): its text reaches the
    /// agent tagged as its own, mid-run as steering, else as a run of its
    /// own (capped).
    pub wake: bool,
    /// What a pi extension's `pi.exec` may run (`exec`; off by default).
    pub exec: ExecGrant,
    /// What its runtime's environment holds beyond the kept set (`env`;
    /// design §12, "Step 5"): names, or prefixes ending in `_*`
    /// (`PI_LENS_*`), sorted — a credential yoagent reads or a name that
    /// loads code only by its full name. A runtime's environment is the
    /// whole runtime's, so the same on every plugin that shares it.
    pub env: Vec<String>,
}

/// [`Grants::exec`]: a pi extension's `pi.exec`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExecGrant {
    /// Refused (pi's adapter answers a failed command).
    #[default]
    Off,
    /// Each call's words judged by the agent's gate, the owner asked when
    /// it would ask (`host_approval`).
    Gate,
    /// Anything, as pi runs it.
    Free,
}

/// [`Grants::model`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ModelGrant {
    /// The model's provider, id and limits; no calls.
    Info,
}

impl Grants {
    /// The keys granted, as the manifest names them.
    pub fn names(&self) -> Vec<&'static str> {
        // Every field named: a grant added later is said here.
        let Grants {
            workspace,
            trust,
            model,
            wake,
            exec,
            env,
        } = self;
        [
            (*workspace, "workspace"),
            (*trust, "trust"),
            (model.is_some(), "model"),
            (*wake, "wake"),
            (*exec != ExecGrant::Off, "exec"),
            (!env.is_empty(), "env"),
        ]
        .into_iter()
        .filter_map(|(on, name)| on.then_some(name))
        .collect()
    }

    /// The grants as `check` says them: their names, `exec` with its value
    /// (`"gate"` and `"free"` are worlds apart) and `env` with its names.
    pub fn said(&self) -> Vec<String> {
        self.names()
            .into_iter()
            .map(|name| match (name, self.exec) {
                ("exec", ExecGrant::Gate) => "exec = \"gate\"".to_owned(),
                ("exec", ExecGrant::Free) => "exec = \"free\"".to_owned(),
                ("env", _) => format!("env = [{}]", self.env.join(", ")),
                (name, _) => name.to_owned(),
            })
            .collect()
    }

    /// Nothing granted.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Nothing the host serves granted: `env` aside, which only the
    /// runtime's launch reads (so a DSH plugin granted only `env` starts no
    /// DSH host).
    pub fn hosts_nothing(&self) -> bool {
        // Every field named: a grant added later is decided here.
        let Grants {
            workspace,
            trust,
            model,
            wake,
            exec,
            env: _,
        } = self;
        !workspace && !trust && model.is_none() && !wake && *exec == ExecGrant::Off
    }

    /// The `env` grant passes `name`: one of its names, or a name beginning
    /// with one of its prefixes.
    pub fn passes(&self, name: &str) -> bool {
        self.env
            .iter()
            .any(|entry| crate::frontend::host::matches(entry.as_ref(), name.as_ref()))
    }
}

impl Plugin {
    /// The configuration with its strings' references filled in by
    /// `lookup` (the environment, at start).
    pub fn config_with(
        &self,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Option<serde_json::Map<String, serde_json::Value>>, Unset> {
        let Some(config) = &self.config else {
            return Ok(None);
        };
        match fill(&serde_json::Value::Object(config.clone()), &lookup)? {
            serde_json::Value::Object(map) => Ok(Some(map)),
            _ => unreachable!("a table stays a table"),
        }
    }

    /// The names its configuration's strings refer to (`${NAME}`), at any
    /// depth, each once, in the order met.
    pub fn config_references(&self) -> Vec<String> {
        fn walk(value: &serde_json::Value, out: &mut Vec<String>) {
            match value {
                serde_json::Value::String(text) => {
                    for name in Value::new(text.clone())
                        .expect("checked when read")
                        .references()
                    {
                        if !out.iter().any(|o| o == name) {
                            out.push(name.to_owned());
                        }
                    }
                }
                serde_json::Value::Array(items) => items.iter().for_each(|v| walk(v, out)),
                serde_json::Value::Object(map) => map.values().for_each(|v| walk(v, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for value in self.config.iter().flat_map(|c| c.values()) {
            walk(value, &mut out);
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PluginKind {
    /// A pi extension package.
    Pi(Source),
    /// A DeepSeek Harness plugin.
    Dsh(Source),
    /// A rutis plugin (TypeScript, Python, Go), from a local file — or one
    /// of yobuild's own (`Plugin::builtin`; Node), its `path` empty until
    /// `run`/`check` resolve it ([`Manifest::with_builtins`]).
    Rutis { path: PathBuf, runtime: Runtime },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Runtime {
    Node,
    Bun,
    Python,
    Go,
}

/// An MCP server: its name (unique among the manifest's plugins and
/// servers), and how to reach it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Mcp {
    /// `name` in the manifest, else the package's name (without its scope),
    /// the file's stem, or the URL's host.
    pub name: String,
    pub transport: Transport,
    /// Its tools the gate lets run without asking (`allow`), as the server
    /// names them (the agent sees each as `<name>__<tool>`).
    pub allow: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Transport {
    /// A stdio server: a pinned npm package's bin (`bin`, when it has
    /// several), or a local one, run with `args` and `env`.
    Stdio {
        source: Source,
        bin: Option<String>,
        args: Vec<Value>,
        env: BTreeMap<String, Value>,
    },
    /// An HTTP server: the URL is the pin (references filled in at start).
    Http {
        url: Value,
        headers: BTreeMap<String, Value>,
    },
}

/// A header or variable's value: text with `${NAME}` references, filled in
/// from the environment when the agent starts. A value whose name looks
/// like a secret is only references (and a scheme word: `Bearer ${TOKEN}`):
/// a secret is never written into the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Value {
    text: String,
}

/// A `${NAME}` a value refers to is not set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unset(pub String);

impl fmt::Display for Unset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the environment variable {} is not set", self.0)
    }
}

impl std::error::Error for Unset {}

impl Value {
    /// `text`, its `${NAME}` references checked (closed, a plain name).
    pub fn new(text: impl Into<String>) -> Result<Self, String> {
        let text = text.into();
        pieces(&text)?;
        Ok(Self { text })
    }

    /// As written.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The names it refers to.
    pub fn references(&self) -> Vec<&str> {
        pieces(&self.text)
            .expect("checked when made")
            .into_iter()
            .filter_map(|p| match p {
                Piece::Name(n) => Some(n),
                Piece::Text(_) => None,
            })
            .collect()
    }

    /// The value with each reference filled in from the environment.
    pub fn resolve(&self) -> Result<String, Unset> {
        self.resolve_with(|name| std::env::var(name).ok())
    }

    /// [`resolve`](Self::resolve), from `lookup` instead of the environment.
    pub fn resolve_with(&self, lookup: impl Fn(&str) -> Option<String>) -> Result<String, Unset> {
        let mut out = String::new();
        for piece in pieces(&self.text).expect("checked when made") {
            match piece {
                Piece::Text(t) => out.push_str(t),
                Piece::Name(n) => out.push_str(&lookup(n).ok_or_else(|| Unset(n.to_owned()))?),
            }
        }
        Ok(out)
    }

    /// The text outside references, trimmed.
    fn literal(&self) -> String {
        pieces(&self.text)
            .expect("checked when made")
            .into_iter()
            .filter_map(|p| match p {
                Piece::Text(t) => Some(t),
                Piece::Name(_) => None,
            })
            .collect::<String>()
            .trim()
            .to_owned()
    }
}

enum Piece<'a> {
    Text(&'a str),
    Name(&'a str),
}

/// `text` split into literal text and `${NAME}` references.
fn pieces(text: &str) -> Result<Vec<Piece<'_>>, String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("${") {
        out.push(Piece::Text(&rest[..at]));
        let Some(end) = rest[at..].find('}').map(|e| e + at) else {
            return Err(format!("{:?} is not closed with `}}`", &rest[at..]));
        };
        let name = &rest[at + 2..end];
        if !identifier(name) {
            return Err(format!(
                "${{{name}}}: a variable's name is letters, digits and `_`"
            ));
        }
        out.push(Piece::Name(name));
        rest = &rest[end + 1..];
    }
    out.push(Piece::Text(rest));
    Ok(out)
}

/// `[A-Za-z_][A-Za-z0-9_]*`.
fn identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Builtins {
    /// The `ask_owner` tool: questions that do not wait for the answer.
    pub ask_owner: bool,
    /// The report every run must end with.
    pub report: Option<Report>,
    /// Host-counted facts after each run.
    pub facts: bool,
    pub gate: Gate,
    /// A scratch folder the gate leaves to the agent.
    pub scratch: bool,
    /// Which of yoagent's tools the agent has.
    pub tools: Tools,
}

/// Which of yoagent's tools an agent has (`[builtins] tools`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Tools {
    /// All six: bash, read, write, edit, list, search (the default).
    Coding,
    /// Read, list and search only, their paths confined to the agent's
    /// folder.
    ReadOnly,
}

/// The approval gate (`yobuild::builtins::Gate`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Gate {
    /// The allow-list (the default).
    AllowList,
    /// No gate: every call runs.
    Off,
}

impl Default for Builtins {
    fn default() -> Self {
        Self {
            ask_owner: false,
            report: None,
            facts: false,
            gate: Gate::AllowList,
            scratch: false,
            tools: Tools::Coding,
        }
    }
}

/// `[extend]` (design §14): the agent finds plugins (`find_plugin`) and,
/// with the owner's approval each time, loads them for the session.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Extend {
    /// What may be found and loaded: MCP stdio servers, pi and DSH packages
    /// (design §14 "Stage 2").
    pub kinds: Vec<ExtendKind>,
    /// Servers kept per session (1 to 5).
    pub max_loads: u32,
    /// `load_plugin` calls per session (1 to 10).
    pub max_attempts: u32,
    /// Whole days a version must have been published (0 to 365).
    pub min_age_days: u32,
}

/// A kind of plugin `[extend]` may find.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExtendKind {
    /// An MCP server over stdio, from npm.
    Mcp,
    /// A pi package, from npm.
    Pi,
    /// A DSH package, from npm.
    Dsh,
}

impl ExtendKind {
    /// Its name in the manifest and in `find_plugin`'s `kind`.
    pub fn name(self) -> &'static str {
        match self {
            ExtendKind::Mcp => "mcp",
            ExtendKind::Pi => "pi",
            ExtendKind::Dsh => "dsh",
        }
    }

    /// What one of its plugins is called, in what yobuild says.
    pub fn label(self) -> &'static str {
        match self {
            ExtendKind::Mcp => "MCP server",
            ExtendKind::Pi => "pi package",
            ExtendKind::Dsh => "DSH package",
        }
    }
}

impl Extend {
    /// Whether it finds MCP servers: the MCP registry is asked only then.
    pub fn finds_mcp(&self) -> bool {
        self.kinds.contains(&ExtendKind::Mcp)
    }
}

impl Default for Extend {
    fn default() -> Self {
        Self {
            kinds: vec![ExtendKind::Mcp],
            max_loads: 2,
            max_attempts: 4,
            min_age_days: 7,
        }
    }
}

/// The tool names `[extend]` brings: never another tool's.
pub const EXTEND_TOOLS: [&str; 2] = ["find_plugin", "load_plugin"];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Dsh {
    /// DSH's own host services (files, attachments, credentials, …).
    pub host_services: bool,
}

/// Which frontends serve the session: the browser (`web`, with a link for
/// a phone on the LAN when `phone`), or the terminal — not both.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Frontend {
    pub web: bool,
    pub phone: bool,
    pub terminal: bool,
}

/// What is wrong with a manifest, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ManifestError {
    pub path: PathBuf,
    /// 1-based line and column (in characters), when the error has a place.
    pub at: Option<(usize, usize)>,
    pub message: String,
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.at {
            Some((line, col)) => {
                write!(f, "{}:{line}:{col}: {}", self.path.display(), self.message)
            }
            None => write!(f, "{}: {}", self.path.display(), self.message),
        }
    }
}

impl std::error::Error for ManifestError {}

// What the file says, as read: unknown keys are errors.

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    agent: RawAgent,
    #[serde(default)]
    limits: RawLimits,
    #[serde(default, rename = "plugin")]
    plugins: Vec<Spanned<RawPlugin>>,
    #[serde(default)]
    mcp: Vec<Spanned<RawMcp>>,
    #[serde(default)]
    builtins: RawBuiltins,
    extend: Option<Spanned<RawExtend>>,
    #[serde(default)]
    dsh: RawDsh,
    #[serde(default)]
    frontend: RawFrontend,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAgent {
    name: Spanned<String>,
    model: Spanned<String>,
    prompt: Option<String>,
    base_url: Option<Spanned<String>>,
    api_key_env: Option<Spanned<String>>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawLimits {
    max_turns: Option<Spanned<usize>>,
    max_tokens: Option<Spanned<u64>>,
    max_minutes: Option<Spanned<u64>>,
    budget_usd: Option<Spanned<f64>>,
    session_budget_usd: Option<Spanned<f64>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPlugin {
    name: Option<Spanned<String>>,
    pi: Option<Spanned<String>>,
    dsh: Option<Spanned<String>>,
    rutis: Option<Spanned<String>>,
    runtime: Option<Runtime>,
    config: Option<Spanned<toml::Value>>,
    allow: Option<Spanned<Vec<Spanned<String>>>>,
    #[serde(default)]
    policy: bool,
    grants: Option<Spanned<RawGrants>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGrants {
    workspace: Option<Spanned<bool>>,
    trust: Option<Spanned<bool>>,
    model: Option<Spanned<String>>,
    wake: Option<Spanned<bool>>,
    exec: Option<Spanned<String>>,
    env: Option<Vec<Spanned<String>>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMcp {
    name: Option<Spanned<String>>,
    npm: Option<Spanned<String>>,
    bin: Option<Spanned<String>>,
    url: Option<Spanned<String>>,
    #[serde(default)]
    args: Vec<Spanned<String>>,
    #[serde(default)]
    env: BTreeMap<String, Spanned<String>>,
    #[serde(default)]
    headers: BTreeMap<String, Spanned<String>>,
    allow: Option<Spanned<Vec<Spanned<String>>>>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawBuiltins {
    #[serde(default)]
    ask_owner: bool,
    report: Option<Spanned<toml::Value>>,
    #[serde(default)]
    facts: bool,
    gate: Option<Spanned<String>>,
    #[serde(default)]
    scratch: bool,
    tools: Option<Spanned<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawExtend {
    kinds: Option<Spanned<Vec<Spanned<String>>>>,
    max_loads: Option<Spanned<i64>>,
    max_attempts: Option<Spanned<i64>>,
    min_age_days: Option<Spanned<i64>>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawDsh {
    #[serde(default)]
    host_services: bool,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawFrontend {
    #[serde(default)]
    web: bool,
    phone: Option<Spanned<bool>>,
    terminal: Option<Spanned<bool>>,
}

/// An error at a byte range of the text.
type Located = (Range<usize>, String);

impl Manifest {
    /// This manifest with each built-in plugin (`yobuild:<name>`) given its
    /// file under `frontend` (yobuild's frontend folder, `Layout::frontend`):
    /// a pi plugin's source becomes that path, a rutis plugin's `path` is
    /// set (`Plugin::builtin` kept). `run` and `check` call it first;
    /// refused when the file is not there.
    pub fn with_builtins(mut self, frontend: &Path) -> Result<Self, String> {
        let file = |name: &str| -> Result<PathBuf, String> {
            let (_, _, at) = BUILTIN_PLUGINS
                .iter()
                .find(|(n, _, _)| *n == name)
                .ok_or_else(|| format!("`yobuild:{name}` is not a built-in plugin"))?;
            let path = frontend.join(at);
            match path.is_file() {
                true => Ok(path),
                false => Err(format!(
                    "`yobuild:{name}`: yobuild's own file is not there ({})",
                    path.display()
                )),
            }
        };
        for plugin in &mut self.plugins {
            let Some(name) = &plugin.builtin else {
                continue;
            };
            match &mut plugin.kind {
                PluginKind::Pi(source) => *source = Source::Path(file(name)?),
                PluginKind::Rutis { path, .. } => *path = file(name)?,
                PluginKind::Dsh(_) => {}
            }
        }
        Ok(self)
    }

    /// Whether the pi plugins are told the model (`model = "info"`): on
    /// every pi plugin or none, which `parse` enforces (the pi adapter has
    /// one model for all its extensions).
    pub fn pi_model(&self) -> bool {
        self.plugins
            .iter()
            .any(|p| matches!(p.kind, PluginKind::Pi(_)) && p.grants.model.is_some())
    }

    /// The variables that may hold a credential yoagent reads:
    /// [`KEY_VARIABLES`], then the agent's own (`api_key_env`, a custom
    /// provider's) when it is not among them; each once. Those set in
    /// yobuild's environment are taken out of plugins' and stdio MCP
    /// servers' unless the manifest hands one over.
    pub fn key_variables(&self) -> Vec<String> {
        key_variables(&self.agent.model)
    }

    /// What every DSH plugin is granted — the same on each, which `parse`
    /// enforces (one DSH host serves them all) — when that is anything the
    /// DSH host serves (`env` alone is not: [`Grants::hosts_nothing`]).
    pub fn dsh_grants(&self) -> Option<&Grants> {
        self.plugins
            .iter()
            .find(|p| matches!(p.kind, PluginKind::Dsh(_)))
            .map(|p| &p.grants)
            .filter(|g| !g.hosts_nothing())
    }

    /// Reads and checks the manifest at `path`; its folder, made absolute,
    /// is what its paths are relative to.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ManifestError> {
        let path = path.as_ref();
        let error = |message: String| ManifestError {
            path: path.to_owned(),
            at: None,
            message,
        };
        let text = std::fs::read_to_string(path).map_err(|e| error(e.to_string()))?;
        let parent = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        let dir = std::path::absolute(parent).map_err(|e| error(e.to_string()))?;
        Self::parse(&text, path, &normal(&dir))
    }

    /// Reads and checks manifest `text`; `path` names it in errors, `dir` is
    /// what its paths are relative to.
    pub fn parse(text: &str, path: &Path, dir: &Path) -> Result<Self, ManifestError> {
        let located = |(span, message): Located| ManifestError {
            path: path.to_owned(),
            at: Some(line_col(text, span.start)),
            message,
        };
        let raw: Raw = toml::from_str(text).map_err(|e| ManifestError {
            path: path.to_owned(),
            at: e.span().map(|s| line_col(text, s.start)),
            message: e.message().to_owned(),
        })?;

        let agent = agent(raw.agent, dir).map_err(located)?;
        let credentials = key_variables(&agent.model);
        let limits = limits(raw.limits, &agent.model).map_err(located)?;
        let mut names: Vec<String> = Vec::new();
        let mut unique = |name: &str, span: Range<usize>| -> Result<(), Located> {
            if names.iter().any(|n| n == name) {
                let m = format!("two plugins or MCP servers are named {name:?}: give one a `name`");
                return Err((span, m));
            }
            names.push(name.to_owned());
            Ok(())
        };
        // Each set's pins (design §4): one npm project holds one version of
        // a package at the top, so two entries of a set must agree.
        let mut pins: Vec<(&str, Package)> = Vec::new();
        let mut pinned = |set: &'static str, source: &Source, span: Range<usize>| {
            let Source::Package(package) = source else {
                return Ok(());
            };
            if let Some((_, other)) = pins
                .iter()
                .find(|(s, p)| *s == set && p.name() == package.name() && p != package)
            {
                let m = format!(
                    "{}: another entry pins {} at {}, and the `{set}` set holds one version of a package",
                    package.spec(),
                    package.name(),
                    other.version()
                );
                return Err((span, m));
            }
            pins.push((set, package.clone()));
            Ok(())
        };
        // Where an entry allows tools: with the gate off, that means nothing.
        let mut allowing: Option<Range<usize>> = None;
        let mut plugins: Vec<Plugin> = Vec::new();
        for raw in raw.plugins {
            let span = raw.span();
            if let Some(a) = raw
                .get_ref()
                .allow
                .as_ref()
                .filter(|a| !a.get_ref().is_empty())
            {
                allowing.get_or_insert(a.span());
            }
            // Where a pi or DSH plugin's grants are said: its `grants`, else
            // its `pi` / `dsh`.
            let grants_at = {
                let r = raw.get_ref();
                let at = r.grants.as_ref().map(Spanned::span);
                at.or_else(|| r.pi.as_ref().or(r.dsh.as_ref()).map(Spanned::span))
            };
            let (plugin, name_span) =
                plugin(raw.into_inner(), span.clone(), dir, &credentials).map_err(located)?;
            unique(&plugin.name, name_span).map_err(located)?;
            match &plugin.kind {
                PluginKind::Pi(source) => pinned("pi", source, span.clone()).map_err(located)?,
                PluginKind::Dsh(source) => pinned("dsh", source, span.clone()).map_err(located)?,
                PluginKind::Rutis { .. } => {}
            }
            // pi's adapter has one model for every extension: `model` on
            // every pi plugin or none (the one rule).
            if let PluginKind::Pi(_) = plugin.kind {
                let first = plugins.iter().find(|p| matches!(p.kind, PluginKind::Pi(_)));
                if let Some(first) = first.filter(|f| f.grants.model != plugin.grants.model) {
                    let (with, without) = match first.grants.model.is_some() {
                        true => (&first.name, &plugin.name),
                        false => (&plugin.name, &first.name),
                    };
                    let m = format!(
                        "every pi extension sees one model: grant `model = \"info\"` to each pi plugin or none (`{with}` has it, `{without}` not)"
                    );
                    return Err(located((grants_at.clone().unwrap_or(span), m)));
                }
                if let Some(first) = first.filter(|f| f.grants.env != plugin.grants.env) {
                    let m = format!(
                        "every pi extension runs in one runtime, whose environment is theirs together: give each pi plugin the same `env` grant or none ({})",
                        env_differ(first, &plugin)
                    );
                    return Err(located((grants_at.unwrap_or(span), m)));
                }
            }
            // One DSH runtime runs every DSH plugin, and one DSH host serves
            // them: a grant to one is a grant to all, so the manifest writes
            // the same on each (design §12, "One rule for both ecosystems").
            if let PluginKind::Dsh(_) = plugin.kind {
                let first = plugins
                    .iter()
                    .find(|p| matches!(p.kind, PluginKind::Dsh(_)));
                if let Some(first) = first.filter(|f| f.grants.env != plugin.grants.env) {
                    let m = format!(
                        "every DSH plugin runs in one runtime, whose environment is theirs together: give each DSH plugin the same `env` grant or none ({})",
                        env_differ(first, &plugin)
                    );
                    return Err(located((grants_at.unwrap_or(span), m)));
                }
                if let Some(first) = first.filter(|f| f.grants != plugin.grants) {
                    let said = |g: &Grants| match g.is_empty() {
                        true => "none".to_owned(),
                        false => g.names().join(", "),
                    };
                    let m = format!(
                        "every DSH plugin shares the DSH host: give each the same grants (`{}` has {}, `{}` {})",
                        first.name,
                        said(&first.grants),
                        plugin.name,
                        said(&plugin.grants)
                    );
                    return Err(located((grants_at.unwrap_or(span), m)));
                }
            }
            plugins.push(plugin);
        }
        let mut mcp = Vec::new();
        for raw in raw.mcp {
            let span = raw.span();
            if let Some(a) = raw
                .get_ref()
                .allow
                .as_ref()
                .filter(|a| !a.get_ref().is_empty())
            {
                allowing.get_or_insert(a.span());
            }
            let (server, name_span) =
                server(raw.into_inner(), span.clone(), dir).map_err(located)?;
            unique(&server.name, name_span).map_err(located)?;
            if let Transport::Stdio { source, .. } = &server.transport {
                pinned("mcp", source, span).map_err(located)?;
            }
            mcp.push(server);
        }
        let builtins = builtins(raw.builtins).map_err(located)?;
        if let (Gate::Off, Some(span)) = (builtins.gate, allowing) {
            let m = "allow with gate = \"off\": nothing asks, so there is nothing to allow";
            return Err(located((span, m.into())));
        }
        let extend = match raw.extend {
            None => None,
            Some(raw) => {
                if builtins.gate == Gate::Off {
                    let m = "[extend] with gate = \"off\": a loaded server's tools would run without asking";
                    return Err(located((raw.span(), m.into())));
                }
                Some(extend(raw.into_inner()).map_err(located)?)
            }
        };
        let frontend = frontend(raw.frontend).map_err(located)?;

        Ok(Manifest {
            path: path.to_owned(),
            dir: dir.to_owned(),
            agent,
            limits,
            plugins,
            mcp,
            builtins,
            extend,
            dsh: Dsh {
                host_services: raw.dsh.host_services,
            },
            frontend,
        })
    }
}

/// 1-based line and column (in characters) of byte `offset` in `text`.
fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let before = &text[..offset.min(text.len())];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    (line, col)
}

/// `path` without `.` components.
fn normal(path: &Path) -> PathBuf {
    path.components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect()
}

/// [`Manifest::key_variables`], for the agent's model.
fn key_variables(model: &Model) -> Vec<String> {
    let mut names: Vec<String> = KEY_VARIABLES.iter().map(|n| n.to_string()).collect();
    for name in model.key_env() {
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    names
}

fn agent(raw: RawAgent, dir: &Path) -> Result<Agent, Located> {
    let name = raw.name.get_ref();
    let plain = matches!(
        Path::new(name).components().collect::<Vec<_>>().as_slice(),
        [Component::Normal(_)]
    );
    if !plain || name.contains(['/', '\\']) {
        let m = format!("agent name {name:?}: one plain name (it names a folder)");
        return Err((raw.name.span(), m));
    }
    let model = model(&raw)?;
    Ok(Agent {
        name: raw.name.into_inner(),
        model,
        prompt: raw.prompt.map(|p| normal(&dir.join(p))),
    })
}

fn model(raw: &RawAgent) -> Result<Model, Located> {
    let spec = raw.model.get_ref();
    let at = raw.model.span();
    let Some((provider, id)) = spec
        .split_once('/')
        .filter(|(p, i)| !p.is_empty() && !i.is_empty())
    else {
        return Err((at, format!("model {spec:?} is not `provider/id`")));
    };
    if LOCAL_PROVIDERS.contains(&provider) {
        if let Some(env) = &raw.api_key_env {
            let m = format!("{provider} takes no key: leave out api_key_env");
            return Err((env.span(), m));
        }
        let base_url = match (&raw.base_url, provider) {
            (Some(url), _) => {
                endpoint(url.get_ref()).map_err(|m| (url.span(), format!("base_url {m}")))?;
                url.get_ref().clone()
            }
            (None, "ollama") => OLLAMA.to_owned(),
            (None, _) => {
                let m = format!("{provider} needs base_url: its OpenAI-compatible endpoint");
                return Err((at, m));
            }
        };
        return Ok(Model {
            provider: provider.to_owned(),
            id: id.to_owned(),
            base_url: Some(base_url),
            key_env: Vec::new(),
        });
    }
    let known = PROVIDERS.iter().find(|(p, _)| *p == provider);
    let key_env = match (known, &raw.base_url, &raw.api_key_env) {
        (Some((_, envs)), None, None) => envs.iter().map(|e| e.to_string()).collect(),
        (Some(_), Some(url), _) => {
            let m = format!(
                "base_url is for a provider yoagent does not know; {provider} is one it does"
            );
            return Err((url.span(), m));
        }
        (Some(_), None, Some(env)) => {
            let m = format!(
                "api_key_env is for a provider yoagent does not know; {provider} is one it does"
            );
            return Err((env.span(), m));
        }
        (None, None, _) => {
            let names: Vec<&str> = PROVIDERS
                .iter()
                .map(|(p, _)| *p)
                .chain(LOCAL_PROVIDERS)
                .collect();
            let m = format!(
                "provider {provider:?} is not one yoagent knows ({}): for an OpenAI-compatible endpoint, set base_url and api_key_env under [agent]",
                names.join(", ")
            );
            return Err((at, m));
        }
        (None, Some(url), None) => {
            let m =
                format!("{provider} needs api_key_env: the environment variable holding its key");
            return Err((url.span(), m));
        }
        (None, Some(url), Some(env)) => {
            endpoint(url.get_ref()).map_err(|m| (url.span(), format!("base_url {m}")))?;
            if !identifier(env.get_ref()) {
                let m = format!(
                    "api_key_env {:?} is not an environment variable's name",
                    env.get_ref()
                );
                return Err((env.span(), m));
            }
            vec![env.get_ref().clone()]
        }
    };
    Ok(Model {
        provider: provider.to_owned(),
        id: id.to_owned(),
        base_url: raw.base_url.as_ref().map(|u| u.get_ref().clone()),
        key_env,
    })
}

/// An endpoint the agent sends its key or headers to: https, or http on
/// this machine (`localhost`, `127.0.0.1`, `[::1]`), with no user in it.
pub(crate) fn endpoint(text: &str) -> Result<url::Url, String> {
    let url = url::Url::parse(text).map_err(|e| format!("{text:?} is not a URL: {e}"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!(
            "{text:?} has a user in it: give credentials as a header"
        ));
    }
    let local = match url.host() {
        Some(url::Host::Domain(d)) => d == "localhost",
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    match url.scheme() {
        "https" if url.host().is_some() => Ok(url),
        "http" if local => Ok(url),
        _ => Err(format!(
            "{text:?}: https, or http on this machine (localhost)"
        )),
    }
}

fn limits(raw: RawLimits, model: &Model) -> Result<Limits, Located> {
    let positive = |v: &Option<Spanned<u64>>, key: &str| match v {
        Some(v) if *v.get_ref() == 0 => Err((v.span(), format!("{key} must be more than 0"))),
        _ => Ok(()),
    };
    positive(&raw.max_tokens, "max_tokens")?;
    positive(&raw.max_minutes, "max_minutes")?;
    if let Some(turns) = raw.max_turns.as_ref().filter(|t| *t.get_ref() == 0) {
        return Err((turns.span(), "max_turns must be more than 0".into()));
    }
    // A dollar budget: positive, and the model has a price.
    let budget = |budget: &Option<Spanned<f64>>, key: &str| -> Result<(), Located> {
        let Some(budget) = budget else {
            return Ok(());
        };
        let usd = *budget.get_ref();
        if !(usd.is_finite() && usd > 0.0) {
            let m = format!("{key} {usd}: a number of dollars more than 0");
            return Err((budget.span(), m));
        }
        // The table `yobuild run` enables; read here without enabling it.
        let priced = model
            .config()
            .with_prices(&PriceTable::builtin())
            .cost
            .is_some();
        if !priced {
            let m = format!(
                "{key} set, but {}/{} has no price",
                model.provider, model.id
            );
            return Err((budget.span(), m));
        }
        Ok(())
    };
    budget(&raw.budget_usd, "budget_usd")?;
    budget(&raw.session_budget_usd, "session_budget_usd")?;
    Ok(Limits {
        max_turns: raw.max_turns.map(Spanned::into_inner),
        max_tokens: raw.max_tokens.map(Spanned::into_inner),
        max_minutes: raw.max_minutes.map(Spanned::into_inner),
        budget_usd: raw.budget_usd.map(Spanned::into_inner),
        session_budget_usd: raw.session_budget_usd.map(Spanned::into_inner),
    })
}

/// A registry package at an exact version, or a path (`./`, `../`, `/`)
/// relative to `dir`.
fn source(spec: &str, dir: &Path) -> Result<Source, String> {
    if spec.starts_with("./") || spec.starts_with("../") || spec.starts_with('/') {
        return Ok(Source::Path(normal(&dir.join(spec))));
    }
    let remote = [
        "git+", "git:", "github:", "gitlab:", "bitbucket:", "file:", "npm:", "http:", "https:",
    ];
    if spec.contains("://") || remote.iter().any(|r| spec.starts_with(r)) {
        return Err(format!(
            "{spec:?} is not a registry package: give `name@1.2.3`, or a path (`./…`)"
        ));
    }
    let Some(at) = spec.rfind('@').filter(|at| *at > 0) else {
        return Err(format!(
            "{spec:?} is not pinned: give an exact version (`{spec}@1.2.3`)"
        ));
    };
    let (name, version) = (&spec[..at], &spec[at + 1..]);
    if !npm_name(name) {
        return Err(format!(
            "{name:?} is not an npm package name (lowercase letters, digits, `-._~`, an optional `@scope/`)"
        ));
    }
    if !semver(version) {
        return Err(format!(
            "{spec:?} is not pinned: give an exact version (`{name}@1.2.3`)"
        ));
    }
    Ok(Source::Package(Package {
        name: name.to_owned(),
        version: version.to_owned(),
    }))
}

/// npm's rule for a package name: `name` or `@scope/name`, each part
/// lowercase letters, digits and `-._~`, not starting with `.` or `_`; at
/// most 214 characters.
fn npm_name(name: &str) -> bool {
    let part = |p: &str| {
        !p.is_empty()
            && !p.starts_with(['.', '_'])
            && p.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-._~".contains(c))
    };
    let parts_ok = match name.strip_prefix('@') {
        Some(scoped) => {
            matches!(scoped.split_once('/'), Some((scope, n)) if part(scope) && part(n))
        }
        None => part(name),
    };
    name.len() <= 214 && parts_ok
}

/// An exact semver version: `x.y.z`, an optional `-pre` and `+build`, no
/// empty identifiers, no leading zeros in numbers.
fn semver(version: &str) -> bool {
    let (rest, build) = match version.split_once('+') {
        Some((r, b)) => (r, Some(b)),
        None => (version, None),
    };
    let (core, pre) = match rest.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (rest, None),
    };
    let number = |p: &str| {
        !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) && (p == "0" || !p.starts_with('0'))
    };
    let ident = |p: &str| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    let numbers: Vec<&str> = core.split('.').collect();
    numbers.len() == 3
        && numbers.iter().all(|n| number(n))
        && pre.is_none_or(|p| {
            p.split('.')
                .all(|i| ident(i) && (!i.chars().all(|c| c.is_ascii_digit()) || number(i)))
        })
        && build.is_none_or(|b| b.split('.').all(ident))
}

/// The name a source goes by: a package's without its scope, a file's stem.
fn default_name(source: &Source) -> String {
    match source {
        Source::Package(p) => p.name.rsplit('/').next().unwrap_or(&p.name).to_owned(),
        Source::Path(path) => stem(path),
        Source::Builtin(name) => name.clone(),
    }
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// An explicit name: letters, digits, `-`, `_`, `.`.
fn explicit_name(name: &Spanned<String>) -> Result<(String, Range<usize>), Located> {
    let n = name.get_ref();
    let fine = n.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    if !fine {
        let m = format!("name {n:?}: letters, digits, `-`, `_`, `.`");
        return Err((name.span(), m));
    }
    Ok((n.clone(), name.span()))
}

/// A TOML value as JSON: a datetime as its text.
fn json(value: toml::Value) -> serde_json::Value {
    match value {
        toml::Value::String(s) => s.into(),
        toml::Value::Integer(i) => i.into(),
        toml::Value::Float(f) => {
            serde_json::Number::from_f64(f).map_or(serde_json::Value::Null, Into::into)
        }
        toml::Value::Boolean(b) => b.into(),
        toml::Value::Datetime(d) => d.to_string().into(),
        toml::Value::Array(a) => a.into_iter().map(json).collect(),
        toml::Value::Table(t) => t
            .into_iter()
            .map(|(k, v)| (k, json(v)))
            .collect::<serde_json::Map<_, _>>()
            .into(),
    }
}

/// Two plugins' `env` grants as a refusal says them: "`a` has X, Y, `b`
/// none".
pub(crate) fn env_differ(first: &Plugin, other: &Plugin) -> String {
    let said = |p: &Plugin| match p.grants.env.is_empty() {
        true => format!("`{}` none", p.name),
        false => format!("`{}` has {}", p.name, p.grants.env.join(", ")),
    };
    format!("{}, {}", said(first), said(other))
}

/// A plugin's `grants`, checked for its ecosystem (design §12's table);
/// `credentials` are the names an `env` prefix may not match
/// ([`key_variables`]).
fn grants(
    raw: Option<Spanned<RawGrants>>,
    kind: &PluginKind,
    credentials: &[String],
) -> Result<Grants, Located> {
    let Some(raw) = raw else {
        return Ok(Grants::default());
    };
    let mut raw = raw.into_inner();
    // Names or prefixes, each once (design §12 "Step 5").
    let mut env: Vec<String> = Vec::new();
    for entry in raw.env.take().into_iter().flatten() {
        if let Some(m) = env_entry(entry.get_ref(), credentials) {
            return Err((entry.span(), m));
        }
        let name = entry.get_ref();
        if env.contains(name) {
            return Err((entry.span(), format!("`{name}` is listed twice")));
        }
        env.push(name.clone());
    }
    env.sort();
    let env = env;
    let model = |raw: Option<Spanned<String>>| match raw {
        None => Ok(None),
        Some(model) if model.get_ref() == "info" => Ok(Some(ModelGrant::Info)),
        Some(model) => {
            let m = format!(
                "model {:?}: \"info\" (the model's provider, id and limits; model calls are not granted yet)",
                model.get_ref()
            );
            Err((model.span(), m))
        }
    };
    let on = |b: Option<Spanned<bool>>| b.is_some_and(|b| *b.get_ref());
    if let PluginKind::Pi(_) = kind {
        // pi's host: the adapter's per-extension settings (design §12,
        // "Step 3"); a pi extension has its folder already (`ctx.cwd`).
        if let Some(workspace) = &raw.workspace {
            let m = "a pi extension takes `trust`, `model`, `wake`, `exec` and `env`, not `workspace` (it has the folder: `ctx.cwd`)";
            return Err((workspace.span(), m.into()));
        }
        let exec = match raw.exec {
            None => ExecGrant::Off,
            Some(exec) => match exec.get_ref().as_str() {
                "off" => ExecGrant::Off,
                "gate" => ExecGrant::Gate,
                "free" => ExecGrant::Free,
                other => {
                    let m = format!("exec {other:?}: \"off\", \"gate\" (each command judged by the gate) or \"free\"");
                    return Err((exec.span(), m));
                }
            },
        };
        return Ok(Grants {
            trust: on(raw.trust),
            model: model(raw.model)?,
            wake: on(raw.wake),
            exec,
            env,
            ..Grants::default()
        });
    }
    if let PluginKind::Dsh(_) = kind {
        // The DSH host gives the stand-in agent's folder and its wake; the
        // rest is not its to give (design §12, "Step 2").
        let other = [
            raw.trust.as_ref().map(|t| (t.span(), "trust")),
            raw.model.as_ref().map(|m| (m.span(), "model")),
            raw.exec.as_ref().map(|e| (e.span(), "exec")),
        ];
        if let Some((at, key)) = other.into_iter().flatten().next() {
            let m = format!("a DSH plugin takes `workspace`, `wake` and `env`, not `{key}`");
            return Err((at, m));
        }
        let workspace = raw.workspace.is_some_and(|w| *w.get_ref());
        if let Some(wake) = raw.wake.filter(|w| *w.get_ref()) {
            if !workspace {
                let m = "a DSH plugin's `wake` needs `workspace` (the stand-in agent reads the folder first)";
                return Err((wake.span(), m.into()));
            }
            return Ok(Grants {
                workspace,
                wake: true,
                env,
                ..Grants::default()
            });
        }
        return Ok(Grants {
            workspace,
            env,
            ..Grants::default()
        });
    }
    if let Some(exec) = &raw.exec {
        return Err((exec.span(), "exec is for a pi extension (`pi.exec`)".into()));
    }
    Ok(Grants {
        workspace: on(raw.workspace),
        trust: on(raw.trust),
        model: model(raw.model)?,
        wake: on(raw.wake),
        exec: ExecGrant::Off,
        env,
    })
}

/// Why an `env` grant's entry is refused, if it is: a name (letters,
/// digits and `_`, not beginning with a digit) or a prefix — at least two
/// such characters, then `_*` (`PI_LENS_*`). A prefix may not match a
/// credential yoagent reads (`credentials`) nor a name that loads code
/// (`NODE_OPTIONS`, `NODE_PATH`, `LD_PRELOAD`, `DYLD_*`): those pass only
/// by their full name, so what `check` notes stays exact.
fn env_entry(entry: &str, credentials: &[String]) -> Option<String> {
    let name = |s: &str| {
        s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && s.chars().next().is_some_and(|c| !c.is_ascii_digit())
    };
    let Some(prefix) = entry.strip_suffix('*') else {
        return (!name(entry)).then(|| {
            format!("`{entry}`: a variable's name (letters, digits and `_`), or a prefix ending in `_*`")
        });
    };
    if !name(prefix) || !prefix.ends_with('_') || prefix.len() < 3 {
        return Some(format!(
            "`{entry}`: a prefix is at least two characters (letters, digits or `_`), then `_*` (`PI_LENS_*`)"
        ));
    }
    if let Some(credential) = credentials.iter().find(|c| c.starts_with(prefix)) {
        return Some(format!(
            "`{entry}` would hand over `{credential}`, a credential yoagent reads: name it"
        ));
    }
    let dyld = crate::environment::CODE_LOADING_PREFIX;
    let code = crate::environment::CODE_LOADING
        .iter()
        .find(|c| c.starts_with(prefix))
        .map(|c| format!("`{c}`"))
        .or_else(|| {
            (dyld.starts_with(prefix) || prefix.starts_with(dyld)).then(|| format!("`{dyld}*`"))
        });
    code.map(|code| {
        format!("`{entry}` would hand over {code}, which loads code into the runtime: name it")
    })
}

fn plugin(
    raw: RawPlugin,
    table: Range<usize>,
    dir: &Path,
    credentials: &[String],
) -> Result<(Plugin, Range<usize>), Located> {
    let given: Vec<&Spanned<String>> = [&raw.pi, &raw.dsh, &raw.rutis]
        .into_iter()
        .flatten()
        .collect();
    let [spec] = given.as_slice() else {
        let span = given.get(1).map_or(table, |s| s.span());
        return Err((
            span,
            "a [[plugin]] names exactly one of pi, dsh or rutis".into(),
        ));
    };
    let span = spec.span();
    if raw.runtime.is_some() && raw.rutis.is_none() {
        return Err((span, "runtime is for a rutis plugin".into()));
    }
    let config = match raw.config {
        None => None,
        Some(config) => match json(config.get_ref().clone()) {
            serde_json::Value::Object(map) => {
                check_config(&serde_json::Value::Object(map.clone()), "")
                    .map_err(|m| (config.span(), m))?;
                Some(map)
            }
            _ => {
                let m = "config is a table: the plugin's own settings";
                return Err((config.span(), m.into()));
            }
        },
    };
    let builtin = spec.get_ref().strip_prefix("yobuild:").map(str::to_owned);
    let kind = if let Some(rutis) = &raw.rutis {
        let text = rutis.get_ref();
        if let Some(name) = text.strip_prefix("yobuild:") {
            builtin_of(name, "rutis").map_err(|m| (span.clone(), m))?;
            let runtime = match raw.runtime {
                None | Some(Runtime::Node) => Runtime::Node,
                Some(_) => {
                    let m = format!("`yobuild:{name}` runs on node");
                    return Err((span, m));
                }
            };
            PluginKind::Rutis {
                path: PathBuf::new(),
                runtime,
            }
        } else {
            if text.contains('@') && !text.contains('/') {
                let m = format!("{text:?}: a rutis plugin is a local file (`./plugin.ts`)");
                return Err((span, m));
            }
            let path = normal(&dir.join(text));
            let Some(runtime) = raw.runtime else {
                let m = "a rutis plugin needs a runtime: node, bun, python or go";
                return Err((span, m.into()));
            };
            PluginKind::Rutis { path, runtime }
        }
    } else {
        let source = match spec.get_ref().strip_prefix("yobuild:") {
            Some(name) => {
                let kind = if raw.pi.is_some() { "pi" } else { "dsh" };
                builtin_of(name, kind).map_err(|m| (span.clone(), m))?;
                Source::Builtin(name.to_owned())
            }
            None => source(spec.get_ref(), dir).map_err(|m| (span.clone(), m))?,
        };
        match raw.pi {
            Some(_) => PluginKind::Pi(source),
            None if raw.policy => {
                let m = "a DSH plugin cannot be the approval policy (`policy`): DSH's adapter judges no calls";
                return Err((span, m.into()));
            }
            None => PluginKind::Dsh(source),
        }
    };
    let (name, name_span) = match &raw.name {
        Some(name) => explicit_name(name)?,
        None => {
            let name = match (&builtin, &kind) {
                (Some(name), _) => name.clone(),
                (None, PluginKind::Pi(s) | PluginKind::Dsh(s)) => default_name(s),
                (None, PluginKind::Rutis { path, .. }) => stem(path),
            };
            (name, span)
        }
    };
    let allow = allow(raw.allow.as_ref(), true)?;
    let grants = grants(raw.grants, &kind, credentials)?;
    Ok((
        Plugin {
            name,
            kind,
            config,
            allow,
            policy: raw.policy,
            grants,
            builtin,
        },
        name_span,
    ))
}

/// A header or variable that carries a secret, by the words of its name
/// (split at `_`, `-` and lower-to-upper case changes).
pub(crate) fn secret(name: &str) -> bool {
    const WORDS: [&str; 12] = [
        "key", "apikey", "token", "secret", "password", "passwd", "pwd", "auth", "authorization",
        "cookie", "credential", "credentials",
    ];
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut previous_lower = false;
    for c in name.chars() {
        let separator = c == '_' || c == '-';
        if (separator || (c.is_ascii_uppercase() && previous_lower)) && !word.is_empty() {
            words.push(std::mem::take(&mut word));
        }
        if !separator {
            word.push(c.to_ascii_lowercase());
        }
        previous_lower = c.is_ascii_lowercase();
    }
    words.push(word);
    words.iter().any(|w| WORDS.contains(&w.as_str()))
}

/// The one rule for a value the manifest gives a plugin or a server — a
/// variable, a header, an argument, a URL, a string in a plugin's config:
/// a secret is never written into the manifest. A value named like a
/// secret (`name`) is only `${NAME}` references, with at most a scheme word
/// (`Bearer ${TOKEN}`); a URL in any value (`scheme://user:password@host`)
/// has its password as a reference.
fn guard(name: &str, value: &Value) -> Result<(), String> {
    if secret(name) {
        let literal = value.literal().to_ascii_lowercase();
        let scheme = ["", "bearer", "basic", "token"].contains(&literal.as_str());
        if value.references().is_empty() || !scheme {
            return Err(format!(
                "{name} looks like a secret: take it from the environment (`\"${{NAME}}\"`, or `\"Bearer ${{NAME}}\"`), never write it here"
            ));
        }
    }
    if written_password(value.as_str()) {
        return Err(format!(
            "{} has a URL with its password written in: take it from the environment (`://user:${{PASSWORD}}@`)",
            if name.is_empty() { "a value" } else { name }
        ));
    }
    Ok(())
}

/// Whether `text` holds a URL whose password (`://user:password@`) is
/// written, not a `${NAME}` reference.
pub(crate) fn written_password(text: &str) -> bool {
    text.match_indices("://").any(|(at, _)| {
        let rest = &text[at + 3..];
        let authority = rest
            .split(|c: char| c == '/' || c == '?' || c == '#' || c.is_whitespace())
            .next()
            .unwrap_or("");
        let Some(user_info) = authority.rfind('@').map(|end| &authority[..end]) else {
            return false;
        };
        let Some((_, password)) = user_info.split_once(':') else {
            return false;
        };
        let reference = password
            .strip_prefix("${")
            .and_then(|p| p.strip_suffix('}'))
            .is_some_and(identifier);
        !password.is_empty() && !reference
    })
}

fn values(raw: BTreeMap<String, Spanned<String>>) -> Result<BTreeMap<String, Value>, Located> {
    raw.into_iter()
        .map(|(name, value)| {
            let span = value.span();
            let value = Value::new(value.into_inner()).map_err(|m| (span.clone(), m))?;
            guard(&name, &value).map_err(|m| (span, m))?;
            Ok((name, value))
        })
        .collect()
}

/// A stdio server's arguments: a value after a flag named like a secret
/// (`--token X`, `--api-key=X`) is that secret.
fn arguments(raw: Vec<Spanned<String>>) -> Result<Vec<Value>, Located> {
    guarded_arguments(raw.into_iter().map(|arg| (arg.span(), arg.into_inner())))
}

/// The arguments the agent gives a server it loads (`load_plugin`, design
/// §14): as a manifest's server's `args` are guarded, with no `${…}` at
/// all, since nothing fills one in, and no control character (the block
/// that keeps the server is TOML, whose strings take none unescaped, and
/// the question shows each argument on one line).
pub(crate) fn loaded_arguments(args: &[String]) -> Result<(), String> {
    if let Some(arg) = args.iter().find(|a| a.chars().any(char::is_control)) {
        return Err(format!(
            "argument {arg:?} holds a control character: an argument is one line of text"
        ));
    }
    let values = guarded_arguments(args.iter().map(|a| (0..0, a.clone()))).map_err(|(_, m)| m)?;
    match values.iter().find(|v| !v.references().is_empty()) {
        Some(v) => Err(format!(
            "argument {:?} refers to a variable: nothing fills one in for a loaded server",
            v.as_str()
        )),
        None => Ok(()),
    }
}

/// [`arguments`] of `(place, text)` pairs.
fn guarded_arguments(
    raw: impl IntoIterator<Item = (std::ops::Range<usize>, String)>,
) -> Result<Vec<Value>, Located> {
    let mut out = Vec::new();
    let mut flag: Option<String> = None;
    for (span, text) in raw {
        let value = Value::new(text.clone()).map_err(|m| (span.clone(), m))?;
        let name = match (text.strip_prefix('-'), flag.take()) {
            (Some(option), _) => {
                let option = option.trim_start_matches('-');
                match option.split_once('=') {
                    Some((name, given)) => {
                        let given = Value::new(given).map_err(|m| (span.clone(), m))?;
                        guard(name, &given).map_err(|m| (span.clone(), m))?;
                    }
                    None => flag = Some(option.to_owned()),
                }
                String::new()
            }
            (None, Some(name)) => name,
            (None, None) => String::new(),
        };
        guard(&name, &value).map_err(|m| (span, m))?;
        out.push(value);
    }
    Ok(out)
}

/// A server's URL: references checked, the endpoint (with each reference
/// standing in as `x`) https or this machine's, and a query parameter named
/// like a secret (`?api_key=…`) a reference.
fn server_url(raw: Spanned<String>) -> Result<(Value, String), Located> {
    let span = raw.span();
    let value = Value::new(raw.into_inner()).map_err(|m| (span.clone(), m))?;
    let filled = value
        .resolve_with(|_| Some("x".into()))
        .expect("every reference filled");
    let host = endpoint(&filled)
        .map_err(|m| (span.clone(), format!("url {m}")))?
        .host_str()
        .unwrap_or_default()
        .to_owned();
    let query = value.as_str().split_once('?').map_or("", |(_, q)| q);
    for pair in query.split(['&', '#']).filter(|p| !p.is_empty()) {
        let (name, given) = pair.split_once('=').unwrap_or((pair, ""));
        let given = Value::new(given).map_err(|m| (span.clone(), m))?;
        guard(name, &given).map_err(|m| (span.clone(), format!("url: {m}")))?;
    }
    Ok((value, host))
}

/// Every string in a plugin's configuration, by the key it is under (in a
/// list, the list's key): references checked, and the rule of [`guard`].
fn check_config(value: &serde_json::Value, key: &str) -> Result<(), String> {
    match value {
        serde_json::Value::String(text) => {
            let value = Value::new(text.clone()).map_err(|m| format!("config.{key}: {m}"))?;
            guard(key, &value).map_err(|m| format!("config.{key}: {m}"))
        }
        serde_json::Value::Array(items) => items.iter().try_for_each(|v| check_config(v, key)),
        serde_json::Value::Object(map) => map.iter().try_for_each(|(k, v)| check_config(v, k)),
        _ => Ok(()),
    }
}

/// `value` with each string's `${NAME}` references filled in by `lookup`.
fn fill(
    value: &serde_json::Value,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<serde_json::Value, Unset> {
    Ok(match value {
        serde_json::Value::String(text) => Value::new(text.clone())
            .expect("checked when read")
            .resolve_with(lookup)?
            .into(),
        serde_json::Value::Array(items) => items
            .iter()
            .map(|v| fill(v, lookup))
            .collect::<Result<_, _>>()?,
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(k, v)| Ok((k.clone(), fill(v, lookup)?)))
            .collect::<Result<serde_json::Map<_, _>, _>>()?
            .into(),
        other => other.clone(),
    })
}

fn server(raw: RawMcp, table: Range<usize>, dir: &Path) -> Result<(Mcp, Range<usize>), Located> {
    let (transport, span, default) = match (raw.npm, raw.url) {
        (Some(npm), None) => {
            let span = npm.span();
            if !raw.headers.is_empty() {
                return Err((span, "headers are for an HTTP server (url)".into()));
            }
            if npm.get_ref().starts_with("yobuild:") {
                let m = "`yobuild:` names yobuild's own plugins, and an MCP server is none: give a package (`name@1.2.3`) or a path";
                return Err((span, m.into()));
            }
            let source = source(npm.get_ref(), dir).map_err(|m| (span.clone(), m))?;
            let default = default_name(&source);
            let transport = Transport::Stdio {
                source,
                bin: raw.bin.map(Spanned::into_inner),
                args: arguments(raw.args)?,
                env: values(raw.env)?,
            };
            (transport, span, default)
        }
        (None, Some(url)) => {
            let span = url.span();
            if !raw.args.is_empty() || !raw.env.is_empty() || raw.bin.is_some() {
                let m = "args, env and bin are for a stdio server (npm)";
                return Err((span, m.into()));
            }
            let (url, default) = server_url(url)?;
            let transport = Transport::Http {
                url,
                headers: values(raw.headers)?,
            };
            (transport, span, default)
        }
        (_, url) => {
            let span = url.map_or(table, |u| u.span());
            let m = "an [[mcp]] names exactly one of npm (stdio) or url (HTTP)";
            return Err((span, m.into()));
        }
    };
    let (name, name_span) = match &raw.name {
        Some(name) => explicit_name(name)?,
        None => (default, span),
    };
    // Its name prefixes its tools (`<name>__<tool>`): with `__` in it, two
    // servers' tools could read alike.
    if name.contains("__") {
        let m = format!("mcp {name:?}: `__` separates a server's name from its tools': give the server a `name` without it");
        return Err((name_span, m));
    }
    let allow = allow(raw.allow.as_ref(), false)?;
    Ok((
        Mcp {
            name,
            transport,
            allow,
        },
        name_span,
    ))
}

/// The tools the gate judges itself, call by call: never allowed by name.
const JUDGED_TOOLS: [&str; 5] = ["bash", "write_file", "write", "edit_file", "edit"];

/// An entry's `allow`: names a tool can have, each once; and on a plugin,
/// not the tools the gate judges itself, nor `ask_owner` (the built-in
/// allows its own), nor a name with `__` — it marks an MCP server's tool
/// (`<server>__<tool>`), which only that server's entry may allow. (A
/// server's names are its own: `srv__bash` is not yoagent's `bash`.)
fn allow(
    raw: Option<&Spanned<Vec<Spanned<String>>>>,
    plugin: bool,
) -> Result<Vec<String>, Located> {
    let mut names: Vec<String> = Vec::new();
    for name in raw.map(|a| a.get_ref().as_slice()).unwrap_or_default() {
        let (text, span) = (name.get_ref(), name.span());
        let fits = (1..=64).contains(&text.len())
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        let refusal = if !fits {
            Some(format!(
                "allow {text:?}: a tool name is 1 to 64 letters, digits, _ and -"
            ))
        } else if names.contains(text) {
            Some(format!("allow {text:?}: named twice"))
        } else if plugin && JUDGED_TOOLS.contains(&text.as_str()) {
            Some(format!(
                "allow {text:?}: the gate judges `{text}` call by call; it is never allowed by name"
            ))
        } else if plugin && text.contains("__") {
            Some(format!(
                "allow {text:?}: `__` marks an MCP server's tool (`<server>__<tool>`); allow it on that [[mcp]] entry, by its own name"
            ))
        } else if plugin && text == "ask_owner" {
            Some("allow \"ask_owner\": the built-in allows its own (`ask_owner = true`); a plugin's tool of that name asks".into())
        } else if plugin && EXTEND_TOOLS.contains(&text.as_str()) {
            Some(format!(
                "allow {text:?}: `[extend]` allows its own; a plugin's tool of that name asks"
            ))
        } else {
            None
        };
        if let Some(m) = refusal {
            return Err((span, m));
        }
        names.push(text.clone());
    }
    Ok(names)
}

fn builtins(raw: RawBuiltins) -> Result<Builtins, Located> {
    let report = match raw.report {
        None => None,
        Some(report) => {
            let span = report.span();
            let made = match report.into_inner() {
                toml::Value::String(name) if name == "handoff" => Ok(Report::handoff()),
                toml::Value::Array(items) => {
                    let names: Option<Vec<String>> = items
                        .into_iter()
                        .map(|i| match i {
                            toml::Value::String(s) => Some(s),
                            _ => None,
                        })
                        .collect();
                    match names {
                        Some(names) => Report::new(names),
                        None => Err("report sections are names (strings)".into()),
                    }
                }
                _ => Err("report is \"handoff\" or a list of section names".into()),
            };
            Some(made.map_err(|m| (span, m))?)
        }
    };
    let gate = match raw.gate {
        None => Gate::AllowList,
        Some(gate) => match gate.get_ref().as_str() {
            "allow-list" => Gate::AllowList,
            "off" => Gate::Off,
            other => {
                let m = format!("gate {other:?}: \"allow-list\" or \"off\"");
                return Err((gate.span(), m));
            }
        },
    };
    let tools = match raw.tools {
        None => Tools::Coding,
        Some(tools) => match tools.get_ref().as_str() {
            "coding" => Tools::Coding,
            "read-only" => Tools::ReadOnly,
            other => {
                let m = format!("tools {other:?}: \"coding\" or \"read-only\"");
                return Err((tools.span(), m));
            }
        },
    };
    Ok(Builtins {
        ask_owner: raw.ask_owner,
        report,
        facts: raw.facts,
        gate,
        scratch: raw.scratch,
        tools,
    })
}

fn extend(raw: RawExtend) -> Result<Extend, Located> {
    let mut extend = Extend::default();
    if let Some(kinds) = raw.kinds {
        if kinds.get_ref().is_empty() {
            let m =
                "[extend] kinds is empty: name what may be found (\"mcp\", \"pi\", \"dsh\"), or leave out [extend]";
            return Err((kinds.span(), m.into()));
        }
        let mut seen = Vec::new();
        for kind in kinds.into_inner() {
            let made = match kind.get_ref().as_str() {
                "mcp" => ExtendKind::Mcp,
                "pi" => ExtendKind::Pi,
                "dsh" => ExtendKind::Dsh,
                "rutis" => {
                    let m = "[extend] kinds \"rutis\": \"mcp\", \"pi\" or \"dsh\" so far; rutis plugins come in a later step (design §14)";
                    return Err((kind.span(), m.into()));
                }
                other => {
                    let m = format!("[extend] kinds {other:?}: \"mcp\", \"pi\" or \"dsh\"");
                    return Err((kind.span(), m));
                }
            };
            if seen.contains(&made) {
                let m = format!("[extend] kinds {:?}: named twice", made.name());
                return Err((kind.span(), m));
            }
            seen.push(made);
        }
        extend.kinds = seen;
    }
    let bounded = |raw: Option<Spanned<i64>>, key: &str, low: i64, high: i64, default: u32| {
        let Some(value) = raw else {
            return Ok(default);
        };
        match *value.get_ref() {
            n if (low..=high).contains(&n) => Ok(n as u32),
            n => Err((value.span(), format!("[extend] {key} {n}: {low} to {high}"))),
        }
    };
    extend.max_loads = bounded(raw.max_loads, "max_loads", 1, 5, extend.max_loads)?;
    extend.max_attempts = bounded(raw.max_attempts, "max_attempts", 1, 10, extend.max_attempts)?;
    extend.min_age_days = bounded(
        raw.min_age_days, "min_age_days", 0, 365, extend.min_age_days,
    )?;
    Ok(extend)
}

fn frontend(raw: RawFrontend) -> Result<Frontend, Located> {
    let phone = raw.phone.as_ref().is_some_and(|p| *p.get_ref());
    let terminal = raw.terminal.as_ref().is_some_and(|t| *t.get_ref());
    if let Some(p) = raw.phone.as_ref().filter(|_| phone && !raw.web) {
        let m = "phone serves the web frontend: set web = true";
        return Err((p.span(), m.into()));
    }
    if let Some(t) = raw.terminal.as_ref().filter(|_| terminal && raw.web) {
        let m = "the terminal or the web frontend, not both";
        return Err((t.span(), m.into()));
    }
    Ok(Frontend {
        web: raw.web,
        phone,
        terminal,
    })
}

#[cfg(test)]
mod tests;
