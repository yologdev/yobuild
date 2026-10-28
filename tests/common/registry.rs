//! A stub npm registry (and an empty MCP registry) on one axum server,
//! serving packages packed here with `npm pack`, so an install is the
//! real `npm install --before` and `npm ci` and no test reaches the
//! network. Shared by `load_plugin_test` and `install_one_test`.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::Path as UrlPath;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use serde_json::{json, Value};

pub const DAY: u64 = 24 * 60 * 60;

/// `value` as a JSON response (axum's `json` feature is off).
#[allow(non_snake_case)]
pub fn Json(value: Value) -> Response {
    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        value.to_string(),
    )
        .into_response()
}

/// An ISO time `days` ago, as npm writes them.
pub fn ago(days: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let t = now - days * DAY;
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

/// One version of a package, packed: what its packument says of it, its
/// tarball, and when it was published.
pub struct Packed {
    pub version: String,
    pub days: u64,
    /// Its `package.json`.
    pub manifest: Value,
    pub tarball: Vec<u8>,
    pub integrity: String,
    pub shasum: String,
}

/// `name@version`, published `days` ago, packed with `npm pack`: `files`
/// (path, text) in it, `extra` merged into its `package.json`.
pub fn pack(
    name: &str,
    version: &str,
    days: u64,
    files: &[(&str, String)],
    extra: Value,
) -> Packed {
    let dir = tempfile::tempdir().unwrap();
    let package = dir.path().join("package");
    std::fs::create_dir(&package).unwrap();
    let mut manifest = json!({ "name": name, "version": version,
        "description": format!("{name} echoes"), "license": "MIT" });
    for (key, value) in extra.as_object().unwrap() {
        manifest[key] = value.clone();
    }
    for (path, text) in files {
        let path = package.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    std::fs::write(package.join("package.json"), manifest.to_string()).unwrap();
    let out = Command::new("npm")
        .args(["pack", "--json", "--ignore-scripts", "--pack-destination"])
        .arg(dir.path())
        .current_dir(&package)
        .env("HOME", dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "npm pack: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let said: Value = serde_json::from_slice(&out.stdout).unwrap();
    let said = &said[0];
    Packed {
        version: version.to_owned(),
        days,
        manifest,
        tarball: std::fs::read(dir.path().join(said["filename"].as_str().unwrap())).unwrap(),
        integrity: said["integrity"].as_str().unwrap().to_owned(),
        shasum: said["shasum"].as_str().unwrap().to_owned(),
    }
}

/// The packages the stub serves, by name: their versions, the last the
/// latest.
pub type Packages = Arc<BTreeMap<String, Vec<Packed>>>;

/// What the stub was asked for, by package, in order.
pub type Fetched = Arc<Mutex<Vec<String>>>;

/// The stub registries, serving until the process ends.
pub struct Stub {
    pub port: u16,
    /// The packages whose tarball was fetched.
    pub tarballs: Fetched,
    /// The packages whose packument (`npm view`, a resolve) was asked for,
    /// served or not.
    pub packuments: Fetched,
}

/// The stub registries on a port of their own; the tarballs of `slow`
/// packages answer only after a minute.
pub fn stub(packages: Packages, slow: &[&str]) -> Stub {
    let slow: Arc<BTreeSet<String>> = Arc::new(slow.iter().map(|s| s.to_string()).collect());
    let fetched = Fetched::default();
    let packuments = Fetched::default();
    let (tx, rx) = std::sync::mpsc::channel();
    let asked = fetched.clone();
    let viewed = packuments.clone();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async move {
            let servers = || async { Json(json!({ "servers": [], "metadata": { "count": 0 } })) };
            let names: Vec<String> = packages.keys().cloned().collect();
            let search = move || {
                let names = names.clone();
                async move {
                    let objects: Vec<Value> = names
                        .iter()
                        .map(|name| {
                            json!({ "package": { "name": name, "version": "1.0.0",
                                "description": format!("{name} echoes"), "date": ago(60),
                                "links": {}, "maintainers": [] }, "score": { "final": 1.0 }, "searchScore": 1.0 })
                        })
                        .collect();
                    Json(json!({ "objects": objects, "total": names.len(), "time": ago(0) }))
                }
            };
            let port = Arc::new(std::sync::OnceLock::<u16>::new());
            let (at, served) = (port.clone(), packages.clone());
            let package = move |UrlPath(name): UrlPath<String>| async move {
                viewed.lock().unwrap().push(name.clone());
                let Some(all) = served.get(&name) else {
                    return StatusCode::NOT_FOUND.into_response();
                };
                let port = at.get().unwrap();
                let mut versions = serde_json::Map::new();
                let mut time = serde_json::Map::new();
                for packed in all {
                    let v = &packed.version;
                    let mut entry = packed.manifest.clone();
                    entry["dist"] = json!({
                        "tarball": format!("http://127.0.0.1:{port}/{name}/-/{name}-{v}.tgz"),
                        "integrity": packed.integrity, "shasum": packed.shasum
                    });
                    versions.insert(v.clone(), entry);
                    time.insert(v.clone(), json!(ago(packed.days)));
                }
                let oldest = all.iter().map(|p| p.days).max().unwrap();
                time.insert("created".into(), json!(ago(oldest)));
                time.insert("modified".into(), json!(ago(all.last().unwrap().days)));
                Json(json!({
                    "name": name, "description": format!("{name} echoes"),
                    "dist-tags": { "latest": all.last().unwrap().version },
                    "versions": versions, "time": time
                }))
            };
            let served = packages.clone();
            let tarball = move |UrlPath((name, file)): UrlPath<(String, String)>| async move {
                asked.lock().unwrap().push(name.clone());
                if slow.contains(&name) {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                }
                let packed = served
                    .get(&name)
                    .and_then(|all| all.iter().find(|p| file == format!("{name}-{}.tgz", p.version)));
                match packed {
                    Some(packed) => (
                        [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
                        packed.tarball.clone(),
                    )
                        .into_response(),
                    None => StatusCode::NOT_FOUND.into_response(),
                }
            };
            let app = Router::new()
                .route("/v0/servers", get(servers))
                .route("/-/v1/search", get(search))
                .route("/{name}", get(package))
                .route("/{name}/-/{file}", get(tarball));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let bound = listener.local_addr().unwrap().port();
            port.set(bound).unwrap();
            tx.send(bound).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });
    Stub {
        port: rx.recv().unwrap(),
        tarballs: fetched,
        packuments,
    }
}
