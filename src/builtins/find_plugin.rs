//! `find_plugin` (`[extend]`, design §14): the agent searches for a plugin
//! it lacks. Step 1: MCP stdio servers, from the MCP registry and npm's
//! search. It starts, loads and installs nothing; what it returns is the
//! packages' own words, cleaned and cut. The session keeps each candidate
//! it returned: `load_plugin` takes only those.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::future::join_all;
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
/// The MCP registry yobuild searches: its `v0` API.
pub const MCP_REGISTRY: &str = "https://registry.modelcontextprotocol.io";
/// Another MCP registry, for the tests: https, or http on this machine.
pub const MCP_REGISTRY_ENV: &str = "YOBUILD_MCP_REGISTRY";
const NPMJS: &str = "https://registry.npmjs.org/";
const DOWNLOADS: &str = "https://api.npmjs.org/downloads/point/last-week/";
const DESCRIPTION_CHARS: usize = 200;
const FIELD_CHARS: usize = 120;
/// Words that find every server, left out of the MCP registry's search.
const PLAIN_WORDS: [&str; 8] = ["mcp", "server", "the", "and", "for", "with", "from", "tool"];
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
    /// else [`MCP_REGISTRY`].
    pub fn from_env() -> Result<Sources, String> {
        match std::env::var(MCP_REGISTRY_ENV) {
            Ok(url) if !url.is_empty() => Self::at(&url),
            _ => Self::at(MCP_REGISTRY),
        }
    }

    /// The MCP registry at `url`: https, or http on this machine.
    pub fn at(url: &str) -> Result<Sources, String> {
        let registry =
            crate::manifest::endpoint(url).map_err(|m| format!("{MCP_REGISTRY_ENV} {m}"))?;
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
    /// Last week's, from npm's downloads API; `None` when not known (a
    /// registry other than npmjs's, or the API failing).
    pub downloads: Option<u64>,
    /// `version` has a provenance attestation published with it (npm
    /// checks it at install with `npm audit signatures`; yobuild does not).
    pub provenance: bool,
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

    /// Every candidate returned this session, the latest of each package.
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
                lines.push(format!("npm: {} found", found.len()));
                found
            }
            Err(e) => {
                lines.push(format!("npm: failed ({e})"));
                failed.push(format!("npm: {e}"));
                Vec::new()
            }
        };
        let hits = merged(registry, npm);
        let npmjs = npm_registry.is_ok_and(|r| r.trim() == NPMJS);
        let now = now();
        let candidates = join_all(
            hits.into_iter()
                .map(|hit| candidate(hit, &self.extend, now, npmjs)),
        )
        .await;
        Found {
            lines,
            failed,
            candidates,
        }
    }

    fn remember(&self, candidates: &[Candidate]) {
        let mut found = self.found.lock().unwrap_or_else(|e| e.into_inner());
        for candidate in candidates {
            found.retain(|c| c.package != candidate.package);
            found.push(candidate.clone());
        }
    }
}

/// One search's results: a line per source, the sources that failed, the
/// candidates.
struct Found {
    lines: Vec<String>,
    failed: Vec<String>,
    candidates: Vec<Candidate>,
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

/// The MCP registry's servers for a search: those with an npm package over
/// stdio, and how many were left out.
#[derive(Debug, Default, PartialEq, Eq)]
struct RegistryFound {
    hits: Vec<Hit>,
    remote: usize,
    other: usize,
    /// A search of several that failed, with why.
    partly: Option<String>,
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
        if !out.is_empty() {
            line.push_str(&format!(
                " ({} left out: {})",
                self.remote + self.other,
                out.join(", ")
            ));
        }
        if let Some(why) = &self.partly {
            line.push_str(&format!("; one of its searches failed ({why})"));
        }
        line
    }
}

/// The two sources' hits as one list of at most [`MAX_CANDIDATES`]: those
/// both found first (the registry's, which lists what they need), then
/// each source's own in turn, each in its own order.
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
    hits.truncate(MAX_CANDIDATES);
    hits
}

/// Whether `hit` names `word` as a word (or with `s`, `er`, `ers`, `ing`,
/// `ed` after it) in its registry name, package or description: the
/// registry matches any part of a name, `lint` in `maxwellintl` too.
fn mentions(hit: &Hit, word: &str) -> bool {
    let text = format!(
        "{} {} {}",
        hit.registry_name.as_deref().unwrap_or_default(),
        hit.package,
        hit.description
    )
    .to_lowercase();
    text.split(|c: char| !c.is_alphanumeric()).any(|token| {
        token
            .strip_prefix(word)
            .is_some_and(|rest| ["", "s", "er", "ers", "ing", "ed"].contains(&rest))
    })
}

/// The words of `need` the MCP registry is searched for, each on its own
/// (it matches a server's name): up to four of three letters or more, not
/// [`PLAIN_WORDS`]; else the whole of it.
fn registry_words(need: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    for word in need
        .to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '-'))
    {
        let word = word.trim_matches('-');
        if word.chars().count() >= 3
            && !PLAIN_WORDS.contains(&word)
            && !words.iter().any(|w| w == word)
        {
            words.push(word.to_owned());
        }
    }
    words.truncate(4);
    if words.is_empty() {
        words.push(need.to_owned());
    }
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
            let word = word.as_str();
            url.query_pairs_mut()
                .append_pair("search", word)
                .append_pair("version", "latest")
                .append_pair("limit", "30");
            let response = client.get(url).send().await.map_err(|e| said(&e))?;
            let status = response.status();
            if !status.is_success() {
                return Err(format!("HTTP {status}"));
            }
            let body: Json = response.json().await.map_err(|e| said(&e))?;
            registry_found(&body)
        }
    }))
    .await;
    // Servers found by more of the words first, then in the order found.
    let mut ranked: Vec<(usize, Hit)> = Vec::new();
    let mut found = RegistryFound::default();
    let mut errors = Vec::new();
    for answer in answers {
        match answer {
            Ok(one) => {
                found.remote = found.remote.max(one.remote);
                found.other = found.other.max(one.other);
                for hit in one
                    .hits
                    .into_iter()
                    .filter(|h| words.iter().any(|w| mentions(h, w)))
                {
                    match ranked.iter_mut().find(|(_, h)| h.package == hit.package) {
                        Some((n, _)) => *n += 1,
                        None => ranked.push((1, hit)),
                    }
                }
            }
            Err(e) => errors.push(e),
        }
    }
    if errors.len() == words.len() {
        return Err(errors.swap_remove(0));
    }
    found.partly = errors.into_iter().next();
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

/// The MCP registry's answer (`v0`: `{servers: [{server, _meta}]}`; an
/// entry that is the server itself read too).
fn registry_found(body: &Json) -> Result<RegistryFound, String> {
    let servers = body["servers"]
        .as_array()
        .ok_or("it answered without a `servers` list")?;
    let mut found = RegistryFound::default();
    for entry in servers {
        let server = entry.get("server").unwrap_or(entry);
        let empty = Vec::new();
        let packages = server["packages"].as_array().unwrap_or(&empty);
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
            match remote {
                true => found.remote += 1,
                false => found.other += 1,
            }
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
        found.hits.push(Hit {
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
        });
    }
    Ok(found)
}

/// A JSON string as it is, anything else empty.
fn text(value: &Json) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

async fn npm_search(need: &str) -> Result<Vec<Hit>, String> {
    let mut args: Vec<String> = ["search", "--json", "--searchlimit=20", "--", "keywords:mcp"]
        .map(String::from)
        .into();
    args.extend(need.split_whitespace().map(String::from));
    npm_hits(&blocking(args).await?)
}

/// `npm search --json`'s answer: a list of packages.
fn npm_hits(stdout: &str) -> Result<Vec<Hit>, String> {
    let list: Json = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("npm search answered what is not JSON ({e})"))?;
    let list = list
        .as_array()
        .ok_or("npm search answered without a list")?;
    Ok(list
        .iter()
        .filter_map(|p| {
            let name = p["name"].as_str()?;
            Package::new(name, "1.0.0").ok()?;
            Some(Hit {
                package: name.to_owned(),
                registry_name: None,
                from_npm: true,
                description: text(&p["description"]),
                required_env: None,
                required_args: None,
            })
        })
        .collect())
}

/// npm with `args`, in a folder of its own (no project's `.npmrc`), with
/// [`SOURCE_TIME`], off the async threads.
async fn blocking(mut args: Vec<String>) -> Result<String, String> {
    args.push("--no-update-notifier".into());
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
    versions: Vec<String>,
    time: BTreeMap<String, String>,
    description: String,
    licence: Option<String>,
    repository: Option<String>,
    provenance: bool,
}

fn viewed(stdout: &str) -> Result<Viewed, String> {
    let view: Json = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("npm view answered what is not JSON ({e})"))?;
    if !view.is_object() {
        return Err("npm view answered what is not an object".into());
    }
    let versions = match &view["versions"] {
        Json::Array(all) => all
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        Json::String(one) => vec![one.clone()],
        _ => Vec::new(),
    };
    let time = view["time"]
        .as_object()
        .map(|t| {
            t.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                .collect()
        })
        .unwrap_or_default();
    let licence = match &view["license"] {
        Json::String(l) => Some(l.clone()),
        Json::Object(l) => l.get("type").and_then(Json::as_str).map(String::from),
        _ => None,
    };
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
        provenance: view["dist"]["attestations"].is_object(),
    })
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

/// The version to take of a package: the newest release not past
/// `latest`, published at least `min_age_days` before `now` (seconds); or
/// why none. With 0 days a publish time is not needed.
fn version_to_take(view: &Viewed, min_age_days: u32, now: i64) -> Result<String, String> {
    let latest = view.latest.as_deref().and_then(release);
    let mut releases: Vec<(&String, (u64, u64, u64))> = view
        .versions
        .iter()
        .filter_map(|v| Some((v, release(v)?)))
        .filter(|(_, n)| latest.is_none_or(|l| *n <= l))
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
        match view.time.get(*version).and_then(|t| seconds(t)) {
            Some(at) if at <= enough => return Ok((*version).clone()),
            Some(_) => continue,
            None => {
                return Err(format!(
                    "npm gives no publish time for {version}, so its age cannot be told"
                ))
            }
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

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// A hit made a candidate: npm asked about its versions, and about the one
/// it would take when that is not the latest; its downloads when `npmjs`.
async fn candidate(hit: Hit, extend: &Extend, now: i64, npmjs: bool) -> Candidate {
    let mut candidate = Candidate {
        package: hit.package.clone(),
        version: Err(String::new()),
        registry_name: hit.registry_name,
        from_npm: hit.from_npm,
        description: clip(&hit.description, DESCRIPTION_CHARS),
        licence: None,
        repository: None,
        published: None,
        downloads: None,
        provenance: false,
        required_env: hit.required_env,
        required_args: hit.required_args,
    };
    let view = |spec: String| async move {
        let args = vec!["view".into(), spec, "--json".into()];
        viewed(&blocking(args).await?)
    };
    let (latest, downloads) = tokio::join!(view(hit.package.clone()), async {
        match npmjs {
            true => downloads(&hit.package).await,
            false => None,
        }
    });
    candidate.downloads = downloads;
    let latest = match latest {
        Ok(latest) => latest,
        Err(e) => {
            candidate.version = Err(format!("npm could not say what it is: {e}"));
            return candidate;
        }
    };
    if candidate.description.is_empty() {
        candidate.description = clip(&latest.description, DESCRIPTION_CHARS);
    }
    candidate.version = version_to_take(&latest, extend.min_age_days, now);
    let Ok(version) = candidate.version.clone() else {
        return candidate;
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
    candidate
}

/// Last week's downloads from npm's API, or `None`.
async fn downloads(package: &str) -> Option<u64> {
    let client = reqwest::Client::builder()
        .timeout(DOWNLOADS_TIME)
        .build()
        .ok()?;
    let response = client
        .get(format!("{DOWNLOADS}{package}"))
        .send()
        .await
        .ok()?;
    let body: Json = response.error_for_status().ok()?.json().await.ok()?;
    body["downloads"].as_u64()
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
        let downloads = c
            .downloads
            .map_or_else(|| "unknown".to_owned(), |n| n.to_string());
        text.push_str(&format!(
            "   licence {}; downloads last week: {downloads}; repository (claimed by the package): {}; provenance: {}\n",
            c.licence.as_deref().unwrap_or("not given"),
            c.repository.as_deref().unwrap_or("not given"),
            match c.provenance {
                true => "an attestation is published with it",
                false => "none",
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
                "downloads": c.downloads,
                "provenance": c.provenance,
                "requiredEnv": c.required_env,
                "requiredArgs": c.required_args,
            })
        })
        .collect();
    json!({ "sources": found.lines, "candidates": candidates })
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
            _ => String::new(),
        };
        if need.is_empty() {
            return Err(ToolError::InvalidArgs(
                "`need` is required: the capability, in a few words".into(),
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
        self.remember(&found.candidates);
        Ok(ToolResult {
            content: vec![Content::Text { text }],
            details: details(&found),
        })
    }
}

#[cfg(test)]
mod tests;
