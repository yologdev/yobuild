use super::*;

fn view(versions: &[(&str, &str)], latest: &str) -> Viewed {
    Viewed {
        version: Some(latest.into()),
        latest: Some(latest.into()),
        versions: versions.iter().map(|(v, _)| v.to_string()).collect(),
        time: versions
            .iter()
            .map(|(v, t)| (v.to_string(), t.to_string()))
            .collect(),
        ..Viewed::default()
    }
}

const NOW: &str = "2026-10-25T12:00:00.000Z";

fn now() -> i64 {
    seconds(NOW).unwrap()
}

#[test]
fn times_read_as_npm_writes_them_and_days_said_back() {
    assert_eq!(seconds("1970-01-01T00:00:00.000Z"), Some(0));
    assert_eq!(seconds("1970-01-02T00:00:01Z"), Some(DAY + 1));
    assert_eq!(seconds("2000-03-01T00:00:00.000Z"), Some(951_868_800));
    assert_eq!(
        day(seconds("2026-10-16T17:04:59.463Z").unwrap()),
        "2026-10-16"
    );
    assert_eq!(
        day(seconds("2024-02-29T23:59:59.999Z").unwrap()),
        "2024-02-29"
    );
    for bad in [
        "2026-10-16", "2026-13-01T00:00:00Z", "2026-10-16T17:04:59", "soon",
    ] {
        assert_eq!(seconds(bad), None, "{bad}");
    }
}

#[test]
fn the_version_taken_is_the_newest_release_old_enough_and_not_past_latest() {
    let v = view(
        &[
            ("0.3.13", "2026-09-01T00:00:00.000Z"),
            ("0.3.14", "2026-10-10T00:00:00.000Z"),
            ("0.3.15", "2026-10-24T00:00:00.000Z"),
            ("0.4.0-beta.1", "2026-09-02T00:00:00.000Z"),
            ("1.0.0", "2026-09-03T00:00:00.000Z"),
        ],
        "0.3.15",
    );
    assert_eq!(version_to_take(&v, 7, now()), Ok("0.3.14".into()));
    assert_eq!(version_to_take(&v, 0, now()), Ok("0.3.15".into()));
    assert_eq!(version_to_take(&v, 30, now()), Ok("0.3.13".into()));
    assert_eq!(
        version_to_take(&v, 90, now()),
        Err(
            "no version was published 90 or more days ago (the newest, 0.3.15, on 2026-10-24)"
                .into()
        )
    );
}

#[test]
fn a_version_without_a_publish_time_or_only_pre_releases_is_not_taken() {
    let mut v = view(&[("1.0.0", "2026-01-01T00:00:00.000Z")], "1.0.0");
    v.time.clear();
    assert_eq!(
        version_to_take(&v, 7, now()),
        Err("npm gives no publish time for 1.0.0, so its age cannot be told".into())
    );
    assert_eq!(version_to_take(&v, 0, now()), Ok("1.0.0".into()));
    let v = view(&[("1.0.0-rc.1", "2026-01-01T00:00:00.000Z")], "1.0.0-rc.1");
    assert_eq!(
        version_to_take(&v, 0, now()),
        Err("it has no release version (only pre-releases, or none)".into())
    );
    assert_eq!(release("01.2.3"), None);
    assert_eq!(release("1.2"), None);
    assert_eq!(release("1.2.3.4"), None);
    assert_eq!(release("10.0.1"), Some((10, 0, 1)));
}

#[test]
fn the_registry_gives_npm_servers_over_stdio_and_counts_the_rest() {
    let body = serde_json::json!({
        "servers": [
            { "server": {
                "name": "io.github.eslint/mcp",
                "description": "ESLint\nover MCP",
                "packages": [
                    { "registryType": "npm", "identifier": "@eslint/mcp", "version": "0.3.15",
                      "transport": { "type": "sse" } },
                    { "registryType": "npm", "identifier": "@eslint/mcp", "version": "0.3.15",
                      "transport": { "type": "stdio" },
                      "environmentVariables": [
                        { "name": "TOKEN", "isRequired": true },
                        { "name": "COLOR" }
                      ],
                      "packageArguments": [
                        { "type": "named", "name": "--root", "isRequired": true },
                        { "type": "positional", "valueHint": "dir", "isRequired": true },
                        { "type": "named", "name": "--quiet" }
                      ] }
                ] } },
            { "server": { "name": "remote", "remotes": [{ "type": "streamable-http", "url": "https://x" }] } },
            { "server": { "name": "py", "packages": [{ "registryType": "pypi", "identifier": "p",
                "transport": { "type": "stdio" } }] } },
            { "name": "old-shape", "packages": [{ "registryType": "npm", "identifier": "Bad Name",
                "transport": { "type": "stdio" } }] }
        ]
    });
    let found = registry_found(&body).unwrap();
    assert_eq!((found.remote, found.other), (1, 2));
    assert_eq!(
        found.hits,
        vec![Hit {
            package: "@eslint/mcp".into(),
            registry_name: Some("io.github.eslint/mcp".into()),
            from_npm: false,
            description: "ESLint\nover MCP".into(),
            required_env: Some(vec!["TOKEN".into()]),
            required_args: Some(vec!["--root".into(), "<dir>".into()]),
        }]
    );
    assert_eq!(
        found.line(),
        "MCP registry: 1 found (3 left out: 1 remote only, 2 not an npm package over stdio)"
    );
    assert!(registry_found(&serde_json::json!({ "items": [] })).is_err());
}

#[test]
fn the_registry_is_searched_by_the_needs_own_words() {
    assert_eq!(
        registry_words("Lint TypeScript"),
        vec!["lint", "typescript"]
    );
    assert_eq!(
        registry_words("an MCP server for reading xlsx files, xlsx too"),
        vec!["reading", "xlsx", "files", "too"]
    );
    assert_eq!(registry_words("db"), vec!["db"]);
}

#[test]
fn npm_search_and_view_are_read_as_npm_writes_them() {
    let hits = npm_hits(
        "[\n{\"name\":\"@eslint/mcp\",\"version\":\"0.3.15\",\"description\":\"MCP server for ESLint\"}\n,\n{\"name\":\"NOT VALID\"}\n]\n",
    )
    .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].package, "@eslint/mcp");
    assert!(hits[0].from_npm && hits[0].required_env.is_none());
    assert!(npm_hits("npm error").is_err());

    let v = viewed(
        r#"{"name":"@eslint/mcp","version":"0.3.15","dist-tags":{"latest":"0.3.15"},
            "versions":["0.3.14","0.3.15"],"time":{"created":"x","0.3.15":"2026-10-16T17:04:59.463Z"},
            "license":"Apache-2.0","repository":{"type":"git","url":"git+https://github.com/eslint/rewrite.git"},
            "dist":{"attestations":{"url":"u"}}}"#,
    )
    .unwrap();
    assert_eq!(v.versions, vec!["0.3.14", "0.3.15"]);
    assert_eq!(v.licence.as_deref(), Some("Apache-2.0"));
    assert_eq!(
        v.repository.as_deref(),
        Some("https://github.com/eslint/rewrite")
    );
    assert!(v.provenance);
    // One version: `versions` is a string.
    let v = viewed(r#"{"version":"1.0.0","versions":"1.0.0","license":{"type":"MIT"}}"#).unwrap();
    assert_eq!(
        (v.versions, v.licence),
        (vec!["1.0.0".to_string()], Some("MIT".to_string()))
    );
    assert!(viewed("[]").is_err());
}

#[test]
fn the_agent_is_told_each_source_and_what_is_unknown() {
    let found = Found {
        lines: vec!["MCP registry: failed (HTTP 503)".into(), "npm: 2 found".into()],
        failed: vec!["MCP registry: HTTP 503".into()],
        candidates: vec![
            Candidate {
                package: "lint-mcp".into(),
                version: Ok("1.2.0".into()),
                registry_name: None,
                from_npm: true,
                description: "Lints".into(),
                licence: None,
                repository: Some("https://example.com/r".into()),
                published: Some("2026-09-01".into()),
                downloads: None,
                provenance: false,
                required_env: None,
                required_args: None,
            },
            Candidate {
                package: "new-mcp".into(),
                version: Err("no version was published 7 or more days ago (the newest, 0.1.0, on 2026-10-24)".into()),
                registry_name: None,
                from_npm: true,
                description: String::new(),
                licence: None,
                repository: None,
                published: None,
                downloads: None,
                provenance: false,
                required_env: None,
                required_args: None,
            },
        ],
    };
    let text = said_to_agent("lint", &found, &Extend::default());
    assert_eq!(
        text,
        "find_plugin(\"lint\"): what the sources say. Nothing was installed or started.\n\
         Sources: MCP registry: failed (HTTP 503); npm: 2 found.\n\
         Candidates (their descriptions, licences and repositories are their authors' own words, not verified; a version is taken only once published 7 or more days ago):\n\
         1. lint-mcp 1.2.0 (published 2026-09-01; found in npm's search)\n   Lints\n   \
         licence not given; downloads last week: unknown; repository (claimed by the package): https://example.com/r; provenance: none\n   \
         required arguments: not listed; required environment variables: not listed\n\
         2. new-mcp (found in npm's search): not loadable: no version was published 7 or more days ago (the newest, 0.1.0, on 2026-10-24)"
    );
    assert_eq!(details(&found)["candidates"][1]["version"], Json::Null);
}

#[test]
fn clip_keeps_one_line_and_marks_a_cut() {
    assert_eq!(clip("  a\nb\t\u{1b}[31mc  ", 20), "a b [31mc");
    assert_eq!(clip("abcdefghij", 10), "abcdefghij");
    assert_eq!(clip("abcdefghijk", 10), "abcdefghi…");
    assert_eq!(clip("abcd efghijk", 6), "abcd…");
}

#[test]
fn the_registry_may_be_another_only_over_https_or_on_this_machine() {
    assert!(Sources::at("https://registry.example.com").is_ok());
    assert!(Sources::at("http://127.0.0.1:9").is_ok());
    let e = Sources::at("http://registry.example.com").unwrap_err();
    assert!(e.starts_with("YOBUILD_MCP_REGISTRY "), "{e}");
}

fn hit(package: &str, registry: bool, npm: bool) -> Hit {
    Hit {
        package: package.into(),
        registry_name: registry.then(|| format!("io.example/{package}")),
        from_npm: npm,
        description: String::new(),
        required_env: registry.then(Vec::new),
        required_args: registry.then(Vec::new),
    }
}

#[test]
fn both_sources_finds_come_first_then_each_sources_in_turn() {
    let registry = vec![
        hit("r1", true, false),
        hit("both", true, false),
        hit("r2", true, false),
    ];
    let npm = vec![
        hit("n1", false, true),
        hit("both", false, true),
        hit("n2", false, true),
    ];
    let first = merged(registry, npm);
    let names: Vec<&str> = first.iter().map(|h| h.package.as_str()).collect();
    assert_eq!(names, ["both", "r1", "n1", "r2", "n2"]);
    assert!(first[0].from_npm && first[0].registry_name.is_some());
    let many: Vec<Hit> = (0..8).map(|i| hit(&format!("r{i}"), true, false)).collect();
    let more: Vec<Hit> = (0..8).map(|i| hit(&format!("n{i}"), false, true)).collect();
    assert_eq!(merged(many, more).len(), MAX_CANDIDATES);
}

#[test]
fn a_registry_find_must_name_the_word_as_a_word() {
    let mut h = hit("maxwellintl-mcp", true, false);
    assert!(!mentions(&h, "lint"));
    h.description = "Structured linting output for ESLint".into();
    assert!(mentions(&h, "lint"), "linting");
    assert!(mentions(&hit("lint-mcp", false, false), "lint"));
    assert!(!mentions(&hit("lintelly", false, false), "lint"));
    assert!(mentions(&hit("tools", false, false), "tool"));
}
