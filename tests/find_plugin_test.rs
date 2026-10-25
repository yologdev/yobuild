//! `find_plugin` (`[extend]`, design §14) through the binary, against a
//! stub MCP registry and a stub npm registry (one axum server): no test
//! reaches the network. npm is the real CLI, pointed at the stub by
//! `npm_config_registry` (a setting `install` keeps). Unix, as `cli_test`.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::Path as UrlPath;
use axum::http::StatusCode;
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
    // Civil from days (as the tool's own `day`).
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

/// The stub registries on a port of their own, serving until the process
/// ends; `broken` answers 503 for the MCP registry and 404 for npm's search.
fn stub(broken: bool) -> u16 {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async move {
            let servers = move || async move {
                if broken {
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                }
                Json(json!({ "servers": [
                    { "server": { "name": "io.example/lint", "description": "Lint\nyour code",
                        "packages": [{ "registryType": "npm", "identifier": "lint-mcp",
                            "version": "1.0.0", "transport": { "type": "stdio" },
                            "environmentVariables": [{ "name": "LINT_TOKEN", "isRequired": true }] }] } },
                    { "server": { "name": "io.example/remote",
                        "remotes": [{ "type": "streamable-http", "url": "https://x" }] } }
                ], "metadata": { "count": 2 } }))
                .into_response()
            };
            let search = move || async move {
                if broken {
                    return StatusCode::NOT_FOUND.into_response();
                }
                let object = |name: &str| {
                    json!({ "package": { "name": name, "version": "1.0.0",
                        "description": format!("{name} lints"), "date": ago(1),
                        "links": {}, "maintainers": [] }, "score": { "final": 1.0 }, "searchScore": 1.0 })
                };
                Json(json!({
                    "objects": [object("lint-mcp"), object("fresh-mcp")],
                    "total": 2, "time": ago(0)
                }))
                .into_response()
            };
            let package = |UrlPath(name): UrlPath<String>| async move {
                match name.as_str() {
                    "lint-mcp" => Json(packument("lint-mcp", &[("0.9.0", 60), ("1.0.0", 30), ("1.1.0", 2)]))
                        .into_response(),
                    "fresh-mcp" => Json(packument("fresh-mcp", &[("0.1.0", 1)])).into_response(),
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
    rx.recv().unwrap()
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
        .env("YOBUILD_MCP_REGISTRY", format!("http://127.0.0.1:{port}"))
        .env("npm_config_registry", format!("http://127.0.0.1:{port}/"))
        .output()
        .unwrap()
}

#[test]
fn find_plugin_returns_candidates_from_both_sources_with_the_version_old_enough() {
    let port = stub(false);
    let dir = project(
        r#"[{ "tools": [{ "name": "find_plugin", "arguments": { "need": "lint code" }, "expect": [
              "Sources: MCP registry: 1 found (1 left out: 1 remote only); npm: 2 found.",
              "1. lint-mcp 1.0.0 (published ",
              "; found in the MCP registry as io.example/lint and npm's search)\n   Lint your code\n",
              "licence MIT; downloads last week: unknown; repository (claimed by the package): https://example.com/lint-mcp; provenance: none",
              "required arguments: none; required environment variables: LINT_TOKEN",
              "2. fresh-mcp (found in npm's search): not loadable: no version was published 7 or more days ago (the newest, 0.1.0, on "
            ] }] },
            { "text": "done" }]"#,
    );
    let out = run(dir.path(), port);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.contains("\n  find_plugin ✓\n"), "{stderr}");
}

#[test]
fn find_plugin_with_both_sources_failing_is_an_error_and_said_to_the_owner() {
    let port = stub(true);
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
