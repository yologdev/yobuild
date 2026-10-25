//! `find_plugin` (`[extend]`, design §14): the agent searches for a plugin
//! it lacks. Only the first kind so far, MCP stdio servers, from the MCP
//! registry and npm's search. It starts, loads and installs nothing; what
//! it returns is the packages' own words, each made one line and cut. The
//! session keeps each candidate it returned, for `load_plugin` (design
//! §14; not built yet) to take only those.

use std::collections::{BTreeMap, BTreeSet};
use std::env::VarError;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::future::join_all;
use futures::stream::{self, StreamExt};
use serde_json::{json, Value as Json};
use yoagent::{AgentTool, Content, ToolContext, ToolError, ToolResult};

use crate::frontend::{NoticeLevel, Session};
use crate::manifest::{Extend, Package};
use crate::run::host::clip;

/// Each source's time, and each `npm view`'s.
const SOURCE_TIME: Duration = Duration::from_secs(15);
/// npm's downloads API, asked only for npmjs's own registry.
const DOWNLOADS_TIME: Duration = Duration::from_secs(5);
const MAX_CANDIDATES: usize = 10;
/// Candidates npm is asked about at once (each up to two `npm view`s).
const VIEWS_AT_ONCE: usize = 4;
/// The servers one MCP registry search returns.
const REGISTRY_PAGE: usize = 30;
/// The MCP registry yobuild searches: its `v0` API.
pub const MCP_REGISTRY: &str = "https://registry.modelcontextprotocol.io";
/// Another MCP registry, for the tests: https, or http on this machine.
pub const MCP_REGISTRY_ENV: &str = "YOBUILD_MCP_REGISTRY";
const NPMJS: &str = "https://registry.npmjs.org";
const DOWNLOADS: &str = "https://api.npmjs.org/downloads/point/last-week/";
const DESCRIPTION_CHARS: usize = 200;
const FIELD_CHARS: usize = 120;
/// Words that find every server, left out of the MCP registry's search.
const PLAIN_WORDS: [&str; 8] = ["mcp", "server", "the", "and", "for", "with", "from", "tool"];
/// What may follow a word for a registry find to name it (`linting`).
const ENDINGS: [&str; 6] = ["", "s", "er", "ers", "ing", "ed"];
const DAY: i64 = 24 * 60 * 60;

/// Where `find_plugin` searches: the MCP registry (its URL), and npm's
/// search through the `npm` CLI, with the npm settings `install` keeps
/// (the registry and its login), so a mirror or a private registry holds.
#[derive(Debug, Clone)]
pub struct Sources {
    registry: url::Url,
}

impl Sources {
    /// The MCP registry from [`MCP_REGISTRY_ENV`] when set and not empty,
    /// else [`MCP_REGISTRY`]; one that is not UTF-8 refused.
    pub fn from_env() -> Result<Sources, String> {
        match std::env::var(MCP_REGISTRY_ENV) {
            Ok(url) if !url.is_empty() => Self::at(&url),
            Ok(_) | Err(VarError::NotPresent) => Self::at(MCP_REGISTRY),
            Err(VarError::NotUnicode(_)) => Err(format!("{MCP_REGISTRY_ENV} is not UTF-8")),
        }
    }

    /// The MCP registry at `url`: https, or http on this machine. Its path
    /// is kept: `v0/servers` goes below it.
    pub fn at(url: &str) -> Result<Sources, String> {
        let mut registry =
            crate::manifest::endpoint(url).map_err(|m| format!("{MCP_REGISTRY_ENV} {m}"))?;
        if !registry.path().ends_with('/') {
            let path = format!("{}/", registry.path());
            registry.set_path(&path);
        }
        Ok(Sources { registry })
    }
}

/// A server `find_plugin` returned, as the session keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Candidate {
    pub package: String,
    /// The version `load_plugin` would take — the newest release, not
    /// past `latest`, published at least `min_age_days` ago — or why none.
    pub version: Result<String, String>,
    /// Its name in the MCP registry, when found there.
    pub registry_name: Option<String>,
    /// Found by npm's search.
    pub from_npm: bool,
    pub description: String,
    pub licence: Option<String>,
    /// As the package claims it.
    pub repository: Option<String>,
    /// When `version` was published (`YYYY-MM-DD`).
    pub published: Option<String>,
    /// Last week's, from npm's downloads API; or why not known (npm's
    /// registry here is not npmjs's, or the API failed).
    pub downloads: Result<u64, String>,
    /// Whether `version` has a provenance attestation published with it
    /// (`dist.attestations`); `None` when npm gave no `dist`. Published,
    /// not verified: neither yobuild nor `npm install` checks it (`npm
    /// audit signatures` does, when someone runs it).
    pub provenance: Option<bool>,
    /// What the MCP registry lists as required; `None` when not in it.
    pub required_env: Option<Vec<String>>,
    pub required_args: Option<Vec<String>>,
}

/// The `find_plugin` tool. Clones share what the session found.
#[derive(Clone)]
pub struct FindPlugin {
    session: Session,
    extend: Extend,
    sources: Sources,
    found: Arc<Mutex<Vec<Candidate>>>,
}

impl FindPlugin {
    pub const TOOL: &'static str = "find_plugin";

    pub fn new(session: Session, extend: Extend, sources: Sources) -> Self {
        Self {
            session,
            extend,
            sources,
            found: Arc::default(),
        }
    }

    /// Every candidate returned this session: of each package the latest
    /// with a version to take, else its latest.
    pub fn candidates(&self) -> Vec<Candidate> {
        self.found.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    async fn find(&self, need: &str) -> Found {
        let (registry, npm, npm_registry) = tokio::join!(
            registry(&self.sources.registry, need),
            npm_search(need),
            blocking(vec!["config".into(), "get".into(), "registry".into()]),
        );
        let mut lines = Vec::new();
        let mut failed = Vec::new();
        let registry = match registry {
            Ok(found) => {
                lines.push(found.line());
                found.hits
            }
            Err(e) => {
                lines.push(format!("MCP registry: failed ({e})"));
                failed.push(format!("MCP registry: {e}"));
                Vec::new()
            }
        };
        let npm = match npm {
            Ok(found) => {
                lines.push(found.line());
                found.hits
            }
            Err(e) => {
                lines.push(format!("npm: failed ({e})"));
                failed.push(format!("npm: {e}"));
                Vec::new()
            }
        };
        let (hits, total) = merged(registry, npm);
        let downloads = match npm_registry {
            Ok(r) if is_npmjs(&r) => Ok(()),
            Ok(r) => Err(format!(
                "not asked: npm's registry here is {}, not npmjs's",
                clip(&r, FIELD_CHARS)
            )),
            Err(e) => Err(format!("not asked: npm's registry could not be read ({e})")),
        };
        let now = now();
        let viewed: Vec<(Candidate, Option<String>)> = stream::iter(hits)
            .map(|hit| candidate(hit, &self.extend, now, &downloads))
            .buffered(VIEWS_AT_ONCE)
            .collect()
            .await;
        let unviewed = match viewed.iter().all(|(_, e)| e.is_some()) {
            true => viewed.first().and_then(|(_, e)| e.clone()),
            false => None,
        };
        Found {
            lines,
            failed,
            candidates: viewed.into_iter().map(|(c, _)| c).collect(),
            total,
            unviewed,
        }
    }

    /// Keeps `candidates`, each in place of its package's, but for one
    /// with a version to take, which only another such replaces (npm
    /// failing once does not lose it).
    fn remember(&self, candidates: &[Candidate]) {
        let mut found = self.found.lock().unwrap_or_else(|e| e.into_inner());
        for candidate in candidates {
            match found.iter_mut().find(|c| c.package == candidate.package) {
                Some(old) if old.version.is_ok() && candidate.version.is_err() => {}
                Some(old) => *old = candidate.clone(),
                None => found.push(candidate.clone()),
            }
        }
    }
}

/// One search's results: a line per source, the sources that failed, the
/// candidates.
struct Found {
    lines: Vec<String>,
    /// Each source that failed, with why.
    failed: Vec<String>,
    candidates: Vec<Candidate>,
    /// The packages the sources found, before the cut to
    /// [`MAX_CANDIDATES`].
    total: usize,
    /// npm could be asked about none of the candidates: why (the first's).
    unviewed: Option<String>,
}

/// Whether `registry` (npm's setting) is npmjs's own.
fn is_npmjs(registry: &str) -> bool {
    registry.trim().trim_end_matches('/') == NPMJS
}

/// A package a source named, before npm is asked about it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Hit {
    package: String,
    registry_name: Option<String>,
    from_npm: bool,
    description: String,
    required_env: Option<Vec<String>>,
    required_args: Option<Vec<String>>,
}

/// What the MCP registry's searches for a need found, each server counted
/// once however many of the searches returned it: those with an npm
/// package over stdio, and those left out, by why.
#[derive(Debug, Default, PartialEq, Eq)]
struct RegistryFound {
    hits: Vec<Hit>,
    remote: usize,
    other: usize,
    /// Returned, but naming none of the need's words (the registry
    /// matches any part of a name).
    unrelated: usize,
    /// A search had more servers than the page it returned.
    more: bool,
    searches: usize,
    /// The searches that failed, with why (each reason once).
    failed: Vec<String>,
}

impl RegistryFound {
    fn line(&self) -> String {
        let mut line = format!("MCP registry: {} found", self.hits.len());
        let mut out = Vec::new();
        if self.remote > 0 {
            out.push(format!("{} remote only", self.remote));
        }
        if self.other > 0 {
            out.push(format!("{} not an npm package over stdio", self.other));
        }
        if self.unrelated > 0 {
            out.push(format!(
                "{} naming none of the need's words",
                self.unrelated
            ));
        }
        if !out.is_empty() {
            line.push_str(&format!(
                " ({} left out: {})",
                self.remote + self.other + self.unrelated,
                out.join(", ")
            ));
        }
        if self.more {
            line.push_str(&format!(
                "; a search matched more than the {REGISTRY_PAGE} it returns"
            ));
        }
        if !self.failed.is_empty() {
            line.push_str(&format!(
                "; {} of its {} searches failed ({})",
                self.failed.len(),
                self.searches,
                distinct(&self.failed).join("; ")
            ));
        }
        line
    }
}

/// Each of `texts` once, in order.
fn distinct(texts: &[String]) -> Vec<&str> {
    let mut seen: Vec<&str> = Vec::new();
    for text in texts {
        if !seen.contains(&text.as_str()) {
            seen.push(text);
        }
    }
    seen
}

/// The two sources' hits as one list of at most [`MAX_CANDIDATES`]: those
/// both found first (the registry's, which lists what they need), then
/// each source's own in turn, each in its own order; and how many there
/// were before the cut.
fn merged(registry: Vec<Hit>, npm: Vec<Hit>) -> (Vec<Hit>, usize) {
    let in_npm = |hit: &Hit| npm.iter().any(|n| n.package == hit.package);
    let (both, registry): (Vec<Hit>, Vec<Hit>) = registry.into_iter().partition(|h| in_npm(h));
    let mut hits: Vec<Hit> = both
        .into_iter()
        .map(|hit| Hit {
            from_npm: true,
            ..hit
        })
        .collect();
    let npm: Vec<Hit> = npm
        .into_iter()
        .filter(|n| !hits.iter().any(|h| h.package == n.package))
        .collect();
    let (mut registry, mut npm) = (registry.into_iter(), npm.into_iter());
    loop {
        let (r, n) = (registry.next(), npm.next());
        if r.is_none() && n.is_none() {
            break;
        }
        hits.extend(r.into_iter().chain(n));
    }
    let mut seen = Vec::new();
    hits.retain(|h| match seen.contains(&h.package) {
        true => false,
        false => {
            seen.push(h.package.clone());
            true
        }
    });
    let total = hits.len();
    hits.truncate(MAX_CANDIDATES);
    (hits, total)
}

/// Whether `text` (a server's name, package and description) names `word`
/// as a word, or with one of [`ENDINGS`] after it: the registry matches
/// any part of a name, `lint` in `maxwellintl` too. A word of several
/// parts (`lint-staged`) must be those parts in a row (`lint_staged`,
/// `lint staged`), the ending on the last.
fn mentions(text: &str, word: &str) -> bool {
    let words = |text: &str| -> Vec<String> {
        text.split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect()
    };
    let (tokens, parts) = (words(text), words(word));
    let Some((last, first)) = parts.split_last() else {
        return false;
    };
    tokens.windows(parts.len()).any(|run| {
        run[..first.len()] == *first
            && run[first.len()]
                .strip_prefix(last.as_str())
                .is_some_and(|rest| ENDINGS.contains(&rest))
    })
}

/// The words of `need` the MCP registry is searched for, a search each:
/// it matches a search against any part of a server's name, so a need of
/// several words, searched whole, would rarely match. Words are split at
/// what is not a letter, digit or `-` (`lint-staged` is one); up to four
/// of three letters or more that are not [`PLAIN_WORDS`], else (a need of
/// only such words, `mcp server`) its first four as they are. Never empty
/// for a need with a letter or digit.
fn registry_words(need: &str) -> Vec<String> {
    let mut all: Vec<String> = Vec::new();
    for word in need
        .to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '-'))
    {
        let word = word.trim_matches('-');
        if word.chars().any(char::is_alphanumeric) && !all.iter().any(|w| w == word) {
            all.push(word.to_owned());
        }
    }
    let mut words: Vec<String> = all
        .iter()
        .filter(|w| w.chars().count() >= 3 && !PLAIN_WORDS.contains(&w.as_str()))
        .cloned()
        .collect();
    if words.is_empty() {
        words = all;
    }
    words.truncate(4);
    words
}

async fn registry(base: &url::Url, need: &str) -> Result<RegistryFound, String> {
    let client = reqwest::Client::builder()
        .timeout(SOURCE_TIME)
        .user_agent(concat!("yobuild/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;
    let words = registry_words(need);
    let answers = join_all(words.iter().map(|word| {
        let client = client.clone();
        async move {
            let mut url = base.join("v0/servers").map_err(|e| e.to_string())?;
            url.query_pairs_mut()
                .append_pair("search", word)
                .append_pair("version", "latest")
                .append_pair("limit", &REGISTRY_PAGE.to_string());
            let response = client.get(url).send().await.map_err(|e| said(&e))?;
            let status = response.status();
            if !status.is_success() {
                return Err(format!("HTTP {status}"));
            }
            let body: Json = response.json().await.map_err(|e| said(&e))?;
            registry_page(&body)
        }
    }))
    .await;
    ranked(&words, answers)
}

/// The searches' pages as one: servers found by more of the words first,
/// then in the order found; each left-out server counted once. Every
/// search failing is an error (each reason once).
fn ranked(words: &[String], pages: Vec<Result<Page, String>>) -> Result<RegistryFound, String> {
    let mut ranked: Vec<(usize, Hit)> = Vec::new();
    let mut found = RegistryFound {
        searches: pages.len(),
        ..RegistryFound::default()
    };
    let (mut remote, mut other, mut unrelated) =
        (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
    for page in pages {
        let page = match page {
            Ok(page) => page,
            Err(e) => {
                found.failed.push(e);
                continue;
            }
        };
        found.more |= page.more;
        for listed in page.servers {
            if !words.iter().any(|w| mentions(&listed.text, w)) {
                unrelated.insert(listed.key);
                continue;
            }
            match listed.hit {
                Some(hit) => match ranked.iter_mut().find(|(_, h)| h.package == hit.package) {
                    Some((n, _)) => *n += 1,
                    None => ranked.push((1, hit)),
                },
                None if listed.remote => _ = remote.insert(listed.key),
                None => _ = other.insert(listed.key),
            }
        }
    }
    if found.failed.len() == found.searches {
        return Err(distinct(&found.failed).join("; "));
    }
    (found.remote, found.other, found.unrelated) = (remote.len(), other.len(), unrelated.len());
    ranked.sort_by_key(|(n, _)| std::cmp::Reverse(*n));
    found.hits = ranked.into_iter().map(|(_, h)| h).collect();
    Ok(found)
}

/// A request's error with its causes (reqwest's own `Display` is only the
/// first).
fn said(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(&format!(": {cause}"));
        source = cause.source();
    }
    text
}

/// One MCP registry search's answer.
#[derive(Debug, PartialEq, Eq)]
struct Page {
    servers: Vec<Listed>,
    /// More matched than it returned (a `nextCursor`, or a full page).
    more: bool,
}

/// A server a search returned.
#[derive(Debug, PartialEq, Eq)]
struct Listed {
    /// Its name (else its JSON), to count it once across searches.
    key: String,
    /// Its name, packages and description, for [`mentions`].
    text: String,
    /// When it has an npm package over stdio.
    hit: Option<Hit>,
    /// Else, whether it is remote only.
    remote: bool,
}

/// The MCP registry's answer (`v0`: `{servers: [{server, _meta}],
/// metadata: {nextCursor}}`; an entry that is the server itself read too).
fn registry_page(body: &Json) -> Result<Page, String> {
    let servers = body["servers"]
        .as_array()
        .ok_or("it answered without a `servers` list")?;
    let more = servers.len() >= REGISTRY_PAGE
        || body["metadata"]["nextCursor"]
            .as_str()
            .is_some_and(|c| !c.is_empty());
    let mut page = Page {
        servers: Vec::new(),
        more,
    };
    for entry in servers {
        let server = entry.get("server").unwrap_or(entry);
        let empty = Vec::new();
        let packages = server["packages"].as_array().unwrap_or(&empty);
        let key = match server["name"].as_str() {
            Some(name) => name.to_owned(),
            None => server.to_string(),
        };
        let words = std::iter::once(text(&server["name"]))
            .chain(packages.iter().map(|p| text(&p["identifier"])))
            .chain([text(&server["description"])])
            .collect::<Vec<_>>()
            .join(" ");
        let stdio = packages.iter().find(|p| {
            p["registryType"] == "npm"
                && p["transport"]["type"] == "stdio"
                && p["identifier"]
                    .as_str()
                    .is_some_and(|id| Package::new(id, "1.0.0").is_ok())
        });
        let Some(package) = stdio else {
            let remote =
                packages.is_empty() && server["remotes"].as_array().is_some_and(|r| !r.is_empty());
            page.servers.push(Listed {
                key,
                text: words,
                hit: None,
                remote,
            });
            continue;
        };
        let required = |key: &str, name: &dyn Fn(&Json) -> String| -> Vec<String> {
            package[key]
                .as_array()
                .unwrap_or(&empty)
                .iter()
                .filter(|v| v["isRequired"] == true)
                .map(|v| clip(&name(v), FIELD_CHARS))
                .collect()
        };
        let hit = Hit {
            package: package["identifier"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            registry_name: server["name"].as_str().map(|n| clip(n, FIELD_CHARS)),
            from_npm: false,
            description: text(&server["description"]),
            required_env: Some(required("environmentVariables", &|v| text(&v["name"]))),
            required_args: Some(required("packageArguments", &|v| {
                let name = text(&v["name"]);
                match name.is_empty() {
                    true => match text(&v["valueHint"]) {
                        hint if hint.is_empty() => "a positional argument".into(),
                        hint => format!("<{hint}>"),
                    },
                    false => name,
                }
            })),
        };
        page.servers.push(Listed {
            key,
            text: words,
            hit: Some(hit),
            remote: false,
        });
    }
    Ok(page)
}

/// A JSON string as it is, anything else empty.
fn text(value: &Json) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

/// What npm's search found.
#[derive(Debug, PartialEq, Eq)]
struct NpmFound {
    hits: Vec<Hit>,
    /// Packages under names yobuild does not take (npm's old forms).
    unnamed: usize,
}

impl NpmFound {
    fn line(&self) -> String {
        let mut line = format!("npm: {} found", self.hits.len());
        if self.unnamed > 0 {
            line.push_str(&format!(
                " ({} left out: a name yobuild does not take)",
                self.unnamed
            ));
        }
        line
    }
}

async fn npm_search(need: &str) -> Result<NpmFound, String> {
    let mut args: Vec<String> = ["search", "--json", "--searchlimit=20", "--", "keywords:mcp"]
        .map(String::from)
        .into();
    args.extend(need.split_whitespace().map(String::from));
    npm_hits(&blocking(args).await?)
}

/// `npm search --json`'s answer: a list of packages, each with its name.
fn npm_hits(stdout: &str) -> Result<NpmFound, String> {
    if stdout.trim().is_empty() {
        return Err("npm search answered nothing".into());
    }
    let list: Json = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("npm search answered what is not JSON ({e})"))?;
    let list = list
        .as_array()
        .ok_or("npm search answered without a list")?;
    let mut found = NpmFound {
        hits: Vec::new(),
        unnamed: 0,
    };
    for p in list {
        let name = p["name"]
            .as_str()
            .ok_or("npm search answered a package without a `name` (has its output changed?)")?;
        if Package::new(name, "1.0.0").is_err() {
            found.unnamed += 1;
            continue;
        }
        found.hits.push(Hit {
            package: name.to_owned(),
            registry_name: None,
            from_npm: true,
            description: text(&p["description"]),
            required_env: None,
            required_args: None,
        });
    }
    Ok(found)
}

/// `args` with `--no-update-notifier`, before any `--` (after it, npm
/// would take it for an argument: a search's words).
fn quiet(mut args: Vec<String>) -> Vec<String> {
    let at = args.iter().position(|a| a == "--").unwrap_or(args.len());
    args.insert(at, "--no-update-notifier".into());
    args
}

/// npm with `args` [`quiet`], in a folder of its own (no project's
/// `.npmrc`), with [`SOURCE_TIME`], off the async threads.
async fn blocking(args: Vec<String>) -> Result<String, String> {
    let args = quiet(args);
    tokio::task::spawn_blocking(move || {
        let dir = tempfile::tempdir().map_err(|e| format!("a folder for npm: {e}"))?;
        crate::install::npm_within(dir.path(), &args, SOURCE_TIME).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("npm's task failed: {e}"))?
}

/// What `npm view <package> --json` says of a package (its latest version)
/// or of one version.
#[derive(Debug, Default, PartialEq, Eq)]
struct Viewed {
    version: Option<String>,
    latest: Option<String>,
    /// `None` when npm gave no `versions`.
    versions: Option<Vec<String>>,
    /// Each version's publish time as npm gives it (not text: its JSON).
    time: BTreeMap<String, String>,
    description: String,
    licence: Option<String>,
    repository: Option<String>,
    /// `None` when npm gave no `dist`.
    provenance: Option<bool>,
}

fn viewed(stdout: &str) -> Result<Viewed, String> {
    let view: Json = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("npm view answered what is not JSON ({e})"))?;
    if !view.is_object() {
        return Err("npm view answered what is not an object".into());
    }
    let versions = match &view["versions"] {
        Json::Array(all) => Some(
            all.iter()
                .map(|v| v.as_str().map(String::from))
                .collect::<Option<Vec<_>>>()
                .ok_or("npm view's `versions` holds what is not text")?,
        ),
        Json::String(one) => Some(vec![one.clone()]),
        Json::Null => None,
        _ => return Err("npm view's `versions` is neither a list nor a text".into()),
    };
    let time = match &view["time"] {
        Json::Object(t) => t
            .iter()
            .map(|(k, v)| {
                let at = v.as_str().map_or_else(|| v.to_string(), String::from);
                (k.clone(), at)
            })
            .collect(),
        Json::Null => BTreeMap::new(),
        _ => return Err("npm view's `time` is not an object".into()),
    };
    let licence = licence(&view["license"]).or_else(|| licence(&view["licenses"]));
    let repository = match &view["repository"] {
        Json::String(r) => Some(r.clone()),
        Json::Object(r) => r.get("url").and_then(Json::as_str).map(String::from),
        _ => None,
    }
    .map(|r| {
        let r = r.strip_prefix("git+").unwrap_or(&r);
        r.strip_suffix(".git").unwrap_or(r).to_owned()
    });
    Ok(Viewed {
        version: view["version"].as_str().map(String::from),
        latest: view["dist-tags"]["latest"].as_str().map(String::from),
        versions,
        time,
        description: text(&view["description"]),
        licence,
        repository,
        provenance: view["dist"]
            .as_object()
            .map(|d| d.get("attestations").is_some_and(Json::is_object)),
    })
}

/// A licence as `license` gives it (`"MIT"`, `{type}`), or as a list of
/// those (`licenses`, an older form), joined with `OR`.
fn licence(value: &Json) -> Option<String> {
    match value {
        Json::String(l) => Some(l.clone()),
        Json::Object(l) => l.get("type").and_then(Json::as_str).map(String::from),
        Json::Array(all) => {
            let all: Vec<String> = all.iter().filter_map(licence).collect();
            (!all.is_empty()).then(|| all.join(" OR "))
        }
        _ => None,
    }
}

/// A release version's numbers: `a.b.c` only (no pre-release or build).
fn release(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split('.');
    let mut number = || -> Option<u64> {
        let part = parts.next()?;
        let plain = !part.is_empty()
            && part.bytes().all(|b| b.is_ascii_digit())
            && (part == "0" || !part.starts_with('0'));
        plain.then(|| part.parse().ok()).flatten()
    };
    let numbers = (number()?, number()?, number()?);
    parts.next().is_none().then_some(numbers)
}

/// A `latest` tag's numbers, and whether a release of those numbers is
/// within it: `1.2.3` and `1.2.3+build` are; before `1.2.3-rc.1` comes
/// 1.2.2, not 1.2.3. `None` when it is not a version.
fn latest_bound(latest: &str) -> Option<((u64, u64, u64), bool)> {
    let core = latest.split_once('+').map_or(latest, |(core, _)| core);
    let (numbers, pre) = match core.split_once('-') {
        Some((numbers, pre)) => (numbers, Some(pre)),
        None => (core, None),
    };
    if pre.is_some_and(str::is_empty) {
        return None;
    }
    Some((release(numbers)?, pre.is_none()))
}

/// The version to take of a package: the newest release not past
/// `latest`, published at least `min_age_days` before `now` (seconds); or
/// why none. With 0 days a publish time is not needed.
fn version_to_take(view: &Viewed, min_age_days: u32, now: i64) -> Result<String, String> {
    let Some(versions) = &view.versions else {
        return Err("npm gives no list of its versions".into());
    };
    let Some(latest) = view.latest.as_deref() else {
        return Err("it has no `latest` tag, so the version to take cannot be told".into());
    };
    let Some((bound, within)) = latest_bound(latest) else {
        return Err(format!(
            "its `latest` tag, {:?}, is not a version",
            clip(latest, FIELD_CHARS)
        ));
    };
    let mut releases: Vec<(&String, (u64, u64, u64))> = versions
        .iter()
        .filter_map(|v| Some((v, release(v)?)))
        .filter(|(_, n)| *n < bound || (within && *n == bound))
        .collect();
    releases.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    let Some((newest, _)) = releases.first() else {
        return Err("it has no release version (only pre-releases, or none)".into());
    };
    if min_age_days == 0 {
        return Ok((*newest).clone());
    }
    let enough = now - i64::from(min_age_days) * DAY;
    for (version, _) in &releases {
        let Some(at) = view.time.get(*version) else {
            return Err(format!(
                "npm gives no publish time for {version}, so its age cannot be told"
            ));
        };
        let Some(at) = seconds(at) else {
            let at = clip(at, 40);
            return Err(format!(
                "npm's publish time for {version}, {at:?}, cannot be read, so its age cannot be told"
            ));
        };
        if at <= enough {
            return Ok((*version).clone());
        }
    }
    let when = view
        .time
        .get(*newest)
        .and_then(|t| seconds(t))
        .map_or_else(String::new, |t| format!(", on {}", day(t)));
    Err(format!(
        "no version was published {min_age_days} or more days ago (the newest, {newest}{when})"
    ))
}

/// Seconds since 1970 of an ISO 8601 time in UTC as npm writes them
/// (`2026-10-16T17:04:59.463Z`).
fn seconds(text: &str) -> Option<i64> {
    let (date, time) = text.strip_suffix('Z')?.split_once('T')?;
    let mut d = date.splitn(3, '-').map(|p| p.parse::<i64>().ok());
    let (y, m, dd) = (d.next()??, d.next()??, d.next()??);
    let time = time.split('.').next()?;
    let mut t = time.splitn(3, ':').map(|p| p.parse::<i64>().ok());
    let (h, mi, s) = (t.next()??, t.next()??, t.next()??);
    if !(1..=12).contains(&m) || !(1..=31).contains(&dd) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    // Days from 1970-01-01 (Howard Hinnant's `days_from_civil`).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + dd - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * DAY + h * 3600 + mi * 60 + s)
}

/// `YYYY-MM-DD` of `seconds` since 1970 (`civil_from_days`).
fn day(seconds: i64) -> String {
    let z = seconds.div_euclid(DAY) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Seconds since 1970; `None` for a clock that reads before it.
fn now() -> Option<i64> {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(since.as_secs()).ok()
}

/// A hit made a candidate: npm asked about its versions, and about the one
/// it would take when that is not the latest; its downloads unless
/// `downloads` says why not. Beside it, why npm could not be asked about
/// it at all.
async fn candidate(
    hit: Hit,
    extend: &Extend,
    now: Option<i64>,
    downloads: &Result<(), String>,
) -> (Candidate, Option<String>) {
    let mut candidate = Candidate {
        package: hit.package.clone(),
        version: Err(String::new()),
        registry_name: hit.registry_name,
        from_npm: hit.from_npm,
        description: clip(&hit.description, DESCRIPTION_CHARS),
        licence: None,
        repository: None,
        published: None,
        downloads: Err(String::new()),
        provenance: None,
        required_env: hit.required_env,
        required_args: hit.required_args,
    };
    let view = |spec: String| async move {
        let args = vec!["view".into(), spec, "--json".into()];
        viewed(&blocking(args).await?)
    };
    let (latest, counted) = tokio::join!(view(hit.package.clone()), async {
        match downloads {
            Ok(()) => weekly_downloads(&hit.package)
                .await
                .map_err(|e| format!("npm's downloads API failed ({e})")),
            Err(why) => Err(why.clone()),
        }
    });
    candidate.downloads = counted;
    let latest = match latest {
        Ok(latest) => latest,
        Err(e) => {
            candidate.version = Err(format!("npm could not say what it is: {e}"));
            return (candidate, Some(e));
        }
    };
    if candidate.description.is_empty() {
        candidate.description = clip(&latest.description, DESCRIPTION_CHARS);
    }
    let days = extend.min_age_days;
    candidate.version = match now {
        Some(now) => version_to_take(&latest, days, now),
        None if days == 0 => version_to_take(&latest, 0, 0),
        None => {
            Err("this machine's clock reads before 1970, so no version's age can be told".into())
        }
    };
    let Ok(version) = candidate.version.clone() else {
        return (candidate, None);
    };
    candidate.published = latest.time.get(&version).and_then(|t| seconds(t)).map(day);
    let this = match latest.version.as_deref() == Some(version.as_str()) {
        true => Ok(latest),
        false => view(format!("{}@{version}", hit.package)).await,
    };
    match this {
        Ok(this) => {
            candidate.licence = this.licence.map(|l| clip(&l, FIELD_CHARS));
            candidate.repository = this.repository.map(|r| clip(&r, FIELD_CHARS));
            candidate.provenance = this.provenance;
        }
        Err(e) => candidate.version = Err(format!("npm could not say what {version} is: {e}")),
    }
    (candidate, None)
}

/// Last week's downloads from npm's API.
async fn weekly_downloads(package: &str) -> Result<u64, String> {
    let client = reqwest::Client::builder()
        .timeout(DOWNLOADS_TIME)
        .user_agent(concat!("yobuild/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| said(&e))?;
    let response = client
        .get(format!("{DOWNLOADS}{package}"))
        .send()
        .await
        .map_err(|e| said(&e))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("HTTP {status}"));
    }
    let body: Json = response.json().await.map_err(|e| said(&e))?;
    body["downloads"]
        .as_u64()
        .ok_or_else(|| "it answered without a count".into())
}

/// The result's text: the sources' lines, then each candidate.
fn said_to_agent(need: &str, found: &Found, extend: &Extend) -> String {
    let mut text = format!(
        "find_plugin({need:?}): what the sources say. Nothing was installed or started.\nSources: {}.\n",
        found.lines.join("; ")
    );
    if found.candidates.is_empty() {
        text.push_str("No candidates.");
        return text;
    }
    let cut = match found.total > found.candidates.len() {
        true => format!(
            "the first {} of {} found; ",
            found.candidates.len(),
            found.total
        ),
        false => String::new(),
    };
    text.push_str(&format!(
        "Candidates ({cut}their descriptions, licences and repositories are their authors' own words, not verified; a version is taken only once published {} or more days ago):\n",
        extend.min_age_days
    ));
    for (i, c) in found.candidates.iter().enumerate() {
        let mut from = Vec::new();
        if let Some(name) = &c.registry_name {
            from.push(format!("the MCP registry as {name}"));
        }
        if c.from_npm {
            from.push("npm's search".into());
        }
        let head = match &c.version {
            Ok(v) => format!(
                "{}. {} {v} (published {}; found in {})",
                i + 1,
                c.package,
                c.published
                    .as_deref()
                    .unwrap_or("on a date npm does not give"),
                from.join(" and ")
            ),
            Err(why) => format!(
                "{}. {} (found in {}): not loadable: {why}",
                i + 1,
                c.package,
                from.join(" and ")
            ),
        };
        text.push_str(&head);
        text.push('\n');
        if !c.description.is_empty() {
            text.push_str(&format!("   {}\n", c.description));
        }
        if c.version.is_err() {
            continue;
        }
        let downloads = match &c.downloads {
            Ok(n) => n.to_string(),
            Err(why) => format!("unknown ({why})"),
        };
        text.push_str(&format!(
            "   licence {}; downloads last week: {downloads}; repository (claimed by the package): {}; provenance: {}\n",
            c.licence.as_deref().unwrap_or("not given"),
            c.repository.as_deref().unwrap_or("not given"),
            match c.provenance {
                Some(true) => "an attestation is published with it (not verified)",
                Some(false) => "none",
                None => "unknown (npm gave no `dist`)",
            }
        ));
        let listed = |what: &Option<Vec<String>>| match what {
            None => "not listed".to_owned(),
            Some(names) if names.is_empty() => "none".to_owned(),
            Some(names) => names.join(", "),
        };
        text.push_str(&format!(
            "   required arguments: {}; required environment variables: {}\n",
            listed(&c.required_args),
            listed(&c.required_env)
        ));
    }
    text.trim_end().to_owned()
}

fn details(found: &Found) -> Json {
    let candidates: Vec<Json> = found
        .candidates
        .iter()
        .map(|c| {
            json!({
                "package": c.package,
                "version": c.version.as_ref().ok(),
                "notLoadable": c.version.as_ref().err(),
                "registryName": c.registry_name,
                "fromNpm": c.from_npm,
                "description": c.description,
                "licence": c.licence,
                "repository": c.repository,
                "published": c.published,
                "downloads": c.downloads.as_ref().ok(),
                "downloadsUnknown": c.downloads.as_ref().err(),
                "provenance": c.provenance,
                "requiredEnv": c.required_env,
                "requiredArgs": c.required_args,
            })
        })
        .collect();
    json!({ "sources": found.lines, "found": found.total, "candidates": candidates })
}

#[async_trait::async_trait]
impl AgentTool for FindPlugin {
    fn name(&self) -> &str {
        Self::TOOL
    }
    fn label(&self) -> &str {
        "Find a plugin"
    }
    fn description(&self) -> &str {
        "Search for an MCP server that gives a capability you lack (a linter, a file format): \
         the MCP registry and npm. It installs and starts nothing; it returns candidates, the \
         version of each that could be loaded, and what the sources say of them, which is the \
         packages' own claims."
    }
    fn parameters_schema(&self) -> Json {
        json!({
            "type": "object",
            "properties": {
                "need": { "type": "string", "description": "The capability, in a few words: \"lint TypeScript\"." },
                "kind": { "type": "string", "enum": ["mcp"], "description": "What to find: \"mcp\" (the default)." }
            },
            "required": ["need"]
        })
    }

    async fn execute(&self, params: Json, _ctx: ToolContext) -> Result<ToolResult, ToolError> {
        let object = params
            .as_object()
            .ok_or_else(|| ToolError::InvalidArgs("the arguments must be an object".into()))?;
        if let Some(key) = object
            .keys()
            .find(|k| !["need", "kind"].contains(&k.as_str()))
        {
            return Err(ToolError::InvalidArgs(format!(
                "unknown key `{key}` (use need, kind)"
            )));
        }
        let need = match &params["need"] {
            Json::String(need) => clip(need, FIELD_CHARS),
            Json::Null => String::new(),
            other => {
                return Err(ToolError::InvalidArgs(format!(
                    "`need` must be a string, not {}",
                    clip(&other.to_string(), 40)
                )))
            }
        };
        if need.is_empty() {
            return Err(ToolError::InvalidArgs(
                "`need` is required: the capability, in a few words".into(),
            ));
        }
        if !need.chars().any(char::is_alphanumeric) {
            return Err(ToolError::InvalidArgs(
                "`need` holds no word to search for".into(),
            ));
        }
        match &params["kind"] {
            Json::Null => {}
            Json::String(kind) if self.extend.kinds.iter().any(|k| k.name() == kind) => {}
            other => {
                return Err(ToolError::InvalidArgs(format!(
                    "kind {other}: this agent may find {}",
                    self.extend
                        .kinds
                        .iter()
                        .map(|k| format!("{:?}", k.name()))
                        .collect::<Vec<_>>()
                        .join(", ")
                )))
            }
        }
        let found = self.find(&need).await;
        let text = said_to_agent(&need, &found, &self.extend);
        if found.failed.len() == 2 {
            self.session.notice(
                NoticeLevel::Warning,
                format!("find_plugin could not search: {}.", found.failed.join("; ")),
            );
            return Err(ToolError::Failed(format!(
                "{text}\nBoth sources failed: this is not \"nothing found\"."
            )));
        }
        if let Some(why) = &found.unviewed {
            self.session.notice(
                NoticeLevel::Warning,
                format!("find_plugin could not ask npm about any candidate: {why}."),
            );
            return Err(ToolError::Failed(format!(
                "{text}\nnpm could be asked about none of them: this is not \"none loadable\"."
            )));
        }
        if let [failed] = found.failed.as_slice() {
            self.session.notice(
                NoticeLevel::Warning,
                format!("find_plugin searched one source only: {failed}."),
            );
        }
        self.remember(&found.candidates);
        Ok(ToolResult {
            content: vec![Content::Text { text }],
            details: details(&found),
        })
    }
}

#[cfg(test)]
mod tests;
