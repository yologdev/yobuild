//! `find_plugin` (`[extend]`, design §14) through the binary, against a
//! stub MCP registry and a stub npm registry (one axum server): no test
//! reaches the network. npm is the real CLI, pointed at the stub by
//! `npm_config_registry` (a setting `install` keeps). Unix, as `cli_test`.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::Path as UrlPath;
use axum::http::{StatusCode, Uri};
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use serde_json::{json, Value};

const DAY: u64 = 24 * 60 * 60;

/// `value` as a JSON response (axum's `json` feature is off).
#[allow(non_snake_case)]
fn Json(value: Value) -> Response {
    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        value.to_string(),
    )
        .into_response()
}

/// An ISO time `days` ago, as npm writes them.
fn ago(days: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let t = now - days * DAY;
    // Civil from days: the tool's own `day` is private, and this one,
    // written apart, checks it.
    let z = (t / DAY) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let s = t % DAY;
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.000Z",
        s / 3600,
        s / 60 % 60,
        s % 60
    )
}

/// A packument: `versions` published that many days ago, the last latest.
fn packument(name: &str, versions: &[(&str, u64)]) -> Value {
    let latest = versions.last().unwrap().0;
    let mut all = serde_json::Map::new();
    let mut time = serde_json::Map::new();
    for (version, days) in versions {
        all.insert(
            version.to_string(),
            json!({
                "name": name, "version": version, "description": format!("{name} lints"),
                "license": "MIT",
                "repository": { "type": "git", "url": format!("git+https://example.com/{name}.git") },
                "dist": { "tarball": format!("http://example.invalid/{name}.tgz"), "shasum": "0" }
            }),
        );
        time.insert(version.to_string(), json!(ago(*days)));
    }
    json!({
        "name": name, "dist-tags": { "latest": latest },
        "versions": all, "time": time, "description": format!("{name} lints")
    })
}

/// What the stubs fail at.
#[derive(Clone, Copy, Default)]
struct Broken {
    /// The MCP registry answers 503.
    registry: bool,
    /// npm's search answers 404.
    search: bool,
    /// Every packument answers 500 (`broken-mcp`'s always does).
    view: bool,
}

/// The queries the stubs were sent: `registry <search>` and `npm <text>`.
type Asked = Arc<Mutex<Vec<String>>>;

fn query(uri: &Uri, key: &str) -> String {
    url::form_urlencoded::parse(uri.query().unwrap_or_default().as_bytes())
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default()
}

/// The stub registries on a port of their own, serving until the process
/// ends, and what they were asked.
fn stub(broken: Broken) -> (u16, Asked) {
    let asked = Asked::default();
    let (tx, rx) = std::sync::mpsc::channel();
    let seen = asked.clone();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async move {
            let at = seen.clone();
            let servers = move |uri: Uri| async move {
                at.lock().unwrap().push(format!("registry {}", query(&uri, "search")));
                if broken.registry {
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                }
                Json(json!({ "servers": [
                    { "server": { "name": "io.example/lint", "description": "Lint\nyour code",
                        "packages": [{ "registryType": "npm", "identifier": "lint-mcp",
                            "version": "1.0.0", "transport": { "type": "stdio" },
                            "environmentVariables": [{ "name": "LINT_TOKEN", "isRequired": true }] }] } },
                    { "server": { "name": "io.example/remote", "description": "lints remotely",
                        "remotes": [{ "type": "streamable-http", "url": "https://x" }] } },
                    { "server": { "name": "io.example/maxwellintl", "packages": [{ "registryType": "npm",
                        "identifier": "maxwellintl", "transport": { "type": "stdio" } }] } }
                ], "metadata": { "count": 3 } }))
            };
            let at = seen.clone();
            let search = move |uri: Uri| async move {
                at.lock().unwrap().push(format!("npm {}", query(&uri, "text")));
                if broken.search {
                    return StatusCode::NOT_FOUND.into_response();
                }
                let object = |name: &str| {
                    json!({ "package": { "name": name, "version": "1.0.0",
                        "description": format!("{name} lints"), "date": ago(1),
                        "links": {}, "maintainers": [] }, "score": { "final": 1.0 }, "searchScore": 1.0 })
                };
                Json(json!({
                    "objects": [object("lint-mcp"), object("fresh-mcp"), object("broken-mcp")],
                    "total": 3, "time": ago(0)
                }))
            };
            let package = move |UrlPath(name): UrlPath<String>| async move {
                if broken.view || name == "broken-mcp" {
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
                match name.as_str() {
                    "lint-mcp" => Json(packument("lint-mcp", &[("0.9.0", 60), ("1.0.0", 30), ("1.1.0", 2)])),
                    "fresh-mcp" => Json(packument("fresh-mcp", &[("0.1.0", 1)])),
                    _ => StatusCode::NOT_FOUND.into_response(),
                }
            };
            let app = Router::new()
                .route("/v0/servers", get(servers))
                .route("/-/v1/search", get(search))
                .route("/{name}", get(package));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap().port()).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });
    (rx.recv().unwrap(), asked)
}

fn project(script: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("agent.toml"),
        "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\n\n[extend]\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("script.json"), script).unwrap();
    std::fs::create_dir(dir.path().join("home")).unwrap();
    dir
}

/// `yobuild run` with the stubs, a home of its own (npm's cache), no keys.
fn run(dir: &Path, port: u16) -> Output {
    run_with(dir, &format!("http://127.0.0.1:{port}"), port)
}

fn run_with(dir: &Path, registry: &str, port: u16) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_yobuild"));
    for key in yobuild::manifest::KEY_VARIABLES {
        command.env_remove(key);
    }
    command
        .args([
            "run", "agent.toml", "--scripted", "script.json", "--once", "lint",
        ])
        .current_dir(dir)
        .env_remove("RUST_LOG")
        .env("HOME", dir.join("home"))
        .env("YOBUILD_HOME", dir.join("home/.yobuild"))
        .env("YOBUILD_MCP_REGISTRY", registry)
        .env("npm_config_registry", format!("http://127.0.0.1:{port}/"))
        .output()
        .unwrap()
}

#[test]
fn find_plugin_returns_candidates_from_both_sources_with_the_version_old_enough() {
    let (port, asked) = stub(Broken::default());
    let dir = project(
        r#"[{ "tools": [{ "name": "find_plugin", "arguments": { "need": "lint code" }, "expect": [
              "Sources: MCP registry: 1 found (2 left out: 1 remote only, 1 naming none of the need's words); npm: 3 found.",
              "1. lint-mcp 1.0.0 (published ",
              "; found in the MCP registry as io.example/lint and npm's search)\n   Lint your code\n",
              "licence MIT; downloads last week: unknown (not asked: npm's registry here is http://127.0.0.1:",
              "repository (claimed by the package): https://example.com/lint-mcp; provenance: none",
              "required arguments: none; required environment variables: LINT_TOKEN",
              "2. fresh-mcp (found in npm's search): not loadable: no version was published 7 or more days ago (the newest, 0.1.0, on ",
              "3. broken-mcp (found in npm's search): not loadable: npm could not say what it is: "
            ] }] },
            { "text": "done" }]"#,
    );
    let out = run(dir.path(), port);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.contains("\n  find_plugin ✓\n"), "{stderr}");
    assert!(!stderr.contains("Warning: find_plugin"), "{stderr}");
    // Each word its own registry search; npm's search the need's words
    // only (npm's own options before its `--`).
    let mut asked = asked.lock().unwrap().clone();
    asked.sort();
    assert_eq!(
        asked,
        ["npm keywords:mcp lint code", "registry code", "registry lint"]
    );
}

#[test]
fn one_source_failing_is_said_and_the_other_still_gives_candidates() {
    for (broken, line, said) in [
        (
            Broken { registry: true, ..Broken::default() },
            "Sources: MCP registry: failed (HTTP 503 Service Unavailable); npm: 3 found.",
            "\n  Warning: find_plugin searched one source only: MCP registry: HTTP 503 Service Unavailable.\n",
        ),
        (
            Broken { search: true, ..Broken::default() },
            "Sources: MCP registry: 1 found (2 left out: 1 remote only, 1 naming none of the need's words); npm: failed (",
            "\n  Warning: find_plugin searched one source only: npm: ",
        ),
    ] {
        let (port, _) = stub(broken);
        let dir = project(&format!(
            r#"[{{ "tools": [{{ "name": "find_plugin", "arguments": {{ "need": "lint" }}, "expect": [
                  {line:?}, "1. lint-mcp 1.0.0 (published "
                ] }}] }},
                {{ "text": "done" }}]"#
        ));
        let out = run(dir.path(), port);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{stderr}");
        assert!(stderr.contains("\n  find_plugin ✓\n"), "{stderr}");
        assert!(stderr.contains(said), "{stderr}");
    }
}

#[test]
fn find_plugin_with_both_sources_failing_is_an_error_and_said_to_the_owner() {
    let (port, _) = stub(Broken {
        registry: true,
        search: true,
        view: false,
    });
    let dir = project(
        r#"[{ "tools": [{ "name": "find_plugin", "arguments": { "need": "lint" }, "expect": [
              "MCP registry: failed (HTTP 503 Service Unavailable); npm: failed (",
              "Both sources failed: this is not \"nothing found\"."
            ] }] },
            { "text": "done" }]"#,
    );
    let out = run(dir.path(), port);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(
        stderr.contains("\n  find_plugin ✗ find_plugin(\"lint\")"),
        "{stderr}"
    );
    assert!(
        stderr.contains("\n  Warning: find_plugin could not search: MCP registry: HTTP 503 Service Unavailable; npm: "),
        "{stderr}"
    );
}

#[test]
fn npm_failing_for_every_candidate_is_an_error_not_ten_unloadable_ones() {
    let (port, _) = stub(Broken {
        view: true,
        ..Broken::default()
    });
    let dir = project(
        r#"[{ "tools": [{ "name": "find_plugin", "arguments": { "need": "lint" }, "expect": [
              "1. lint-mcp (found in the MCP registry as io.example/lint and npm's search): not loadable: npm could not say what it is: ",
              "npm could be asked about none of them: this is not \"none loadable\"."
            ] }] },
            { "text": "done" }]"#,
    );
    let out = run(dir.path(), port);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.contains("\n  find_plugin ✗ "), "{stderr}");
    assert!(
        stderr.contains("\n  Warning: find_plugin could not ask npm about any candidate: "),
        "{stderr}"
    );
}

#[test]
fn a_registry_that_is_not_https_is_refused_before_anything_starts() {
    let dir = project("[]");
    let out = run_with(dir.path(), "http://registry.example.com", 9);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("YOBUILD_MCP_REGISTRY \"http://registry.example.com\""),
        "{stderr}"
    );
    let out = Command::new(env!("CARGO_BIN_EXE_yobuild"))
        .args(["check", "agent.toml"])
        .current_dir(dir.path())
        .env("HOME", dir.path().join("home"))
        .env("YOBUILD_HOME", dir.path().join("home/.yobuild"))
        .env("YOBUILD_MCP_REGISTRY", "http://registry.example.com")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success() && out.stdout.is_empty(), "{stderr}");
    assert!(
        stderr.contains("YOBUILD_MCP_REGISTRY \"http://registry.example.com\""),
        "{stderr}"
    );
}

#[test]
fn check_lists_find_plugin_as_allowed_and_says_what_extend_lets_the_agent_do() {
    let dir = project("[]");
    let out = Command::new(env!("CARGO_BIN_EXE_yobuild"))
        .args(["check", "agent.toml", "--json"])
        .current_dir(dir.path())
        .env("HOME", dir.path().join("home"))
        .env("YOBUILD_HOME", dir.path().join("home/.yobuild"))
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stderr)));
    assert_eq!(report["ok"], true, "{report:#}");
    let agent = &report["agent"];
    assert_eq!(
        agent["extend"],
        json!({ "kinds": ["mcp"], "maxLoads": 2, "maxAttempts": 4, "minAgeDays": 7 })
    );
    let find = agent["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "find_plugin")
        .expect("find_plugin listed");
    assert_eq!(find["kind"], "allowed");
    assert!(
        find["parameters"]["properties"]["need"].is_object(),
        "{find:#}"
    );
    assert!(
        agent["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n.as_str().unwrap().starts_with(
                "[extend]: the agent may search the MCP registry and npm for MCP servers"
            )),
        "{agent:#}"
    );
}
