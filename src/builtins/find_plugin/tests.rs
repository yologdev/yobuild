use super::*;

fn view(versions: &[(&str, &str)], latest: &str) -> Viewed {
    Viewed {
        version: Some(latest.into()),
        latest: Some(latest.into()),
        versions: Some(versions.iter().map(|(v, _)| v.to_string()).collect()),
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
    // npm's `--before`, to the second, as `seconds` reads it back.
    assert_eq!(
        iso(seconds("2026-10-16T17:04:59.463Z").unwrap()),
        "2026-10-16T17:04:59Z"
    );
    assert_eq!(iso(0), "1970-01-01T00:00:00Z");
    assert_eq!(seconds(&iso(951_868_799)), Some(951_868_799));
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
fn latest_bounds_what_is_taken_and_without_it_nothing_is() {
    let all = [
        ("1.4.0", "2026-01-01T00:00:00.000Z"),
        ("1.5.0", "2026-02-01T00:00:00.000Z"),
        ("2.0.0", "2026-03-01T00:00:00.000Z"),
    ];
    // A pre-release `latest`: 2.0.0 comes after it.
    assert_eq!(
        version_to_take(&view(&all, "2.0.0-rc.1"), 7, now()),
        Ok("1.5.0".into())
    );
    // Build metadata: within it.
    assert_eq!(
        version_to_take(&view(&all, "2.0.0+b-1"), 7, now()),
        Ok("2.0.0".into())
    );
    assert_eq!(
        version_to_take(&view(&all, "1.5.0"), 7, now()),
        Ok("1.5.0".into())
    );
    let mut v = view(&all, "2.0.0");
    v.latest = None;
    assert_eq!(
        version_to_take(&v, 7, now()),
        Err("it has no `latest` tag, so the version to take cannot be told".into())
    );
    for bad in ["next", "2.0.0-", "2.0"] {
        assert_eq!(
            version_to_take(&view(&all, bad), 7, now()),
            Err(format!("its `latest` tag, {bad:?}, is not a version"))
        );
    }
    let mut v = view(&all, "2.0.0");
    v.versions = None;
    assert_eq!(
        version_to_take(&v, 7, now()),
        Err("npm gives no list of its versions".into())
    );
    let mut v = view(&all, "2.0.0");
    v.time.insert("2.0.0".into(), "yesterday".into());
    assert_eq!(
        version_to_take(&v, 7, now()),
        Err("npm's publish time for 2.0.0, \"yesterday\", cannot be read, so its age cannot be told".into())
    );
}

#[test]
fn the_registry_gives_npm_servers_over_stdio_and_lists_the_rest() {
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
        ],
        "metadata": { "count": 4 }
    });
    let page = registry_page(&body).unwrap();
    assert!(!page.more);
    let eslint = &page.servers[0];
    assert_eq!(eslint.key, "io.github.eslint/mcp");
    assert_eq!(
        eslint.text,
        "io.github.eslint/mcp @eslint/mcp @eslint/mcp ESLint\nover MCP"
    );
    assert_eq!(
        eslint.hit,
        Some(Hit {
            package: "@eslint/mcp".into(),
            registry_name: Some("io.github.eslint/mcp".into()),
            from_npm: false,
            description: "ESLint\nover MCP".into(),
            required_env: Some(vec!["TOKEN".into()]),
            required_args: Some(vec!["--root".into(), "<dir>".into()]),
        })
    );
    let rest: Vec<(&str, bool, bool)> = page.servers[1..]
        .iter()
        .map(|l| (l.key.as_str(), l.hit.is_some(), l.remote))
        .collect();
    assert_eq!(
        rest,
        [
            ("remote", false, true),
            ("py", false, false),
            ("old-shape", false, false)
        ]
    );
    assert!(registry_page(&serde_json::json!({ "items": [] })).is_err());
    let more = serde_json::json!({ "servers": [], "metadata": { "nextCursor": "abc" } });
    assert!(registry_page(&more).unwrap().more);
}

fn listed(key: &str, text: &str, hit: Option<Hit>, remote: bool) -> Listed {
    Listed {
        key: key.into(),
        text: text.into(),
        hit,
        remote,
    }
}

#[test]
fn the_registrys_searches_are_ranked_and_each_server_counted_once() {
    let words = ["lint".to_string(), "typescript".to_string()];
    let ts = || {
        listed(
            "io.x/ts-lint",
            "ts-lint TypeScript linting",
            Some(hit("ts-lint", true, false)),
            false,
        )
    };
    let pages = vec![
        Ok(Page {
            servers: vec![
                listed(
                    "io.x/lint-mcp",
                    "lint-mcp",
                    Some(hit("lint-mcp", true, false)),
                    false,
                ),
                ts(),
                listed("io.x/remote", "remote lint", None, true),
                listed(
                    "io.x/maxwellintl",
                    "maxwellintl",
                    Some(hit("maxwellintl", true, false)),
                    false,
                ),
            ],
            more: false,
        }),
        Ok(Page {
            servers: vec![
                ts(),
                listed("io.x/remote", "remote lint", None, true),
                listed("io.x/py", "a python typescript checker", None, false),
            ],
            more: true,
        }),
        Err("HTTP 503 Service Unavailable".into()),
    ];
    let found = ranked(&words, pages).unwrap();
    let names: Vec<&str> = found.hits.iter().map(|h| h.package.as_str()).collect();
    assert_eq!(names, ["ts-lint", "lint-mcp"]);
    assert_eq!((found.remote, found.other, found.unrelated), (1, 1, 1));
    assert_eq!(
        found.line(),
        "MCP registry: 2 found (3 left out: 1 remote only, 1 not an npm package over stdio, 1 naming none of the need's words); a search matched more than the 30 it returns; 1 of its 3 searches failed (HTTP 503 Service Unavailable)"
    );
    let failing = vec![
        Err("HTTP 503 Service Unavailable".to_string()),
        Err("HTTP 503 Service Unavailable".into()),
        Err("timed out".into()),
    ];
    assert_eq!(
        ranked(&words, failing),
        Err("HTTP 503 Service Unavailable; timed out".into())
    );
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
    // Only plain or short words: those, as they are.
    assert_eq!(registry_words("MCP server"), vec!["mcp", "server"]);
    assert_eq!(
        registry_words("lint-staged config"),
        vec!["lint-staged", "config"]
    );
    assert_eq!(
        registry_words("one two three four five"),
        vec!["one", "two", "three", "four"]
    );
}

#[test]
fn npm_search_and_view_are_read_as_npm_writes_them() {
    let found = npm_hits(
        "[\n{\"name\":\"@eslint/mcp\",\"version\":\"0.3.15\",\"description\":\"MCP server for ESLint\"}\n,\n{\"name\":\"NOT VALID\"}\n]\n",
    )
    .unwrap();
    assert_eq!(found.hits.len(), 1);
    assert_eq!(found.hits[0].package, "@eslint/mcp");
    assert!(found.hits[0].from_npm && found.hits[0].required_env.is_none());
    assert_eq!(
        found.line(),
        "npm: 1 found (1 left out: 1 with a name yobuild does not take)"
    );
    // npm's `keywords:mcp` only ranks: a package marked by neither an MCP
    // keyword nor "MCP" in its name or description is left out.
    let found = npm_hits(
        r#"[{"name":"@reactuses/core","description":"React hooks","keywords":["react","typescript"]},
            {"name":"fallow","description":"Codebase intelligence: CLI, LSP, and MCP server"},
            {"name":"mcp-use","keywords":["typescript"]},
            {"name":"tool-kit","keywords":["Model-Context-Protocol"]},
            {"name":"gamcp","description":"its name holds the letters only"}]"#,
    )
    .unwrap();
    let names: Vec<&str> = found.hits.iter().map(|h| h.package.as_str()).collect();
    assert_eq!(names, ["fallow", "mcp-use", "tool-kit"]);
    assert_eq!(
        found.line(),
        "npm: 3 found (2 left out: 2 not marked as MCP)"
    );
    assert_eq!(npm_hits("[]").unwrap().line(), "npm: 0 found");
    assert!(npm_hits("npm error").is_err());
    assert_eq!(npm_hits(" \n"), Err("npm search answered nothing".into()));
    // Another shape is an error, never "0 found".
    assert_eq!(
        npm_hits(r#"[{"package":{"name":"x"}}]"#),
        Err("npm search answered a package without a `name` (has its output changed?)".into())
    );

    let v = viewed(
        r#"{"name":"@eslint/mcp","version":"0.3.15","dist-tags":{"latest":"0.3.15"},
            "versions":["0.3.14","0.3.15"],"time":{"created":"x","0.3.15":"2026-10-16T17:04:59.463Z"},
            "license":"Apache-2.0","repository":{"type":"git","url":"git+https://github.com/eslint/rewrite.git"},
            "dist":{"attestations":{"url":"u"}}}"#,
    )
    .unwrap();
    assert_eq!(v.versions, Some(vec!["0.3.14".into(), "0.3.15".into()]));
    assert_eq!(v.licence.as_deref(), Some("Apache-2.0"));
    assert_eq!(
        v.repository.as_deref(),
        Some("https://github.com/eslint/rewrite")
    );
    assert_eq!(v.provenance, Some(true));
    assert!(!v.program, "no `bin`: a library");
    for (bin, program) in [
        (r#""bin":"cli.js""#, true),
        (r#""bin":{"eslint-mcp":"dist/cli.js"}"#, true),
        (r#""bin":{}"#, false),
        (r#""bin":"""#, false),
    ] {
        assert_eq!(
            viewed(&format!("{{{bin}}}")).unwrap().program,
            program,
            "{bin}"
        );
    }
    // One version: `versions` is a string.
    let v = viewed(r#"{"version":"1.0.0","versions":"1.0.0","license":{"type":"MIT"},"dist":{}}"#)
        .unwrap();
    assert_eq!(
        (v.versions, v.licence, v.provenance),
        (
            Some(vec!["1.0.0".to_string()]),
            Some("MIT".to_string()),
            Some(false)
        )
    );
    // The older `licenses` list; no `versions`, `dist` or `time`.
    let v = viewed(r#"{"licenses":[{"type":"MIT"},"Apache-2.0"]}"#).unwrap();
    assert_eq!(
        (v.versions, v.licence, v.provenance, v.time.len()),
        (None, Some("MIT OR Apache-2.0".to_string()), None, 0)
    );
    assert!(viewed("[]").is_err());
    for bad in [
        r#"{"versions":{}}"#, r#"{"versions":[1]}"#, r#"{"time":"x"}"#,
    ] {
        assert!(viewed(bad).is_err(), "{bad}");
    }
}

#[test]
fn the_agent_is_told_each_source_and_what_is_unknown() {
    let found = Found {
        lines: vec!["MCP registry: failed (HTTP 503)".into(), "npm: 2 found".into()],
        failed: vec!["MCP registry: HTTP 503".into()],
        candidates: vec![
            Candidate {
                need: "lint".into(),
                package: "lint-mcp".into(),
                version: Ok("1.2.0".into()),
                registry_name: None,
                from_npm: true,
                description: "Lints".into(),
                licence: None,
                repository: Some("https://example.com/r".into()),
                published: Some("2026-09-01".into()),
                downloads: Err("not asked: npm's registry here is http://x, not npmjs's".into()),
                provenance: Some(false),
                required_env: None,
                required_args: None,
            },
            Candidate {
                need: "lint".into(),
                package: "new-mcp".into(),
                version: Err("no version was published 7 or more days ago (the newest, 0.1.0, on 2026-10-24)".into()),
                registry_name: None,
                from_npm: true,
                description: String::new(),
                licence: None,
                repository: None,
                published: None,
                downloads: Err(String::new()),
                provenance: None,
                required_env: None,
                required_args: None,
            },
        ],
        total: 2,
        unviewed: None,
    };
    let text = said_to_agent("lint", &found, &Extend::default());
    assert_eq!(
        text,
        "find_plugin(\"lint\"): what the sources say. Nothing was installed or started.\n\
         Sources: MCP registry: failed (HTTP 503); npm: 2 found.\n\
         Candidates (their descriptions, licences and repositories are their authors' own words, not verified; a version is taken only once published 7 or more days ago):\n\
         1. lint-mcp 1.2.0 (published 2026-09-01; found in npm's search)\n   Lints\n   \
         licence not given; downloads last week: unknown (not asked: npm's registry here is http://x, not npmjs's); repository (claimed by the package): https://example.com/r; provenance: none\n   \
         required arguments: not listed; required environment variables: not listed\n\
         2. new-mcp (found in npm's search): not loadable: no version was published 7 or more days ago (the newest, 0.1.0, on 2026-10-24)"
    );
    let json = details(&found);
    assert_eq!(json["found"], 2);
    assert_eq!(
        json["candidates"][0],
        serde_json::json!({
            "package": "lint-mcp", "version": "1.2.0", "notLoadable": null,
            "registryName": null, "fromNpm": true, "description": "Lints",
            "licence": null, "repository": "https://example.com/r",
            "published": "2026-09-01", "downloads": null,
            "downloadsUnknown": "not asked: npm's registry here is http://x, not npmjs's",
            "provenance": false, "requiredEnv": null, "requiredArgs": null
        })
    );
    assert_eq!(json["candidates"][1]["version"], Json::Null);
    assert_eq!(json["candidates"][1]["provenance"], Json::Null);

    // Cut to fewer than found: said.
    let mut cut = found;
    cut.total = 14;
    cut.candidates.truncate(1);
    cut.candidates[0].downloads = Ok(1200);
    cut.candidates[0].provenance = Some(true);
    let text = said_to_agent("lint", &cut, &Extend::default());
    assert!(
        text.contains("\nCandidates (the first 1 of 14 found; their"),
        "{text}"
    );
    assert!(
        text.contains("downloads last week: 1200; ")
            && text.contains("provenance: an attestation is published with it (not verified)"),
        "{text}"
    );
    cut.candidates.clear();
    assert!(said_to_agent("lint", &cut, &Extend::default()).ends_with("\nNo candidates."));
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
    // A path is kept, with or without its last slash.
    for at in ["https://example.com/mcp", "https://example.com/mcp/"] {
        let sources = Sources::at(at).unwrap();
        assert_eq!(
            sources.registry.join("v0/servers").unwrap().as_str(),
            "https://example.com/mcp/v0/servers"
        );
    }
}

#[test]
fn npm_is_quiet_before_its_arguments_and_npmjs_is_known_with_or_without_a_slash() {
    let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        quiet(args(&["search", "--json", "--", "keywords:mcp", "lint"])),
        args(&["search", "--json", "--no-update-notifier", "--", "keywords:mcp", "lint"])
    );
    assert_eq!(
        quiet(args(&["view", "x", "--json"])),
        args(&["view", "x", "--json", "--no-update-notifier"])
    );
    assert!(is_npmjs("https://registry.npmjs.org/\n"));
    assert!(is_npmjs("https://registry.npmjs.org"));
    assert!(!is_npmjs("https://registry.npmjs.org.example.com/"));
    assert!(!is_npmjs("http://127.0.0.1:4873/"));
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
}

/// A library (npm says it has no program) is left out and counted; one
/// npm could not be asked about stays, said not loadable; the rest cut to
/// [`MAX_CANDIDATES`], the total counting those not viewed.
#[test]
fn libraries_are_left_out_before_the_cut() {
    let viewing = |name: &str, error: Option<&str>, program: Option<bool>| -> Viewing {
        (
            candidate_of(name, Ok("1.0.0")),
            error.map(String::from),
            program,
        )
    };
    let mut viewed: Vec<Viewing> = (0..12)
        .map(|i| viewing(&format!("s{i}"), None, Some(true)))
        .collect();
    viewed.insert(1, viewing("sdk", None, Some(false)));
    viewed.insert(2, viewing("unviewed", Some("HTTP 500"), None));
    let (kept, total, libraries, unviewed) = cut(viewed, 3);
    let names: Vec<&str> = kept.iter().map(|c| c.package.as_str()).collect();
    assert_eq!(
        names,
        ["s0", "unviewed", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8"]
    );
    assert_eq!((total, libraries, unviewed), (13 + 3, 1, None));
    // Every one unviewed: why (the first's).
    let all = vec![
        viewing("a", Some("HTTP 500"), None),
        viewing("b", Some("HTTP 502"), None),
    ];
    assert_eq!(cut(all, 0).3.as_deref(), Some("HTTP 500"));
    assert_eq!(cut(Vec::new(), 0), (Vec::new(), 0, 0, None));
}

#[test]
fn downloads_are_read_in_one_answer_or_many() {
    let one = vec!["a".to_owned()];
    assert_eq!(
        counts(&serde_json::json!({ "downloads": 7, "package": "a" }), &one),
        [("a".to_owned(), Ok(7))]
    );
    let two = vec!["a".to_owned(), "b".to_owned()];
    let both = counts(
        &serde_json::json!({ "a": { "downloads": 3 }, "b": null }),
        &two,
    );
    assert_eq!(both[0], ("a".to_owned(), Ok(3)));
    assert!(both[1].1.as_ref().unwrap_err().contains("without a count"));
}

#[test]
fn a_registry_find_must_name_the_word_as_a_word() {
    assert!(!mentions("io.x/maxwellintl maxwellintl-mcp", "lint"));
    assert!(mentions("Structured linting output for ESLint", "lint"));
    assert!(mentions("lint-mcp", "lint"));
    assert!(!mentions("lintelly", "lint"));
    assert!(mentions("tools", "tool"));
    // A word of parts: those parts in a row, the ending on the last.
    assert!(mentions("io.x/lint-staged", "lint-staged"));
    assert!(mentions("runs lint_stageds", "lint-staged"));
    assert!(!mentions("staged lint", "lint-staged"));
    assert!(!mentions("lint", "lint-staged"));
    assert!(mentions("An MCP server", "mcp"));
    assert!(!mentions("anything", "-"));
}

fn tool() -> (FindPlugin, crate::frontend::Driver) {
    let (session, driver) = Session::new(false);
    let sources = Sources::at("http://127.0.0.1:9").unwrap();
    (FindPlugin::new(session, Extend::default(), sources), driver)
}

fn candidate_of(package: &str, version: Result<&str, &str>) -> Candidate {
    Candidate {
        need: "lint".into(),
        package: package.into(),
        version: version.map(String::from).map_err(String::from),
        registry_name: None,
        from_npm: true,
        description: String::new(),
        licence: None,
        repository: None,
        published: None,
        downloads: Err(String::new()),
        provenance: None,
        required_env: None,
        required_args: None,
    }
}

#[test]
fn the_session_keeps_each_packages_latest_but_a_failure_never_loses_a_version() {
    let (find, _driver) = tool();
    find.remember(&[
        candidate_of("a", Ok("1.0.0")),
        candidate_of("b", Err("too new")),
    ]);
    find.remember(&[candidate_of(
        "a",
        Err("npm could not say what it is: timed out"),
    )]);
    find.remember(&[candidate_of("b", Ok("0.2.0")), candidate_of("c", Err("x"))]);
    find.remember(&[candidate_of("c", Err("y"))]);
    let kept: Vec<(String, Result<String, String>)> = find
        .candidates()
        .into_iter()
        .map(|c| (c.package, c.version))
        .collect();
    assert_eq!(
        kept,
        [
            ("a".to_string(), Ok("1.0.0".to_string())),
            ("b".into(), Ok("0.2.0".into())),
            ("c".into(), Err("y".into())),
        ]
    );
    find.remember(&[candidate_of("a", Ok("1.1.0"))]);
    assert_eq!(find.candidates()[0].version, Ok("1.1.0".into()));
}

#[tokio::test]
async fn the_arguments_are_refused_before_anything_is_searched() {
    let (find, _driver) = tool();
    let call = |args: Json| {
        let find = find.clone();
        async move {
            match find
                .execute(args, ToolContext::new("1", FindPlugin::TOOL))
                .await
            {
                Err(ToolError::InvalidArgs(why)) => why,
                other => panic!("{other:?}"),
            }
        }
    };
    let cases = [
        (serde_json::json!("lint"), "the arguments must be an object"),
        (
            serde_json::json!({ "need": "lint", "query": "x" }),
            "unknown key `query` (use need, kind)",
        ),
        (
            serde_json::json!({}),
            "`need` is required: the capability, in a few words",
        ),
        (
            serde_json::json!({ "need": " \n " }),
            "`need` is required: the capability, in a few words",
        ),
        (
            serde_json::json!({ "need": ["lint"] }),
            "`need` must be a string, not [\"lint\"]",
        ),
        (
            serde_json::json!({ "need": "--" }),
            "`need` holds no word to search for",
        ),
        (
            serde_json::json!({ "need": "lint", "kind": "pi" }),
            "kind \"pi\": this agent may find \"mcp\"",
        ),
        (
            serde_json::json!({ "need": "lint", "kind": 1 }),
            "kind 1: this agent may find \"mcp\"",
        ),
    ];
    for (args, why) in cases {
        assert_eq!(call(args.clone()).await, why, "{args}");
    }
    assert!(find.candidates().is_empty());
}
