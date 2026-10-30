//! `find_plugin` (`[extend]`, design §14): the agent searches for a plugin
//! it lacks, of a kind the manifest's `[extend] kinds` names: an MCP stdio
//! server, from the MCP registry and npm's search, a pi or DSH package,
//! from npm's search by its keyword (design §14 "Stage 2", point 8), or a
//! remote MCP server, from the MCP registry's `remotes` ("Stage 3"). It
//! starts, loads and installs nothing; what
//! it returns is the packages' own words, each made one line and cut. The
//! session keeps each candidate it returned, for `load_plugin` (design
//! §14, `run/load.rs`) to take only those.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::env::VarError;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::future::join_all;
use futures::stream::{self, StreamExt};
use serde_json::{json, Value as Json};
use yoagent::{AgentTool, Content, ToolContext, ToolError, ToolResult};

use crate::frontend::{NoticeLevel, Session};
use crate::manifest::{Extend, ExtendKind, Package, Value};
use crate::run::host::clip;

/// The MCP registry's time, each `npm view`'s, and `npm config get
/// registry`'s.
const SOURCE_TIME: Duration = Duration::from_secs(15);
/// npm's search: its first call in a cold cache was seen to take more
/// than 15 s.
const SEARCH_TIME: Duration = Duration::from_secs(30);
/// The hits npm is asked about at most: in rounds, until
/// [`MAX_CANDIDATES`] are kept; one not of the kind (an MCP library, an
/// unmarked pi or DSH package) is left out only once npm says so, making
/// room for the next.
const VIEWED_AT_MOST: usize = 20;
/// Unscoped packages asked for at once in npm's downloads API (its bulk
/// form; a scoped package is asked for alone).
const DOWNLOADS_AT_ONCE: usize = 128;
/// The keywords that mark an npm package as an MCP server's (npm's
/// `keywords:` qualifier ranks by them; it does not filter).
const MCP_KEYWORDS: [&str; 4] = [
    "mcp", "mcp-server", "model-context-protocol", "modelcontextprotocol",
];
/// A request to npm's downloads API (asked only for npmjs's own
/// registry); twice it for a round's requests together.
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
/// The keyword that marks a pi package (pi's own `pi install` convention).
const PI_KEYWORD: &str = "pi-package";
/// The keyword that marks a DSH package.
const DSH_KEYWORD: &str = "dsh-plugin";

/// Where `find_plugin` searches: the MCP registry (its URL; none when
/// `[extend] kinds` names no MCP), and npm's search through the `npm` CLI,
/// with the npm settings `install` keeps (the registry and its login), so
/// a mirror or a private registry holds.
#[derive(Debug, Clone)]
pub struct Sources {
    registry: Option<url::Url>,
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
        Ok(Sources {
            registry: Some(registry),
        })
    }

    /// npm alone: for an agent that finds no MCP server, so a
    /// [`MCP_REGISTRY_ENV`] it would never use refuses nothing.
    pub fn npm_only() -> Sources {
        Sources { registry: None }
    }
}

/// A plugin `find_plugin` returned — an MCP server, a pi or a DSH package,
/// as `kind` says — as the session keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Candidate {
    /// What the agent said it needs, in the search that found it.
    pub need: String,
    /// What it was searched for as.
    pub kind: ExtendKind,
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
    /// A remote server's address and headers: only for `kind` remote
    /// (read it through [`Candidate::remote`]), whose `package` is its MCP
    /// registry name and whose `version` is an `Err`: a remote has none to
    /// take (the entry's is `Remote::version`).
    pub remote: Option<Remote>,
}

impl Candidate {
    /// The remote server it is, when its kind is remote.
    pub fn remote(&self) -> Option<&Remote> {
        self.remote
            .as_ref()
            .filter(|_| self.kind == ExtendKind::Remote)
    }
}

/// Why a remote candidate's `version` is an `Err`.
pub(crate) const NO_VERSION: &str = "a remote server has no version to take";

/// A remote MCP server, as the MCP registry lists it and as yobuild would
/// reach it (design §14 "Stage 3", point 2): its first streamable-HTTP
/// remote whose address [`remote_address`] takes and whose headers yobuild
/// can fill. Made only from the registry's answer, so its address is
/// always one [`remote_address`] took.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Remote {
    url: url::Url,
    headers: Vec<RemoteHeader>,
    others: Vec<String>,
    version: Option<String>,
}

impl Remote {
    /// Its address, as checked.
    pub fn url(&self) -> &url::Url {
        &self.url
    }

    pub fn host(&self) -> &str {
        self.url.host_str().unwrap_or_default()
    }

    pub fn headers(&self) -> &[RemoteHeader] {
        &self.headers
    }

    /// Its other remotes (`<type> <url>`, cut for showing): listed, not
    /// taken.
    pub fn others(&self) -> &[String] {
        &self.others
    }

    /// The entry's version, as the registry says it.
    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    /// One at `url`, checked, with nothing else listed: for other modules'
    /// tests.
    #[cfg(test)]
    pub(crate) fn stand_in(url: &str) -> Remote {
        Remote {
            url: remote_address(url).expect("a stand-in's address is one the agent may reach"),
            headers: Vec::new(),
            others: Vec::new(),
            version: None,
        }
    }
}

/// A header a remote server lists. `name` and `template` are as listed
/// (whole, not cut: what is sent), `description` cut for showing.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RemoteHeader {
    pub name: String,
    pub required: bool,
    pub secret: bool,
    pub description: String,
    /// Its value with one `{…}` where the credential goes (`Bearer
    /// {token}`), or a fixed value (no `{…}`), as listed; `None`: the
    /// credential alone.
    pub template: Option<String>,
}

/// The longest header name and template yobuild takes.
const HEADER_NAME_CHARS: usize = 64;
const HEADER_VALUE_CHARS: usize = 256;

impl RemoteHeader {
    /// Whether its value is fixed: a template with no `{…}`, sent as
    /// listed when required, nothing asked.
    pub fn fixed(&self) -> bool {
        self.template
            .as_deref()
            .is_some_and(|t| !t.contains(['{', '}']))
    }

    /// Whether the owner is asked for it: required, or optional and secret
    /// (an optional header that is not secret is left off), and not fixed.
    pub fn asked(&self) -> bool {
        (self.required || self.secret) && !self.fixed()
    }

    /// Whether it is sent: asked, or required with a fixed value.
    pub fn sent(&self) -> bool {
        self.asked() || (self.required && self.fixed())
    }

    /// Its value: `${variable}` where the credential goes (its template's
    /// one `{…}`, else alone), or its fixed value; checked as a manifest's
    /// header is ([`crate::manifest::guard`]). `None` when yobuild cannot
    /// fill it: `variable` not a plain name, a name a header cannot have,
    /// a name or template too long, a template with several `{…}`, or a
    /// `$` in it, or a value the rule refuses (a literal part other than
    /// `Bearer`/`Basic`/`Token` on a header named like a secret).
    pub fn value(&self, variable: &str) -> Option<Value> {
        let named = !self.name.is_empty()
            && self.name.chars().count() <= HEADER_NAME_CHARS
            && self
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !named || !crate::manifest::identifier(variable) {
            return None;
        }
        let reference = format!("${{{variable}}}");
        let text = match &self.template {
            None => reference,
            Some(t) if t.contains('$') || t.chars().count() > HEADER_VALUE_CHARS => return None,
            Some(t) if self.fixed() => t.clone(),
            Some(t) => {
                let (open, close) = (t.find('{')?, t.find('}')?);
                let single = t.matches('{').count() == 1 && t.matches('}').count() == 1;
                if !single || close < open {
                    return None;
                }
                format!("{}{reference}{}", &t[..open], &t[close + 1..])
            }
        };
        let value = Value::new(text).ok()?;
        crate::manifest::guard(&self.name, &value).ok()?;
        Some(value)
    }
}

/// The stand-in variable a header's value is checked with when found.
const CREDENTIAL: &str = "CREDENTIAL";

/// Names on this machine or its network (RFC 6761's `localhost`, RFC 6762's
/// `local`, RFC 8375's `home.arpa`, ICANN's `internal`, and
/// `localhost.localdomain`, which many systems map to this machine).
const LOCAL_SUFFIXES: [&str; 5] = [
    ".localhost", ".local", ".internal", ".home.arpa", ".localdomain",
];
const LOCAL_NAMES: [&str; 3] = ["localhost", "localdomain", "localhost6.localdomain6"];

/// Why a remote's address is not one the agent may reach: more strictly
/// than a manifest's (`manifest::endpoint`), since the agent picked it — no
/// user in it, https only, not an IP address, not a name on this machine
/// or its network ([`LOCAL_NAMES`], [`LOCAL_SUFFIXES`]), nor a name of one
/// label (a machine on this network). The name is not resolved: a public
/// name pointing inward is not caught.
pub(crate) fn remote_address(url: &str) -> Result<url::Url, String> {
    if url.contains('{') {
        return Err("its address takes values".into());
    }
    let refused = |why: &str| Err(format!("an address refused ({why})"));
    let Ok(parsed) = url::Url::parse(url) else {
        return refused("not a URL");
    };
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return refused("a user in it");
    }
    if parsed.scheme() != "https" {
        return refused("http: a server the agent found is reached over https only");
    }
    match parsed.host() {
        Some(url::Host::Domain(name)) => {
            let name = name.trim_end_matches('.').to_ascii_lowercase();
            let local = LOCAL_NAMES.contains(&name.as_str())
                || LOCAL_SUFFIXES.iter().any(|end| name.ends_with(end))
                || !name.contains('.');
            if local {
                return refused("a name on this machine or its network");
            }
        }
        Some(_) => return refused("an IP address"),
        None => return refused("no host"),
    }
    Ok(parsed)
}

/// The longest registry name yobuild takes as a remote's.
const REGISTRY_NAME_CHARS: usize = 200;

/// Whether `name` is of the MCP registry's shape (`namespace/name`: the
/// namespace letters, digits, `.` and `-`; the name those and `_`), within
/// [`REGISTRY_NAME_CHARS`]: a remote candidate's name is what the agent
/// reads and passes to `load_plugin`, so it is one plain line.
fn registry_name(name: &str) -> bool {
    let Some((space, own)) = name.split_once('/') else {
        return false;
    };
    let plain = |part: &str, more: &[char]| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || more.contains(&c))
    };
    name.len() <= REGISTRY_NAME_CHARS && plain(space, &[]) && plain(own, &['_'])
}

/// A registry entry's remote, when it lists any: the one taken, or why
/// none is (each reason a count on the sources' line).
fn remote_of(server: &Json) -> Option<Result<Remote, String>> {
    let remotes = server["remotes"].as_array().filter(|r| !r.is_empty())?;
    let said = |r: &Json| {
        clip(
            &format!("{} {}", text(&r["type"]), text(&r["url"])),
            FIELD_CHARS,
        )
    };
    let mut why: Option<String> = None;
    for (at, item) in remotes.iter().enumerate() {
        if item["type"] != "streamable-http" {
            continue;
        }
        let url = text(&item["url"]);
        let address = match remote_address(&url) {
            Ok(address) => address,
            Err(e) => {
                why.get_or_insert(e);
                continue;
            }
        };
        let empty = Vec::new();
        let headers: Vec<RemoteHeader> = item["headers"]
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .map(|h| RemoteHeader {
                name: text(&h["name"]),
                required: h["isRequired"] == true,
                secret: h["isSecret"] == true,
                description: clip(&text(&h["description"]), DESCRIPTION_CHARS),
                template: h["value"].as_str().map(String::from),
            })
            .collect();
        if headers
            .iter()
            .any(|h| h.sent() && h.value(CREDENTIAL).is_none())
        {
            why.get_or_insert("a header yobuild cannot fill".into());
            continue;
        }
        let others = remotes
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != at)
            .map(|(_, r)| said(r))
            .collect();
        return Some(Ok(Remote {
            url: address,
            headers,
            others,
            version: server["version"].as_str().map(|v| clip(v, FIELD_CHARS)),
        }));
    }
    Some(Err(why.unwrap_or_else(|| {
        match remotes.iter().any(|r| r["type"] == "sse") {
            true => "SSE only: yoagent speaks streamable HTTP".into(),
            false => "no streamable-HTTP remote".into(),
        }
    })))
}

/// A remote the registry lists, with its entry's repository and when it
/// was published.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RemoteHit {
    remote: Remote,
    repository: Option<String>,
    published: Option<String>,
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

    async fn find(&self, need: &str, kind: ExtendKind) -> Found {
        if kind == ExtendKind::Remote {
            return self.find_remote(need).await;
        }
        // The MCP registry lists MCP servers only.
        let mcp = kind == ExtendKind::Mcp;
        let (registry, npm, npm_registry) = tokio::join!(
            async {
                match (mcp, &self.sources.registry) {
                    (true, Some(at)) => Some(registry(at, need, kind).await),
                    (true, None) => Some(Err("no MCP registry was set up for this agent".into())),
                    (false, _) => None,
                }
            },
            npm_search(need, kind),
            blocking(
                vec!["config".into(), "get".into(), "registry".into()],
                SOURCE_TIME
            ),
        );
        // The searches made: npm's, and the registry's when asked.
        let sources = 1 + usize::from(registry.is_some());
        let mut lines = Vec::new();
        let mut failed = Vec::new();
        let registry = match registry {
            None => Vec::new(),
            Some(Ok(found)) => {
                lines.push(found.line());
                found.hits
            }
            Some(Err(e)) => {
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
        let hits = merged(registry, npm);
        let found = hits.len();
        let now = now();
        let mut pending = hits.into_iter();
        let mut viewed: Vec<Viewing> = Vec::new();
        loop {
            let kept = viewed.iter().filter(|(_, _, p)| *p != Some(false)).count();
            let round: Vec<Hit> = pending
                .by_ref()
                .take(next_round(viewed.len(), kept))
                .collect();
            if round.is_empty() {
                break;
            }
            let names: Vec<String> = round.iter().map(|h| h.package.clone()).collect();
            let mut counts = downloads(&npm_registry, &names).await;
            let this: Vec<Viewing> = stream::iter(round)
                .map(|hit| {
                    let counted = counts.remove(&hit.package).unwrap_or_else(|| {
                        Err("npm's downloads API failed (its answer left this package out)".into())
                    });
                    candidate(need, kind, hit, &self.extend, now, counted)
                })
                .buffered(VIEWS_AT_ONCE)
                .collect()
                .await;
            viewed.extend(this);
        }
        let not_viewed = pending.len();
        let (candidates, unfit_count, unviewed) = cut(viewed);
        if unfit_count > 0 {
            lines.push(format!(
                "{unfit_count} left out once npm was asked: {}",
                unfit(kind)
            ));
        }
        if not_viewed > 0 {
            lines.push(format!(
                "{not_viewed} more found and not looked at (a search offers at most {MAX_CANDIDATES}, and asks npm about at most {VIEWED_AT_MOST}): a narrower need puts others first"
            ));
        }
        Found {
            kind,
            sources,
            lines,
            failed,
            candidates,
            found,
            not_viewed,
            unviewed,
        }
    }

    /// Remote servers: the MCP registry alone (npm lists none), each
    /// candidate as its entry says it — nothing to install, no version to
    /// choose between, so npm is not asked.
    async fn find_remote(&self, need: &str) -> Found {
        let found = match &self.sources.registry {
            Some(at) => registry(at, need, ExtendKind::Remote).await,
            None => Err("no MCP registry was set up for this agent".into()),
        };
        remote_found(need, found)
    }

    /// Keeps `candidates`, each in place of its package's, but for one
    /// with a version to take, which only another such replaces (npm
    /// failing once does not lose it).
    pub(crate) fn remember(&self, candidates: &[Candidate]) {
        let mut found = self.found.lock().unwrap_or_else(|e| e.into_inner());
        for candidate in candidates {
            debug_assert_eq!(
                candidate.kind == ExtendKind::Remote,
                candidate.remote.is_some(),
                "a remote candidate, and only one, has its remote"
            );
            let same =
                |c: &&mut Candidate| c.package == candidate.package && c.kind == candidate.kind;
            match found.iter_mut().find(same) {
                Some(old) if old.version.is_ok() && candidate.version.is_err() => {}
                Some(old) => *old = candidate.clone(),
                None => found.push(candidate.clone()),
            }
        }
    }
}

/// A remote search's result from the MCP registry's answer: each candidate
/// as its entry says it — nothing to install, no version to choose between,
/// so npm is not asked — up to [`MAX_CANDIDATES`].
fn remote_found(need: &str, found: Result<RegistryFound, String>) -> Found {
    let kind = ExtendKind::Remote;
    let (mut lines, mut failed) = (Vec::new(), Vec::new());
    let hits = match found {
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
    let total = hits.len();
    let candidates: Vec<Candidate> = hits
        .into_iter()
        .filter_map(|hit| {
            let found = hit.remote?;
            Some(Candidate {
                need: need.to_owned(),
                kind,
                package: hit.package,
                version: Err(NO_VERSION.into()),
                registry_name: hit.registry_name,
                from_npm: false,
                description: clip(&hit.description, DESCRIPTION_CHARS),
                licence: None,
                repository: found.repository,
                published: found.published,
                downloads: Err("a remote server has no npm downloads".into()),
                provenance: None,
                required_env: None,
                required_args: None,
                remote: Some(found.remote),
            })
        })
        .take(MAX_CANDIDATES)
        .collect();
    let not_viewed = total - candidates.len();
    if not_viewed > 0 {
        lines.push(format!(
            "{not_viewed} more found and not offered (a search offers at most {MAX_CANDIDATES}): a narrower need puts others first"
        ));
    }
    Found {
        kind,
        sources: 1,
        lines,
        failed,
        candidates,
        found: total,
        not_viewed,
        unviewed: None,
    }
}

/// One search's results: a line per source, the sources that failed, the
/// candidates.
struct Found {
    kind: ExtendKind,
    /// The sources searched: the MCP registry and npm, or npm alone.
    sources: usize,
    lines: Vec<String>,
    /// Each source that failed, with why.
    failed: Vec<String>,
    candidates: Vec<Candidate>,
    /// The packages the sources found, each once.
    found: usize,
    /// Of those, the ones npm was not asked about: past the candidates'
    /// room, or past [`VIEWED_AT_MOST`].
    not_viewed: usize,
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
    /// A remote server: its entry's remote, and when it was published.
    remote: Option<RemoteHit>,
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
    /// For remote servers: those left out, by why, in the order first
    /// seen.
    left: Vec<(String, usize)>,
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
        for (why, n) in &self.left {
            out.push(format!("{n} {why}"));
        }
        if self.unrelated > 0 {
            out.push(format!(
                "{} naming none of the need's words",
                self.unrelated
            ));
        }
        let left: usize = self.left.iter().map(|(_, n)| n).sum();
        if !out.is_empty() {
            line.push_str(&format!(
                " ({} left out: {})",
                self.remote + self.other + left + self.unrelated,
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

/// The two sources' hits as one list: those both found first (the
/// registry's, which lists what they need), then each source's own in
/// turn, each in its own order; each package once.
fn merged(registry: Vec<Hit>, npm: Vec<Hit>) -> Vec<Hit> {
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
    hits
}

/// How many hits npm is asked about next, `viewed` asked about already and
/// `kept` of them not libraries: as many as could still be kept, so a
/// library left out makes room for the next hit, up to
/// [`VIEWED_AT_MOST`] in all. 0: no more.
fn next_round(viewed: usize, kept: usize) -> usize {
    MAX_CANDIDATES
        .saturating_sub(kept)
        .min(VIEWED_AT_MOST.saturating_sub(viewed))
}

/// A hit npm was asked about: the candidate, why npm could not be asked
/// at all, and whether the version it would take (else its latest) is of
/// the kind searched for — an MCP server's has a program to start, a pi
/// or DSH package is marked as one ([`Viewed::fits`]; `None`: not known).
type Viewing = (Candidate, Option<String>, Option<bool>);

/// The viewed hits made the result: those npm says are not of the kind
/// (an MCP library: no program; an unmarked pi or DSH package) left out.
/// With the candidates: how many were left out so, and why npm could be
/// asked about none of them, when it could not.
fn cut(viewed: Vec<Viewing>) -> (Vec<Candidate>, usize, Option<String>) {
    let unviewed = match !viewed.is_empty() && viewed.iter().all(|(_, e, _)| e.is_some()) {
        true => viewed.first().and_then(|(_, e, _)| e.clone()),
        false => None,
    };
    let before = viewed.len();
    let mut kept: Vec<Candidate> = viewed
        .into_iter()
        .filter(|(_, _, fits)| *fits != Some(false))
        .map(|(c, _, _)| c)
        .collect();
    let unfit = before - kept.len();
    kept.truncate(MAX_CANDIDATES);
    (kept, unfit, unviewed)
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

async fn registry(base: &url::Url, need: &str, kind: ExtendKind) -> Result<RegistryFound, String> {
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
    ranked(&words, answers, kind)
}

/// The searches' pages as one: servers found by more of the words first,
/// then in the order found; each left-out server counted once. Every
/// search failing is an error (each reason once). For MCP servers a hit is
/// an npm package over stdio; for remote servers, a remote it lists that
/// yobuild could reach ([`remote_of`]).
fn ranked(
    words: &[String],
    pages: Vec<Result<Page, String>>,
    kind: ExtendKind,
) -> Result<RegistryFound, String> {
    let mut ranked: Vec<(usize, Hit)> = Vec::new();
    let mut found = RegistryFound {
        searches: pages.len(),
        ..RegistryFound::default()
    };
    let (mut remote, mut other, mut unrelated) =
        (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
    let mut left: Vec<(String, BTreeSet<String>)> = Vec::new();
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
            let hit = match kind {
                ExtendKind::Remote => match listed.remote {
                    Some(Ok(found)) if registry_name(&listed.key) => Some(Hit {
                        package: listed.key.clone(),
                        registry_name: Some(listed.key.clone()),
                        from_npm: false,
                        description: listed.description,
                        required_env: None,
                        required_args: None,
                        remote: Some(found),
                    }),
                    found => {
                        let why = match found {
                            Some(Ok(_)) => "a name not of the registry's shape".into(),
                            Some(Err(why)) => why,
                            None => "not a remote server".into(),
                        };
                        match left.iter_mut().find(|(w, _)| *w == why) {
                            Some((_, keys)) => _ = keys.insert(listed.key),
                            None => left.push((why, BTreeSet::from([listed.key]))),
                        }
                        continue;
                    }
                },
                _ => listed.hit,
            };
            match hit {
                Some(hit) => match ranked.iter_mut().find(|(_, h)| h.package == hit.package) {
                    Some((n, _)) => *n += 1,
                    None => ranked.push((1, hit)),
                },
                None if listed.remote_only => _ = remote.insert(listed.key),
                None => _ = other.insert(listed.key),
            }
        }
    }
    if found.failed.len() == found.searches {
        return Err(distinct(&found.failed).join("; "));
    }
    (found.remote, found.other, found.unrelated) = (remote.len(), other.len(), unrelated.len());
    // A server left out once is not counted again by another search's
    // other reason, nor when it was kept.
    let mut counted: BTreeSet<String> = ranked.iter().map(|(_, h)| h.package.clone()).collect();
    for (why, keys) in left {
        let n = keys.iter().filter(|k| counted.insert((*k).clone())).count();
        if n > 0 {
            found.left.push((why, n));
        }
    }
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
    remote_only: bool,
    /// Its remote, or why none is taken; `None`: it lists none.
    remote: Option<Result<RemoteHit, String>>,
    description: String,
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
        let remote_hit = remote_of(server).map(|found| {
            found.map(|remote| RemoteHit {
                remote,
                repository: server["repository"]["url"]
                    .as_str()
                    .map(|r| clip(r, FIELD_CHARS)),
                published: entry["_meta"]["io.modelcontextprotocol.registry/official"]
                    ["publishedAt"]
                    .as_str()
                    .and_then(seconds)
                    .map(day),
            })
        });
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
            let remote_only =
                packages.is_empty() && server["remotes"].as_array().is_some_and(|r| !r.is_empty());
            page.servers.push(Listed {
                key,
                text: words,
                hit: None,
                remote_only,
                remote: remote_hit,
                description: text(&server["description"]),
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
            remote: None,
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
            remote_only: false,
            remote: remote_hit,
            description: text(&server["description"]),
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
    /// Packages not marked as MCP: no MCP keyword, and no "MCP" in their
    /// name or description.
    unmarked: usize,
}

impl NpmFound {
    fn line(&self) -> String {
        let mut line = format!("npm: {} found", self.hits.len());
        let mut out = Vec::new();
        if self.unmarked > 0 {
            out.push(format!("{} not marked as MCP", self.unmarked));
        }
        if self.unnamed > 0 {
            out.push(format!(
                "{} with a name yobuild does not take",
                self.unnamed
            ));
        }
        if !out.is_empty() {
            line.push_str(&format!(
                " ({} left out: {})",
                self.unmarked + self.unnamed,
                out.join(", ")
            ));
        }
        line
    }
}

/// Whether npm's search answer `p` is marked as an MCP server's: in its
/// name or a keyword, a word that begins or ends with "mcp" (`fastmcp`,
/// `mcpserver`) or is "modelcontextprotocol" (its scope), or an MCP
/// keyword; in its description, "MCP" or "Model Context Protocol" as
/// words.
fn marked(p: &Json) -> bool {
    let mcp_word = |text: &str| {
        text.split(|c: char| !c.is_alphanumeric())
            .map(str::to_lowercase)
            .any(|w| w.starts_with("mcp") || w.ends_with("mcp") || w == "modelcontextprotocol")
    };
    let keywords: Vec<&str> = p["keywords"]
        .as_array()
        .map(|all| all.iter().filter_map(Json::as_str).collect())
        .unwrap_or_default();
    let keyword = keywords
        .iter()
        .any(|k| MCP_KEYWORDS.contains(&k.to_lowercase().as_str()) || mcp_word(k));
    let description = text(&p["description"]);
    keyword
        || mcp_word(&text(&p["name"]))
        || mentions(&description, "mcp")
        || mentions(&description, "model context protocol")
}

/// The keyword npm's search ranks a kind's packages by (`keywords:` ranks;
/// it does not filter).
fn search_keyword(kind: ExtendKind) -> &'static str {
    match kind {
        // npm lists no remote server: never searched for one.
        ExtendKind::Mcp | ExtendKind::Remote => "mcp",
        ExtendKind::Pi => PI_KEYWORD,
        ExtendKind::Dsh => DSH_KEYWORD,
    }
}

/// Why a package npm was asked about is not of `kind`.
fn unfit(kind: ExtendKind) -> &'static str {
    match kind {
        ExtendKind::Mcp | ExtendKind::Remote => "no program to start (a library, not a server)",
        ExtendKind::Pi => "not marked as a pi package (no `pi` field, no keyword `pi-package`)",
        ExtendKind::Dsh => "not marked as a DSH package (no `dsh` field, no keyword `dsh-plugin`)",
    }
}

async fn npm_search(need: &str, kind: ExtendKind) -> Result<NpmFound, String> {
    let keyword = format!("keywords:{}", search_keyword(kind));
    let mut args: Vec<String> = ["search", "--json", "--searchlimit=20", "--", &keyword]
        .map(String::from)
        .into();
    args.extend(need.split_whitespace().map(String::from));
    npm_hits(&blocking(args, SEARCH_TIME).await?, kind)
}

/// `npm search --json`'s answer: a list of packages, each with its name.
/// An MCP server's must be marked as MCP ([`marked`]); a pi or DSH
/// package is judged once npm is asked about it (its `pi` or `dsh` field
/// is not in the search's answer).
fn npm_hits(stdout: &str, kind: ExtendKind) -> Result<NpmFound, String> {
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
        unmarked: 0,
    };
    for p in list {
        let name = p["name"]
            .as_str()
            .ok_or("npm search answered a package without a `name` (has its output changed?)")?;
        if Package::new(name, "1.0.0").is_err() {
            found.unnamed += 1;
            continue;
        }
        if kind == ExtendKind::Mcp && !marked(p) {
            found.unmarked += 1;
            continue;
        }
        found.hits.push(Hit {
            package: name.to_owned(),
            registry_name: None,
            from_npm: true,
            description: text(&p["description"]),
            required_env: None,
            required_args: None,
            remote: None,
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
/// `.npmrc`), killed after `limit`, off the async threads.
async fn blocking(args: Vec<String>, limit: Duration) -> Result<String, String> {
    let args = quiet(args);
    tokio::task::spawn_blocking(move || {
        let dir = tempfile::tempdir().map_err(|e| format!("a folder for npm: {e}"))?;
        crate::install::npm_within(dir.path(), &args, limit).map_err(|e| e.to_string())
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
    /// It names a program to start (`bin`): a server, not a library.
    program: bool,
    /// Its `pi` field (pi's manifest of extensions, skills, …).
    pi: Field,
    /// Its `dsh` field (a DSH bundle's rows).
    dsh: Field,
    keywords: Vec<String>,
    /// `keywords` of another shape than a list of texts (or one text):
    /// what it is.
    keywords_shape: Option<String>,
}

/// A package field that marks its kind: absent, an object, or of another
/// shape (said, never taken for absent).
#[derive(Debug, Default, PartialEq, Eq)]
enum Field {
    #[default]
    Absent,
    Object,
    Other(String),
}

impl Field {
    fn of(value: &Json) -> Field {
        match value {
            Json::Null => Field::Absent,
            Json::Object(_) => Field::Object,
            other => Field::Other(shape(other).into()),
        }
    }
}

/// What kind of JSON value `value` is, as a message says it.
fn shape(value: &Json) -> &'static str {
    match value {
        Json::Null => "null",
        Json::Bool(_) => "a boolean",
        Json::Number(_) => "a number",
        Json::String(_) => "a text",
        Json::Array(_) => "a list",
        Json::Object(_) => "an object",
    }
}

impl Viewed {
    /// Whether it is of `kind`: an MCP server names a program; a pi
    /// package has a `pi` field or the keyword `pi-package`, a DSH package
    /// a `dsh` field or the keyword `dsh-plugin` (keywords in any case).
    fn fits(&self, kind: ExtendKind) -> bool {
        let keyword = |k: &str| self.keywords.iter().any(|w| w.eq_ignore_ascii_case(k));
        match kind {
            ExtendKind::Mcp => self.program,
            // A remote server is the MCP registry's, never npm's.
            ExtendKind::Remote => false,
            ExtendKind::Pi => self.pi == Field::Object || keyword(PI_KEYWORD),
            ExtendKind::Dsh => self.dsh == Field::Object || keyword(DSH_KEYWORD),
        }
    }

    /// Why npm's answer cannot say whether it is of `kind`: a marking
    /// field or its keywords of another shape.
    fn misshapen(&self, kind: ExtendKind) -> Option<String> {
        let (field, name) = match kind {
            ExtendKind::Mcp | ExtendKind::Remote => return None,
            ExtendKind::Pi => (&self.pi, "pi"),
            ExtendKind::Dsh => (&self.dsh, "dsh"),
        };
        if let Field::Other(what) = field {
            return Some(format!("npm view's `{name}` is {what}, not an object"));
        }
        self.keywords_shape
            .as_ref()
            .map(|what| format!("npm view's `keywords` is {what}, not a list of texts"))
    }
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
    let (keywords, keywords_shape) = match &view["keywords"] {
        Json::Null => (Vec::new(), None),
        Json::String(one) => (vec![one.clone()], None),
        Json::Array(all) => match all.iter().map(Json::as_str).collect::<Option<Vec<_>>>() {
            Some(all) => (all.into_iter().map(String::from).collect(), None),
            None => (
                Vec::new(),
                Some("a list holding what is not text".to_owned()),
            ),
        },
        other => (Vec::new(), Some(shape(other).to_owned())),
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
        program: match &view["bin"] {
            Json::String(bin) => !bin.is_empty(),
            Json::Object(bins) => !bins.is_empty(),
            _ => false,
        },
        pi: Field::of(&view["pi"]),
        dsh: Field::of(&view["dsh"]),
        keywords,
        keywords_shape,
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

/// `YYYY-MM-DDTHH:MM:SSZ` of `seconds` since 1970: npm's `--before`.
pub(crate) fn iso(seconds: i64) -> String {
    let s = seconds.rem_euclid(DAY);
    format!(
        "{}T{:02}:{:02}:{:02}Z",
        day(seconds),
        s / 3600,
        s % 3600 / 60,
        s % 60
    )
}

/// Seconds since 1970; `None` for a clock that reads before it.
pub(crate) fn now() -> Option<i64> {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(since.as_secs()).ok()
}

/// A hit made a candidate: npm asked about its versions, and about the one
/// it would take when that is not the latest; `downloads` its count, or
/// why none. Beside it, why npm could not be asked about it at all, and
/// whether it is of `kind` ([`Viewed::fits`]): the version it would take,
/// else (none to take) its latest; not known when npm could not say. One
/// whose marks npm gives in another shape is kept, not loadable, saying
/// so.
async fn candidate(
    need: &str,
    kind: ExtendKind,
    hit: Hit,
    extend: &Extend,
    now: Option<i64>,
    downloads: Result<u64, String>,
) -> Viewing {
    let mut candidate = Candidate {
        need: need.to_owned(),
        kind,
        package: hit.package.clone(),
        version: Err(String::new()),
        registry_name: hit.registry_name,
        from_npm: hit.from_npm,
        description: clip(&hit.description, DESCRIPTION_CHARS),
        licence: None,
        repository: None,
        published: None,
        downloads,
        provenance: None,
        required_env: hit.required_env,
        required_args: hit.required_args,
        remote: None,
    };
    let view = |spec: String| async move {
        let args = vec!["view".into(), spec, "--json".into()];
        viewed(&blocking(args, SOURCE_TIME).await?)
    };
    let latest = match view(hit.package.clone()).await {
        Ok(latest) => latest,
        Err(e) => {
            candidate.version = Err(format!("npm could not say what it is: {e}"));
            return (candidate, Some(e), None);
        }
    };
    // Whether it is of the kind: by the version it would take, once known.
    if let Some(why) = latest.misshapen(kind) {
        candidate.version = Err(why);
        return (candidate, None, Some(true));
    }
    let mut fits = Some(latest.fits(kind));
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
        return (candidate, None, fits);
    };
    candidate.published = latest.time.get(&version).and_then(|t| seconds(t)).map(day);
    let latest_fits = latest.fits(kind);
    let this = match latest.version.as_deref() == Some(version.as_str()) {
        true => Ok(latest),
        false => view(format!("{}@{version}", hit.package)).await,
    };
    match this {
        Ok(this) if this.misshapen(kind).is_some() => {
            fits = Some(true);
            candidate.version = Err(format!(
                "{version}: {}",
                this.misshapen(kind).unwrap_or_default()
            ))
        }
        Ok(this) if !this.fits(kind) => {
            // Not of the kind, unless the latest is (then said).
            fits = Some(latest_fits);
            candidate.version = Err(match kind {
                ExtendKind::Mcp | ExtendKind::Remote => {
                    format!("{version} names no program to start (`bin`), though the latest does")
                }
                ExtendKind::Pi | ExtendKind::Dsh => {
                    format!("{version} is {}, though the latest is marked", unfit(kind))
                }
            })
        }
        Ok(this) => {
            fits = Some(true);
            candidate.licence = this.licence.map(|l| clip(&l, FIELD_CHARS));
            candidate.repository = this.repository.map(|r| clip(&r, FIELD_CHARS));
            candidate.provenance = this.provenance;
        }
        Err(e) => {
            fits = None;
            candidate.version = Err(format!("npm could not say what {version} is: {e}"))
        }
    }
    (candidate, None, fits)
}

/// Last week's downloads of each of `names`, or why not: npm's downloads
/// API is asked only when npm's registry here (`npm_registry`) is npmjs's.
async fn downloads(
    npm_registry: &Result<String, String>,
    names: &[String],
) -> HashMap<String, Result<u64, String>> {
    let why = match npm_registry {
        Ok(r) if is_npmjs(r) => return downloads_of(DOWNLOADS, names).await,
        Ok(r) => format!(
            "not asked: npm's registry here is {}, not npmjs's",
            clip(r, FIELD_CHARS)
        ),
        Err(e) => format!("not asked: npm's registry could not be read ({e})"),
    };
    names
        .iter()
        .map(|n| (n.clone(), Err(why.clone())))
        .collect()
}

/// The requests npm's downloads API is asked, in order: the unscoped
/// packages [`DOWNLOADS_AT_ONCE`] to a request (its bulk form, so a
/// search's candidates do not run into its rate limit), then each scoped
/// one alone (the bulk form takes none).
fn requests(packages: &[String]) -> Vec<Vec<String>> {
    let (scoped, unscoped): (Vec<String>, Vec<String>) =
        packages.iter().cloned().partition(|p| p.starts_with('@'));
    unscoped
        .chunks(DOWNLOADS_AT_ONCE)
        .map(<[String]>::to_vec)
        .chain(scoped.into_iter().map(|p| vec![p]))
        .collect()
}

/// Last week's downloads of each of `packages` from npm's downloads API at
/// `base`, or why not: [`requests`] one at a time (several at once ran
/// into its rate limit), within [`DOWNLOADS_TIME`] each and twice that in
/// all — those past it not asked.
async fn downloads_of(base: &str, packages: &[String]) -> HashMap<String, Result<u64, String>> {
    let failed = |e: String| format!("npm's downloads API failed ({e})");
    let client = match reqwest::Client::builder()
        .timeout(DOWNLOADS_TIME)
        .user_agent(concat!("yobuild/", env!("CARGO_PKG_VERSION")))
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            let why = failed(said(&e));
            return packages
                .iter()
                .map(|p| (p.clone(), Err(why.clone())))
                .collect();
        }
    };
    let deadline = tokio::time::Instant::now() + 2 * DOWNLOADS_TIME;
    let mut all = HashMap::new();
    for names in requests(packages) {
        if tokio::time::Instant::now() >= deadline {
            let why = format!(
                "not asked: npm's downloads API took more than {} s for the others before it",
                (2 * DOWNLOADS_TIME).as_secs()
            );
            all.extend(names.into_iter().map(|n| (n, Err(why.clone()))));
            continue;
        }
        let body = async {
            let response = client
                .get(format!("{base}{}", names.join(",")))
                .send()
                .await
                .map_err(|e| said(&e))?;
            let status = response.status();
            if !status.is_success() {
                return Err(format!("HTTP {status}"));
            }
            response.json::<Json>().await.map_err(|e| said(&e))
        }
        .await;
        match body {
            Ok(body) => all.extend(counts(&body, &names)),
            Err(e) => all.extend(names.into_iter().map(|n| (n, Err(failed(e.clone()))))),
        }
    }
    all
}

/// The counts npm's downloads API answered for `names`: one package's
/// answer is its own (`{downloads}`), several's keyed by name (`null` for
/// one it does not know); an answer may be `{error}` instead, npm's words
/// kept.
fn counts(body: &Json, names: &[String]) -> Vec<(String, Result<u64, String>)> {
    let npm_said = |entry: &Json| {
        entry["error"]
            .as_str()
            .map(|e| format!("npm's downloads API answered: {}", clip(e, FIELD_CHARS)))
    };
    let count = |entry: Option<&Json>| match entry {
        Some(Json::Null) => Err("npm's downloads API knows no such package".to_owned()),
        Some(entry) => entry["downloads"].as_u64().ok_or_else(|| {
            npm_said(entry).unwrap_or_else(|| {
                "npm's downloads API failed (it answered without a count)".to_owned()
            })
        }),
        None => Err(npm_said(body).unwrap_or_else(|| {
            "npm's downloads API failed (its answer left this package out)".to_owned()
        })),
    };
    match names {
        [one] => vec![(one.clone(), count(Some(body)))],
        _ => names
            .iter()
            .map(|n| (n.clone(), count(body.get(n.as_str()))))
            .collect(),
    }
}

/// The result's text: the sources' lines, then each candidate.
fn said_to_agent(need: &str, found: &Found, extend: &Extend) -> String {
    let what = format!("{}s", found.kind.label());
    let mut text = format!(
        "find_plugin({need:?}, kind {:?}): {what}, as the sources say. Nothing was installed or started.\nSources: {}.\n",
        found.kind.name(),
        found.lines.join("; ")
    );
    if found.candidates.is_empty() {
        text.push_str("No candidates.");
        return text;
    }
    if found.kind == ExtendKind::Remote {
        text.push_str(&remotes_said(&found.candidates));
        return text.trim_end().to_owned();
    }
    text.push_str(&format!(
        "Candidates (their descriptions, licences and repositories are their authors' own words, not verified; a version is taken only once published {} or more days ago):\n",
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
        if c.kind != ExtendKind::Mcp {
            continue;
        }
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

/// Remote candidates, as the agent reads them: each address and the
/// headers the owner would be asked for.
fn remotes_said(candidates: &[Candidate]) -> String {
    let mut text = String::from(
        "Candidates (their descriptions and repositories are their authors' own words, not verified; nothing of them runs here: each call of their tools would send its arguments to the server's host, which may change what it does any day). yobuild does not load a remote server yet: tell the owner what you found.\n",
    );
    for (i, c) in candidates.iter().enumerate() {
        let Some(remote) = c.remote() else { continue };
        text.push_str(&format!(
            "{}. {} at {} (version {}, published {}; found in the MCP registry)\n",
            i + 1,
            c.package,
            remote.host(),
            remote.version().unwrap_or("not given"),
            c.published
                .as_deref()
                .unwrap_or("on a date the registry does not give")
        ));
        if !c.description.is_empty() {
            text.push_str(&format!("   {}\n", c.description));
        }
        let headers: Vec<String> = remote
            .headers()
            .iter()
            .map(|h| {
                let what = match (h.required, h.secret, h.fixed()) {
                    (true, _, true) => "required, fixed",
                    (false, _, true) => "optional, left off",
                    (true, true, false) => "required, secret",
                    (true, false, false) => "required",
                    (false, true, false) => "optional, secret",
                    (false, false, false) => "optional, left off",
                };
                format!("{} ({what})", clip(&h.name, FIELD_CHARS))
            })
            .collect();
        text.push_str(&format!(
            "   address: {}; headers: {}; repository (claimed by the entry): {}\n",
            clip(remote.url().as_str(), FIELD_CHARS),
            match headers.is_empty() {
                true => "none".to_owned(),
                false => format!(
                    "{} (the owner names the environment variable of each one asked; you never do)",
                    headers.join(", ")
                ),
            },
            c.repository.as_deref().unwrap_or("not given")
        ));
    }
    text
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
                "remote": c.remote().map(|r| json!({
                    "url": r.url().as_str(),
                    "host": r.host(),
                    "version": r.version(),
                    "headers": r.headers().iter().map(|h| json!({
                        "name": clip(&h.name, FIELD_CHARS),
                        "required": h.required,
                        "secret": h.secret,
                        "description": h.description,
                        "value": h.template.as_deref().map(|t| clip(t, FIELD_CHARS)),
                    })).collect::<Vec<_>>(),
                    "others": r.others(),
                })),
            })
        })
        .collect();
    json!({
        "kind": found.kind.name(),
        "sources": found.lines,
        "found": found.found,
        "notLookedAt": found.not_viewed,
        "candidates": candidates,
    })
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
        "Search for a plugin that gives a capability you lack (a linter, a file format): an MCP \
         server (the MCP registry and npm), a pi or DSH package (npm), or a remote MCP server \
         (the MCP registry), as `kind` allows. It installs and starts nothing; it returns \
         candidates, the version of each that could be loaded, and what the sources say of \
         them, which is the packages' own claims."
    }
    fn parameters_schema(&self) -> Json {
        let kinds: Vec<&str> = self.extend.kinds.iter().map(|k| k.name()).collect();
        let kind = format!(
            "What to find: {} (the default: the first).",
            self.extend
                .kinds
                .iter()
                .map(|k| format!("{:?} ({})", k.name(), k.label()))
                .collect::<Vec<_>>()
                .join(", "),
        );
        json!({
            "type": "object",
            "properties": {
                "need": { "type": "string", "description": "The capability, in a few words, naming the tool when you know one: \"eslint\", \"read xlsx files\"." },
                "kind": { "type": "string", "enum": kinds, "description": kind }
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
        let refused = |given: &Json| {
            ToolError::InvalidArgs(format!(
                "kind {given}: this agent may find {}",
                self.extend
                    .kinds
                    .iter()
                    .map(|k| format!("{:?}", k.name()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        };
        let kind = match &params["kind"] {
            // The first kind the manifest lists (it refuses an empty list).
            Json::Null => self.extend.kinds.first().copied(),
            Json::String(name) => self.extend.kinds.iter().copied().find(|k| k.name() == name),
            _ => None,
        }
        .ok_or_else(|| refused(&params["kind"]))?;
        let found = self.find(&need, kind).await;
        let text = said_to_agent(&need, &found, &self.extend);
        if found.failed.len() == found.sources {
            self.session.notice(
                NoticeLevel::Warning,
                format!("find_plugin could not search: {}.", found.failed.join("; ")),
            );
            let which = match (found.sources, kind) {
                (1, ExtendKind::Remote) => "The MCP registry, the one source for this kind, failed",
                (1, _) => "npm, the one source for this kind, failed",
                _ => "Both sources failed",
            };
            return Err(ToolError::Failed(format!(
                "{text}\n{which}: this is not \"nothing found\"."
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
