//! The agent manifest: one TOML file per agent (design §2). Read strictly —
//! an unknown key, an unpinned plugin, a budget for an unpriced model, a
//! secret written into the file are errors, each naming the file, line and
//! column — into typed parts `yobuild run` wires up.
//!
//! ```toml
//! [agent]
//! name  = "handoff"
//! model = "deepseek/deepseek-flash"
//!
//! [[plugin]]
//! pi = "@juicesharp/rpiv-todo@2.12.0"
//!
//! [parts]
//! report = "handoff"
//! ```
//!
//! Paths in the manifest are relative to its folder.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use toml::Spanned;
use yoagent::provider::model::OpenAiCompat;
use yoagent::provider::ModelConfig;

use crate::parts::Report;

/// A manifest, read and checked.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Manifest {
    /// The manifest's folder: what its paths are relative to.
    pub dir: PathBuf,
    pub agent: Agent,
    pub limits: Limits,
    pub plugins: Vec<Plugin>,
    pub mcp: Vec<Mcp>,
    pub parts: Parts,
    pub dsh: Dsh,
    pub frontend: Frontend,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Agent {
    pub name: String,
    pub model: Model,
    /// The system prompt's file (`{cwd}` and `{scratch}` are filled in).
    pub prompt: Option<PathBuf>,
}

/// `provider/id`, and how to reach a provider yoagent has no constructor for.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Model {
    pub provider: String,
    pub id: String,
    /// For a provider not in [`PROVIDERS`]: its OpenAI-compatible endpoint.
    pub base_url: Option<String>,
    /// The environment variable holding the API key.
    pub key_env: String,
}

/// The providers yoagent has a constructor for, and their key's variable.
pub const PROVIDERS: [(&str, &str); 11] = [
    ("anthropic", "ANTHROPIC_API_KEY"),
    ("openai", "OPENAI_API_KEY"),
    ("deepseek", "DEEPSEEK_API_KEY"),
    ("google", "GEMINI_API_KEY"),
    ("groq", "GROQ_API_KEY"),
    ("mistral", "MISTRAL_API_KEY"),
    ("xai", "XAI_API_KEY"),
    ("qwen", "QWEN_API_KEY"),
    ("zai", "ZAI_API_KEY"),
    ("minimax", "MINIMAX_API_KEY"),
    ("meta", "META_API_KEY"),
];

impl Model {
    /// The yoagent model configuration (priced from the bundled table once
    /// the process enabled it: `yoagent::provider::prices::enable_bundled`).
    pub fn config(&self) -> ModelConfig {
        let (id, name) = (self.id.clone(), self.id.clone());
        match self.provider.as_str() {
            "anthropic" => ModelConfig::anthropic(id, name),
            "openai" => ModelConfig::openai(id, name),
            "deepseek" => ModelConfig::deepseek(id, name),
            "google" => ModelConfig::google(id, name),
            "groq" => ModelConfig::groq(id, name),
            "mistral" => ModelConfig::mistral(id, name),
            "xai" => ModelConfig::xai(id, name),
            "qwen" => ModelConfig::qwen(id, name),
            "zai" => ModelConfig::zai(id, name),
            "minimax" => ModelConfig::minimax(id, name),
            "meta" => ModelConfig::meta(id, name),
            provider => ModelConfig::openai_compat(
                self.base_url.clone().unwrap_or_default(),
                id,
                provider,
                OpenAiCompat::default(),
            ),
        }
    }

    /// The API key, from [`key_env`](Self::key_env).
    pub fn api_key(&self) -> Option<String> {
        std::env::var(&self.key_env).ok().filter(|k| !k.is_empty())
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct Limits {
    pub max_turns: Option<usize>,
    /// Per run.
    pub max_tokens: Option<u64>,
    pub max_minutes: Option<u64>,
    /// Checked when read: the model has a price.
    pub budget_usd: Option<f64>,
}

/// Where a plugin comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A package at an exact version (`name@1.2.3`, `@scope/name@1.2.3`).
    Package { name: String, version: String },
    /// A local file or folder, relative to the manifest (`./x.ts`, `/abs`).
    Path(PathBuf),
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Plugin {
    /// A pi extension package.
    Pi {
        source: Source,
        config: Option<serde_json::Value>,
    },
    /// A DeepSeek Harness plugin.
    Dsh {
        source: Source,
        config: Option<serde_json::Value>,
    },
    /// A rutis plugin (TypeScript, Python, Go), from a local file.
    Rutis {
        path: PathBuf,
        runtime: Runtime,
        config: Option<serde_json::Value>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
    Node,
    Bun,
    Python,
    Go,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Mcp {
    /// A stdio server: a pinned npm package's bin, run with `args`.
    Stdio {
        package: Source,
        args: Vec<String>,
        env: BTreeMap<String, Value>,
    },
    /// An HTTP server: the URL is the pin.
    Http {
        url: String,
        headers: BTreeMap<String, Value>,
    },
}

/// A value with `${NAME}` references filled in from the environment when
/// the agent starts (never written into the manifest).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Value(pub String);

impl Value {
    /// The value with each `${NAME}` replaced; the first name not set, as
    /// the error.
    pub fn resolve(&self) -> Result<String, String> {
        let mut out = String::new();
        let mut rest = self.0.as_str();
        while let Some(at) = rest.find("${") {
            out.push_str(&rest[..at]);
            let end = rest[at..].find('}').ok_or_else(|| rest[at..].to_owned())? + at;
            let name = &rest[at + 2..end];
            out.push_str(&std::env::var(name).map_err(|_| name.to_owned())?);
            rest = &rest[end + 1..];
        }
        out.push_str(rest);
        Ok(out)
    }

    fn references(&self) -> bool {
        self.0.contains("${")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Parts {
    /// The `ask_owner` tool: questions that do not wait for the answer.
    pub ask_owner: bool,
    /// The report every run must end with: its required sections.
    pub report: Option<Vec<String>>,
    /// Host-counted facts after each run.
    pub facts: bool,
    /// The approval gate: on unless the manifest says `"off"`.
    pub gate: bool,
    /// A scratch folder the gate leaves to the agent.
    pub scratch: bool,
}

impl Default for Parts {
    fn default() -> Self {
        Self {
            ask_owner: false,
            report: None,
            facts: false,
            gate: true,
            scratch: false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Dsh {
    /// DSH's own host services (files, attachments, credentials, …).
    pub host_services: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Frontend {
    pub web: bool,
    /// Served on the LAN, with a phone link.
    pub phone: bool,
    pub terminal: bool,
}

/// What is wrong with a manifest, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestError {
    pub path: PathBuf,
    /// 1-based line and column, when the error has a place.
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
    parts: RawParts,
    #[serde(default)]
    dsh: RawDsh,
    #[serde(default)]
    frontend: RawFrontend,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAgent {
    name: String,
    model: Spanned<String>,
    prompt: Option<String>,
    base_url: Option<String>,
    api_key_env: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawLimits {
    max_turns: Option<usize>,
    max_tokens: Option<u64>,
    max_minutes: Option<u64>,
    budget_usd: Option<Spanned<f64>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPlugin {
    pi: Option<Spanned<String>>,
    dsh: Option<Spanned<String>>,
    rutis: Option<Spanned<String>>,
    runtime: Option<Runtime>,
    config: Option<toml::Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMcp {
    npm: Option<Spanned<String>>,
    url: Option<Spanned<String>>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, Spanned<String>>,
    #[serde(default)]
    headers: BTreeMap<String, Spanned<String>>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawParts {
    #[serde(default)]
    ask_owner: bool,
    report: Option<Spanned<toml::Value>>,
    #[serde(default)]
    facts: bool,
    gate: Option<Spanned<String>>,
    #[serde(default)]
    scratch: bool,
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
    #[serde(default)]
    phone: bool,
    #[serde(default)]
    terminal: bool,
}

impl Manifest {
    /// Reads and checks the manifest at `path`.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ManifestError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| ManifestError {
            path: path.to_owned(),
            at: None,
            message: e.to_string(),
        })?;
        let dir = path.parent().map(Path::to_owned).unwrap_or_default();
        Self::parse(&text, path, &dir)
    }

    /// Reads and checks manifest `text`; `path` names it in errors, `dir` is
    /// what its paths are relative to.
    pub fn parse(text: &str, path: &Path, dir: &Path) -> Result<Self, ManifestError> {
        let error = |span: Option<Range<usize>>, message: String| ManifestError {
            path: path.to_owned(),
            at: span.map(|s| line_col(text, s.start)),
            message,
        };
        let raw: Raw = toml::from_str(text).map_err(|e| error(e.span(), e.message().to_owned()))?;

        let model = model(&raw.agent).map_err(|m| error(Some(raw.agent.model.span()), m))?;
        let agent = Agent {
            name: raw.agent.name,
            model,
            prompt: raw.agent.prompt.map(|p| dir.join(p)),
        };

        if let Some(budget) = &raw.limits.budget_usd {
            // Priced from yoagent's bundled table, as the run will be.
            yoagent::provider::prices::enable_bundled();
            let priced =
                yoagent::extension::Budget::for_model(*budget.get_ref(), &agent.model.config());
            if priced.is_none() {
                let m = format!(
                    "budget_usd set, but {}/{} has no price",
                    agent.model.provider, agent.model.id
                );
                return Err(error(Some(budget.span()), m));
            }
        }
        let limits = Limits {
            max_turns: raw.limits.max_turns,
            max_tokens: raw.limits.max_tokens,
            max_minutes: raw.limits.max_minutes,
            budget_usd: raw.limits.budget_usd.map(|b| b.into_inner()),
        };

        let plugins = raw
            .plugins
            .into_iter()
            .map(|p| plugin(p, dir))
            .collect::<Result<_, _>>()
            .map_err(|(span, m)| error(span, m))?;
        let mcp = raw
            .mcp
            .into_iter()
            .map(|m| mcp(m, dir))
            .collect::<Result<_, _>>()
            .map_err(|(span, m)| error(span, m))?;
        let parts = parts(raw.parts).map_err(|(span, m)| error(Some(span), m))?;

        Ok(Manifest {
            dir: dir.to_owned(),
            agent,
            limits,
            plugins,
            mcp,
            parts,
            dsh: Dsh {
                host_services: raw.dsh.host_services,
            },
            frontend: Frontend {
                web: raw.frontend.web,
                phone: raw.frontend.phone,
                terminal: raw.frontend.terminal,
            },
        })
    }
}

/// 1-based line and column of byte `offset` in `text`.
fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let before = &text[..offset.min(text.len())];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    (line, col)
}

fn model(raw: &RawAgent) -> Result<Model, String> {
    let spec = raw.model.get_ref();
    let Some((provider, id)) = spec
        .split_once('/')
        .filter(|(p, i)| !p.is_empty() && !i.is_empty())
    else {
        return Err(format!("model {spec:?} is not `provider/id`"));
    };
    let known = PROVIDERS.iter().find(|(p, _)| *p == provider);
    let key_env = match (known, &raw.api_key_env, &raw.base_url) {
        (Some((_, env)), None, None) => env.to_string(),
        (Some(_), _, _) => {
            return Err(format!(
                "base_url and api_key_env are for providers yoagent does not know; {provider} is one it does"
            ))
        }
        (None, Some(env), Some(_)) => env.clone(),
        (None, _, _) => {
            let names: Vec<&str> = PROVIDERS.iter().map(|(p, _)| *p).collect();
            return Err(format!(
                "unknown provider {provider:?}: one of {}, or set base_url and api_key_env under [agent] for an OpenAI-compatible endpoint",
                names.join(", ")
            ));
        }
    };
    Ok(Model {
        provider: provider.to_owned(),
        id: id.to_owned(),
        base_url: raw.base_url.clone(),
        key_env,
    })
}

/// `name@1.2.3` (`@scope/name@1.2.3`) at an exact version, or a path
/// (`./x`, `../x`, `/x`) relative to `dir`.
fn source(spec: &str, dir: &Path) -> Result<Source, String> {
    if spec.starts_with("./") || spec.starts_with("../") || spec.starts_with('/') {
        return Ok(Source::Path(dir.join(spec)));
    }
    let at = spec.rfind('@').filter(|at| *at > 0);
    let (name, version) = match at {
        Some(at) => (&spec[..at], &spec[at + 1..]),
        None => (spec, ""),
    };
    let exact = {
        let core = version.split(['-', '+']).next().unwrap_or("");
        let parts: Vec<&str> = core.split('.').collect();
        parts.len() == 3
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
    };
    if name.is_empty() || !exact {
        return Err(format!(
            "{spec:?} is not pinned: give an exact version (`{}@1.2.3`), or a path (`./…`)",
            if name.is_empty() { spec } else { name }
        ));
    }
    Ok(Source::Package {
        name: name.to_owned(),
        version: version.to_owned(),
    })
}

type Located = (Option<Range<usize>>, String);

fn plugin(raw: Spanned<RawPlugin>, dir: &Path) -> Result<Plugin, Located> {
    let table = raw.span();
    let raw = raw.into_inner();
    let config = raw
        .config
        .map(|c| serde_json::to_value(c).expect("TOML is JSON"));
    let given: Vec<&Spanned<String>> = [&raw.pi, &raw.dsh, &raw.rutis]
        .into_iter()
        .flatten()
        .collect();
    let [spec] = given.as_slice() else {
        let span = given.get(1).map_or(table, |s| s.span());
        return Err((
            Some(span),
            "a [[plugin]] names exactly one of pi, dsh or rutis".into(),
        ));
    };
    let span = Some(spec.span());
    if raw.runtime.is_some() && raw.rutis.is_none() {
        return Err((span, "runtime is for a rutis plugin".into()));
    }
    if let Some(rutis) = &raw.rutis {
        let Source::Path(path) = source(rutis.get_ref(), dir).map_err(|m| (span.clone(), m))?
        else {
            return Err((span, "a rutis plugin is a local file (`./…`)".into()));
        };
        let runtime = raw.runtime.ok_or((
            span,
            "a rutis plugin needs a runtime: node, bun, python or go".into(),
        ))?;
        return Ok(Plugin::Rutis {
            path,
            runtime,
            config,
        });
    }
    let source = source(spec.get_ref(), dir).map_err(|m| (span, m))?;
    Ok(match raw.pi {
        Some(_) => Plugin::Pi { source, config },
        None => Plugin::Dsh { source, config },
    })
}

/// A header or variable that carries a secret: its value must come from the
/// environment (`${NAME}`), never be written into the manifest.
fn secret(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "authorization", "token", "key", "secret", "password", "cookie",
    ]
    .iter()
    .any(|s| name.contains(s))
}

fn values(raw: BTreeMap<String, Spanned<String>>) -> Result<BTreeMap<String, Value>, Located> {
    raw.into_iter()
        .map(|(name, value)| {
            let span = Some(value.span());
            let value = Value(value.into_inner());
            if secret(&name) && !value.references() {
                return Err((span, format!("{name} looks like a secret: take it from the environment (`\"${{NAME}}\"`), never write it here")));
            }
            Ok((name, value))
        })
        .collect()
}

fn mcp(raw: Spanned<RawMcp>, dir: &Path) -> Result<Mcp, Located> {
    let table = raw.span();
    let raw = raw.into_inner();
    match (raw.npm, raw.url) {
        (Some(npm), None) => {
            if !raw.headers.is_empty() {
                return Err((
                    Some(npm.span()),
                    "headers are for an HTTP server (url)".into(),
                ));
            }
            let span = Some(npm.span());
            let package = source(npm.get_ref(), dir).map_err(|m| (span, m))?;
            Ok(Mcp::Stdio {
                package,
                args: raw.args,
                env: values(raw.env)?,
            })
        }
        (None, Some(url)) => {
            let span = Some(url.span());
            if !raw.args.is_empty() || !raw.env.is_empty() {
                return Err((span, "args and env are for a stdio server (npm)".into()));
            }
            let url = url.into_inner();
            if !(url.starts_with("https://")
                || url.starts_with("http://localhost")
                || url.starts_with("http://127.0.0.1"))
            {
                return Err((
                    span,
                    format!("{url:?}: an MCP server over https (or http on this machine)"),
                ));
            }
            Ok(Mcp::Http {
                url,
                headers: values(raw.headers)?,
            })
        }
        (_, url) => {
            let span = url.map_or(table, |u| u.span());
            Err((
                Some(span),
                "an [[mcp]] names exactly one of npm (stdio) or url (HTTP)".into(),
            ))
        }
    }
}

fn parts(raw: RawParts) -> Result<Parts, (Range<usize>, String)> {
    let report = match raw.report {
        None => None,
        Some(report) => {
            let span = report.span();
            let sections = match report.into_inner() {
                toml::Value::String(name) if name == "handoff" => {
                    Report::handoff().sections().to_vec()
                }
                toml::Value::Array(items) => items
                    .into_iter()
                    .map(|i| match i {
                        toml::Value::String(s) if !s.trim().is_empty() => Ok(s),
                        _ => Err((
                            span.clone(),
                            "report sections are non-empty strings".to_owned(),
                        )),
                    })
                    .collect::<Result<_, _>>()?,
                _ => {
                    return Err((
                        span,
                        "report is \"handoff\" or a list of section names".into(),
                    ))
                }
            };
            Some(sections)
        }
    };
    let gate = match raw.gate {
        None => true,
        Some(gate) => match gate.get_ref().as_str() {
            "allow-list" => true,
            "off" => false,
            other => {
                return Err((
                    gate.span(),
                    format!("gate {other:?}: \"allow-list\" or \"off\""),
                ))
            }
        },
    };
    Ok(Parts {
        ask_owner: raw.ask_owner,
        report,
        facts: raw.facts,
        gate,
        scratch: raw.scratch,
    })
}

#[cfg(test)]
mod tests;
