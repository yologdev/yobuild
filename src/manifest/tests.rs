//! The manifest read strictly: each rule, with the exact place and message
//! of its error.

use super::*;

fn read(text: &str) -> Result<Manifest, ManifestError> {
    Manifest::parse(text, Path::new("agent.toml"), Path::new("/agents"))
}

/// The error as `line:col: message`.
fn error(text: &str) -> String {
    let e = read(text).expect_err("should be refused");
    let (line, col) = e.at.expect("a place");
    format!("{line}:{col}: {}", e.message)
}

const MINIMAL: &str = "[agent]\nname = \"a\"\nmodel = \"deepseek/deepseek-flash\"\n";

fn with(extra: &str) -> String {
    format!("{MINIMAL}{extra}")
}

fn package(name: &str, version: &str) -> Source {
    Source::Package(Package {
        name: name.into(),
        version: version.into(),
    })
}

#[test]
fn a_full_manifest_reads_into_every_field() {
    let m = read(
        r#"
[agent]
name   = "handoff"
model  = "anthropic/claude-sonnet-5"
prompt = "./prompts/handoff.md"

[limits]
max_turns   = 80
max_tokens  = 4_000_000
max_minutes = 30
budget_usd  = 2.5

[[plugin]]
pi = "@juicesharp/rpiv-todo@2.12.0"

[[plugin]]
dsh    = "dsh-free-search@0.8.1"
config = { provider = "bing", since = 1979-05-27 }

[[plugin]]
name    = "words"
rutis   = "plugins/word_count.ts"
runtime = "bun"

[[mcp]]
npm  = "@modelcontextprotocol/server-filesystem@1.2.3"
bin  = "mcp-fs"
args = ["."]
env  = { MODE = "fast" }

[[mcp]]
url     = "https://mcp.example.com/mcp"
headers = { Authorization = "Bearer ${EXAMPLE_MCP_TOKEN}", Accept = "application/json" }

[parts]
ask_owner = true
report    = ["Summary", "Next"]
facts     = true
gate      = "off"
scratch   = false

[dsh]
host_services = true

[frontend]
web   = true
phone = true
"#,
    )
    .unwrap();
    assert_eq!(m.dir, PathBuf::from("/agents"));
    assert_eq!(m.agent.name, "handoff");
    assert_eq!(m.agent.model.provider(), "anthropic");
    assert_eq!(m.agent.model.id(), "claude-sonnet-5");
    assert_eq!(m.agent.model.key_env(), ["ANTHROPIC_API_KEY"]);
    assert_eq!(
        m.agent.prompt,
        Some(PathBuf::from("/agents/prompts/handoff.md"))
    );
    assert_eq!(
        m.limits,
        Limits {
            max_turns: Some(80),
            max_tokens: Some(4_000_000),
            max_minutes: Some(30),
            budget_usd: Some(2.5),
        }
    );
    assert_eq!(
        m.plugins,
        [
            Plugin {
                name: "rpiv-todo".into(),
                kind: PluginKind::Pi(package("@juicesharp/rpiv-todo", "2.12.0")),
                config: None,
            },
            Plugin {
                name: "dsh-free-search".into(),
                kind: PluginKind::Dsh(package("dsh-free-search", "0.8.1")),
                config: serde_json::json!({ "provider": "bing", "since": "1979-05-27" })
                    .as_object()
                    .cloned(),
            },
            Plugin {
                name: "words".into(),
                kind: PluginKind::Rutis {
                    path: PathBuf::from("/agents/plugins/word_count.ts"),
                    runtime: Runtime::Bun,
                },
                config: None,
            },
        ]
    );
    assert_eq!(
        m.mcp,
        [
            Mcp {
                name: "server-filesystem".into(),
                transport: Transport::Stdio {
                    source: package("@modelcontextprotocol/server-filesystem", "1.2.3"),
                    bin: Some("mcp-fs".into()),
                    args: vec![Value::new(".").unwrap()],
                    env: [("MODE".into(), Value::new("fast").unwrap())].into(),
                },
            },
            Mcp {
                name: "mcp.example.com".into(),
                transport: Transport::Http {
                    url: Value::new("https://mcp.example.com/mcp").unwrap(),
                    headers: [
                        ("Accept".into(), Value::new("application/json").unwrap()),
                        (
                            "Authorization".into(),
                            Value::new("Bearer ${EXAMPLE_MCP_TOKEN}").unwrap()
                        ),
                    ]
                    .into(),
                },
            },
        ]
    );
    assert_eq!(
        m.parts,
        Parts {
            ask_owner: true,
            report: Some(Report::new(["Summary", "Next"]).unwrap()),
            facts: true,
            gate: Gate::Off,
            scratch: false,
        }
    );
    assert_eq!(
        m.dsh,
        Dsh {
            host_services: true
        }
    );
    assert_eq!(
        m.frontend,
        Frontend {
            web: true,
            phone: true,
            terminal: false
        }
    );
}

#[test]
fn the_smallest_manifest_has_the_gate_on() {
    let m = read(MINIMAL).unwrap();
    assert_eq!(m.parts, Parts::default());
    assert_eq!(m.parts.gate, Gate::AllowList);
    assert!(m.plugins.is_empty() && m.mcp.is_empty());
    assert_eq!(m.limits, Limits::default());
    assert_eq!(m.frontend, Frontend::default());
}

#[test]
fn an_error_is_shown_with_its_file_line_and_column() {
    let e = read(&with("[parts]\ngat = \"off\"\n")).unwrap_err();
    assert_eq!(
        e.to_string(),
        "agent.toml:5:1: unknown field `gat`, expected one of `ask_owner`, `report`, `facts`, `gate`, `scratch`"
    );
    let e = Manifest::load("/no/such/agent.toml").unwrap_err();
    assert_eq!(e.at, None);
    assert!(e.to_string().starts_with("/no/such/agent.toml: "), "{e}");
}

#[test]
fn unknown_keys_are_errors_where_they_are() {
    assert!(
        error("[agent]\nname = \"a\"\nmodel = \"x/y\"\nmodle = \"z\"\n")
            .starts_with("4:1: unknown field `modle`")
    );
    assert!(error(&with("[[plugin]]\npi = \"a@1.0.0\"\nconfg = {}\n"))
        .starts_with("6:1: unknown field `confg`"));
    assert!(error("[agnet]\n").starts_with("1:2: unknown field `agnet`"));
}

#[test]
fn columns_count_characters() {
    assert_eq!(
        error(&with(
            "[[plugin]]\nconfig = { k = \"日本\" }\npi = \"x@latest\"\n"
        )),
        "6:6: \"x@latest\" is not pinned: give an exact version (`x@1.2.3`)"
    );
    assert_eq!(
        error(&with("[[plugin]]\npi = \"日本@1.0.0\"\n")),
        "5:6: \"日本\" is not an npm package name (lowercase letters, digits, `-._~`, an optional `@scope/`)"
    );
}

#[test]
fn plugins_and_servers_are_pinned_or_local() {
    for spec in [
        "rpiv-todo", "rpiv-todo@latest", "rpiv-todo@^2.12.0", "rpiv-todo@2.12", "@scope/x",
        "@scope/x@~1.0.0", "x@1.2.3-", "x@1.2.3+", "x@01.2.3", "x@1.2.3.4", "x@1..3", "x@1.2.3-01",
    ] {
        let e = error(&with(&format!("[[plugin]]\npi = \"{spec}\"\n")));
        assert!(
            e.starts_with("5:6: ") && e.contains("is not pinned"),
            "{spec}: {e}"
        );
    }
    for spec in [
        "x@1.2.3", "@scope/x@1.2.3", "x@1.2.3-beta.1", "x@1.2.3+build.5", "x@0.0.0", "./local",
        "../up/x.ts", "/abs/x",
    ] {
        read(&with(&format!("[[plugin]]\ndsh = \"{spec}\"\n")))
            .unwrap_or_else(|e| panic!("{spec}: {e}"));
    }
    let m = read(&with("[[plugin]]\ndsh = \"./a/./b\"\n")).unwrap();
    assert_eq!(
        m.plugins[0].kind,
        PluginKind::Dsh(Source::Path("/agents/a/b".into()))
    );
    assert_eq!(
        error(&with("[[mcp]]\nnpm = \"server@latest\"\n")),
        "5:7: \"server@latest\" is not pinned: give an exact version (`server@1.2.3`)"
    );
}

#[test]
fn only_registry_packages_with_npm_names() {
    for spec in [
        "https://evil.example/p@1.2.3",
        "git+ssh://git@github.com/x/y@1.0.0",
        "github:x/y@1.0.0",
        "file:../x@1.0.0",
        "npm:foo@1.2.3",
    ] {
        assert_eq!(
            error(&with(&format!("[[plugin]]\npi = \"{spec}\"\n"))),
            format!(
                "5:6: {spec:?} is not a registry package: give `name@1.2.3`, or a path (`./…`)"
            )
        );
    }
    for name in ["a/b", "..", "@scope", "X Y", "x@1.2.3", "_x", "Up"] {
        let e = error(&with(&format!("[[plugin]]\npi = \"{name}@1.2.3\"\n")));
        assert_eq!(
            e,
            format!("5:6: {name:?} is not an npm package name (lowercase letters, digits, `-._~`, an optional `@scope/`)")
        );
    }
}

#[test]
fn a_plugin_names_one_ecosystem_and_a_rutis_one_its_runtime() {
    assert_eq!(
        error(&with("[[plugin]]\npi = \"a@1.0.0\"\ndsh = \"b@1.0.0\"\n")),
        "6:7: a [[plugin]] names exactly one of pi, dsh or rutis"
    );
    assert_eq!(
        error(&with("[[plugin]]\nconfig = {}\n")),
        "4:1: a [[plugin]] names exactly one of pi, dsh or rutis"
    );
    assert_eq!(
        error(&with("[[plugin]]\nrutis = \"./x.ts\"\n")),
        "5:9: a rutis plugin needs a runtime: node, bun, python or go"
    );
    assert_eq!(
        error(&with(
            "[[plugin]]\nrutis = \"x@1.0.0\"\nruntime = \"node\"\n"
        )),
        "5:9: \"x@1.0.0\": a rutis plugin is a local file (`./plugin.ts`)"
    );
    assert_eq!(
        error(&with("[[plugin]]\npi = \"a@1.0.0\"\nruntime = \"node\"\n")),
        "5:6: runtime is for a rutis plugin"
    );
    assert!(error(&with(
        "[[plugin]]\nrutis = \"./x.ts\"\nruntime = \"ruby\"\n"
    ))
    .contains("unknown variant `ruby`"));
    assert_eq!(
        error(&with("[[plugin]]\npi = \"a@1.0.0\"\nconfig = 5\n")),
        "6:10: config is a table: the plugin's own settings"
    );
    let m = read(&with(
        "[[plugin]]\nrutis = \"x.ts\"\nruntime = \"go\"\nconfig = { a = 1 }\n",
    ))
    .unwrap();
    assert_eq!(
        m.plugins[0],
        Plugin {
            name: "x".into(),
            kind: PluginKind::Rutis {
                path: "/agents/x.ts".into(),
                runtime: Runtime::Go
            },
            config: serde_json::json!({ "a": 1 }).as_object().cloned(),
        }
    );
}

#[test]
fn names_are_unique_and_plain() {
    assert_eq!(
        error(&with(
            "[[plugin]]\npi = \"@a/tool@1.0.0\"\n[[plugin]]\ndsh = \"@b/tool@1.0.0\"\n"
        )),
        "7:7: two plugins or MCP servers are named \"tool\": give one a `name`"
    );
    assert_eq!(
        error(&with(
            "[[plugin]]\npi = \"tool@1.0.0\"\n[[mcp]]\nname = \"tool\"\nurl = \"https://x/mcp\"\n"
        )),
        "7:8: two plugins or MCP servers are named \"tool\": give one a `name`"
    );
    read(&with(
        "[[plugin]]\npi = \"@a/tool@1.0.0\"\n[[plugin]]\nname = \"tool-b\"\ndsh = \"@b/tool@1.0.0\"\n",
    ))
    .unwrap();
    assert_eq!(
        error(&with("[[plugin]]\nname = \"a b\"\npi = \"x@1.0.0\"\n")),
        "5:8: name \"a b\": letters, digits, `-`, `_`, `.`"
    );
}

#[test]
fn models_are_provider_and_id() {
    assert_eq!(
        error("[agent]\nname = \"a\"\nmodel = \"deepseek-flash\"\n"),
        "3:9: model \"deepseek-flash\" is not `provider/id`"
    );
    assert_eq!(
        error("[agent]\nname = \"a\"\nmodel = \"/x\"\n"),
        "3:9: model \"/x\" is not `provider/id`"
    );
    assert_eq!(
        error("[agent]\nname = \"a\"\nmodel = \"deepseek/\"\n"),
        "3:9: model \"deepseek/\" is not `provider/id`"
    );
    assert!(
        error("[agent]\nname = \"a\"\nmodel = \"acme/m1\"\n").starts_with(
            "3:9: provider \"acme\" is not one yoagent knows (anthropic, openai, deepseek"
        )
    );
    // Without base_url, api_key_env alone is not enough.
    assert!(
        error("[agent]\nname = \"a\"\nmodel = \"acme/m1\"\napi_key_env = \"K\"\n")
            .starts_with("3:9: provider \"acme\" is not one yoagent knows")
    );
    assert_eq!(
        error(
            "[agent]\nname = \"a\"\nmodel = \"acme/m1\"\nbase_url = \"https://acme.example/v1\"\n"
        ),
        "4:12: acme needs api_key_env: the environment variable holding its key"
    );
    let m = read("[agent]\nname = \"a\"\nmodel = \"acme/meta-llama/llama-3\"\nbase_url = \"https://acme.example/v1\"\napi_key_env = \"ACME_KEY\"\n").unwrap();
    assert_eq!(m.agent.model.id(), "meta-llama/llama-3");
    assert_eq!(m.agent.model.key_env(), ["ACME_KEY"]);
    assert_eq!(m.agent.model.base_url(), Some("https://acme.example/v1"));
    let config = m.agent.model.config();
    assert_eq!(
        (
            config.base_url.as_str(),
            config.provider.as_str(),
            config.id.as_str()
        ),
        ("https://acme.example/v1", "acme", "meta-llama/llama-3")
    );
    assert_eq!(
        error("[agent]\nname = \"a\"\nmodel = \"openai/gpt\"\nbase_url = \"https://x\"\n"),
        "4:12: base_url is for a provider yoagent does not know; openai is one it does"
    );
    assert_eq!(
        error("[agent]\nname = \"a\"\nmodel = \"openai/gpt\"\napi_key_env = \"K\"\n"),
        "4:15: api_key_env is for a provider yoagent does not know; openai is one it does"
    );
    assert_eq!(
        error("[agent]\nname = \"a\"\nmodel = \"acme/m\"\nbase_url = \"http://acme.example/v1\"\napi_key_env = \"K\"\n"),
        "4:12: base_url \"http://acme.example/v1\": https, or http on this machine (localhost)"
    );
    assert_eq!(
        error("[agent]\nname = \"a\"\nmodel = \"acme/m\"\nbase_url = \"https://acme.example/v1\"\napi_key_env = \"\"\n"),
        "5:15: api_key_env \"\" is not an environment variable's name"
    );
    for (provider, envs) in PROVIDERS {
        let m = read(&format!(
            "[agent]\nname = \"a\"\nmodel = \"{provider}/m\"\n"
        ))
        .unwrap();
        assert_eq!(m.agent.model.key_env(), envs);
        let config = m.agent.model.config();
        assert_eq!(
            (config.id.as_str(), config.provider.as_str()),
            ("m", provider)
        );
    }
}

#[test]
fn the_key_is_the_first_variable_set() {
    let m = read("[agent]\nname = \"a\"\nmodel = \"google/gemini\"\n").unwrap();
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            pairs
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| v.to_string())
        }
    };
    assert_eq!(
        m.agent.model.api_key_with(env(&[("GOOGLE_API_KEY", "g")])),
        Some("g".into())
    );
    assert_eq!(
        m.agent
            .model
            .api_key_with(env(&[("GEMINI_API_KEY", ""), ("GOOGLE_API_KEY", "g")])),
        Some("g".into())
    );
    assert_eq!(
        m.agent
            .model
            .api_key_with(env(&[("GEMINI_API_KEY", "a"), ("GOOGLE_API_KEY", "g")])),
        Some("a".into())
    );
    assert_eq!(m.agent.model.api_key_with(env(&[])), None);
    let q = read("[agent]\nname = \"a\"\nmodel = \"qwen/qwen-max\"\n").unwrap();
    assert_eq!(q.agent.model.key_env(), ["DASHSCOPE_API_KEY"]);
}

#[test]
fn a_budget_is_dollars_for_a_priced_model() {
    assert_eq!(
        error("[agent]\nname = \"a\"\nmodel = \"deepseek/no-such-model\"\n[limits]\nbudget_usd = 2.0\n"),
        "5:14: budget_usd set, but deepseek/no-such-model has no price"
    );
    let priced = "[agent]\nname = \"a\"\nmodel = \"anthropic/claude-sonnet-5\"\n[limits]\n";
    for (value, shown) in [("-1.0", "-1"), ("0.0", "0"), ("nan", "NaN"), ("inf", "inf")] {
        assert_eq!(
            error(&format!("{priced}budget_usd = {value}\n")),
            format!("5:14: budget_usd {shown}: a number of dollars more than 0")
        );
    }
    let m = read(&format!("{priced}budget_usd = 2.0\n")).unwrap();
    assert_eq!(m.limits.budget_usd, Some(2.0));
}

#[test]
fn limits_are_more_than_zero() {
    for key in ["max_turns", "max_tokens", "max_minutes"] {
        assert_eq!(
            error(&with(&format!("[limits]\n{key} = 0\n"))),
            format!("5:{}: {key} must be more than 0", key.len() + 4)
        );
    }
}

#[test]
fn the_agent_name_is_one_plain_name() {
    for name in ["", "a/b", "..", ".", "a\\b"] {
        let text = format!("[agent]\nname = {name:?}\nmodel = \"deepseek/x\"\n");
        assert_eq!(
            error(&text),
            format!("2:8: agent name {name:?}: one plain name (it names a folder)")
        );
    }
}

#[test]
fn secrets_come_from_the_environment() {
    for (name, written) in [
        ("Authorization", "Bearer abc123"),
        ("Authorization", "Bearer sk-live ${X}"),
        ("X-Auth-Token", "t"),
        ("CLIENT_SECRET", "s"),
        ("DB_PASSWORD", "p"),
        ("Cookie", "c"),
        ("X-Api-Auth", "a"),
        ("apiKey", "k"),
        ("PASSWD", "p"),
        // A scheme word, or nothing, without a reference.
        ("Authorization", "Bearer"),
        ("API_KEY", ""),
    ] {
        let e = error(&with(&format!(
            "[[mcp]]\nurl = \"https://x/mcp\"\nheaders = {{ \"{name}\" = \"{written}\" }}\n"
        )));
        assert!(
            e.starts_with("6:") && e.contains(&format!("{name} looks like a secret")),
            "{name}: {e}"
        );
    }
    assert!(error(&with(
        "[[mcp]]\nnpm = \"s@1.0.0\"\nenv = { API_KEY = \"abc\" }\n"
    ))
    .contains("API_KEY looks like a secret"));
    read(&with(
        "[[mcp]]\nnpm = \"s@1.0.0\"\nenv = { API_KEY = \"${MY_KEY}\", MODE = \"fast\", KEYBOARD = \"us\", MONKEY = \"x\", TOKENIZERS_PARALLELISM = \"false\" }\n",
    ))
    .unwrap();
    read(&with(
        "[[mcp]]\nurl = \"https://x/mcp\"\nheaders = { Authorization = \"Basic ${CREDS}\" }\n",
    ))
    .unwrap();
}

#[test]
fn references_are_closed_plain_names() {
    assert_eq!(
        error(&with(
            "[[mcp]]\nnpm = \"s@1.0.0\"\nenv = { MODE = \"a ${FOO\" }\n"
        )),
        "6:16: \"${FOO\" is not closed with `}`"
    );
    assert_eq!(
        error(&with(
            "[[mcp]]\nnpm = \"s@1.0.0\"\nenv = { MODE = \"${}\" }\n"
        )),
        "6:16: ${}: a variable's name is letters, digits and `_`"
    );
    assert_eq!(
        error(&with(
            "[[mcp]]\nurl = \"https://x/mcp\"\nheaders = { Authorization = \"abc${\" }\n"
        )),
        "6:29: \"${\" is not closed with `}`"
    );
}

#[test]
fn values_take_names_from_the_environment() {
    let env = |name: &str| (name == "TOKEN").then(|| "t0k".to_owned());
    let v = Value::new("Bearer ${TOKEN}!").unwrap();
    assert_eq!(v.resolve_with(env), Ok("Bearer t0k!".into()));
    assert_eq!(v.references(), ["TOKEN"]);
    assert_eq!(v.as_str(), "Bearer ${TOKEN}!");
    assert_eq!(
        Value::new("plain").unwrap().resolve_with(env),
        Ok("plain".into())
    );
    assert_eq!(
        Value::new("${NOPE}").unwrap().resolve_with(env),
        Err(Unset("NOPE".into()))
    );
    assert!(Value::new("${").is_err());
}

#[test]
fn servers_are_stdio_or_http_not_both() {
    assert_eq!(
        error(&with("[[mcp]]\nnpm = \"s@1.0.0\"\nurl = \"https://x\"\n")),
        "6:7: an [[mcp]] names exactly one of npm (stdio) or url (HTTP)"
    );
    assert_eq!(
        error(&with("[[mcp]]\nargs = []\n")),
        "4:1: an [[mcp]] names exactly one of npm (stdio) or url (HTTP)"
    );
    assert_eq!(
        error(&with("[[mcp]]\nurl = \"https://x\"\nargs = [\"a\"]\n")),
        "5:7: args, env and bin are for a stdio server (npm)"
    );
    assert_eq!(
        error(&with(
            "[[mcp]]\nurl = \"https://x\"\nenv = { MODE = \"a\" }\n"
        )),
        "5:7: args, env and bin are for a stdio server (npm)"
    );
    assert_eq!(
        error(&with(
            "[[mcp]]\nnpm = \"s@1.0.0\"\nheaders = { Accept = \"x\" }\n"
        )),
        "5:7: headers are for an HTTP server (url)"
    );
}

#[test]
fn http_servers_are_https_or_on_this_machine() {
    for url in [
        "http://example.com/mcp", "http://localhost.evil.com/mcp", "http://127.0.0.1.nip.io/mcp",
        "ftp://x/mcp",
    ] {
        assert_eq!(
            error(&with(&format!("[[mcp]]\nurl = \"{url}\"\n"))),
            format!("5:7: url {url:?}: https, or http on this machine (localhost)")
        );
    }
    assert_eq!(
        error(&with("[[mcp]]\nurl = \"http://localhost@evil.com/mcp\"\n")),
        "5:7: url \"http://localhost@evil.com/mcp\" has a user in it: give credentials as a header"
    );
    for url in [
        "http://localhost:8080/mcp", "http://127.0.0.1:9000/mcp", "http://[::1]:8080/mcp",
        "HTTPS://Example.com/mcp",
    ] {
        read(&with(&format!("[[mcp]]\nurl = \"{url}\"\n")))
            .unwrap_or_else(|e| panic!("{url}: {e}"));
    }
}

#[test]
fn parts_are_read_strictly() {
    assert_eq!(
        error(&with("[parts]\ngate = \"destructive\"\n")),
        "5:8: gate \"destructive\": \"allow-list\" or \"off\""
    );
    assert_eq!(
        read(&with("[parts]\ngate = \"off\"\n")).unwrap().parts.gate,
        Gate::Off
    );
    assert_eq!(
        read(&with("[parts]\nreport = \"handoff\"\n"))
            .unwrap()
            .parts
            .report,
        Some(Report::handoff())
    );
    for (report, message) in [
        (
            "\"weekly\"",
            "report is \"handoff\" or a list of section names",
        ),
        ("[]", "a report needs at least one section"),
        (
            "[\"Next\", \"next\"]",
            "the report section \"next\" is given twice",
        ),
        ("[\"  \"]", "a report section needs a name"),
        ("[1]", "report sections are names (strings)"),
    ] {
        assert_eq!(
            error(&with(&format!("[parts]\nreport = {report}\n"))),
            format!("5:10: {message}")
        );
    }
}

#[test]
fn frontends_are_web_or_terminal() {
    assert_eq!(
        error(&with("[frontend]\nphone = true\n")),
        "5:9: phone serves the web frontend: set web = true"
    );
    assert_eq!(
        error(&with("[frontend]\nweb = true\nterminal = true\n")),
        "6:12: the terminal or the web frontend, not both"
    );
    let m = read(&with("[frontend]\nterminal = true\n")).unwrap();
    assert_eq!(
        m.frontend,
        Frontend {
            web: false,
            phone: false,
            terminal: true
        }
    );
}

#[test]
fn loading_a_file_makes_its_paths_absolute() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("agent.toml");
    std::fs::write(
        &file,
        "[agent]\nname = \"a\"\nmodel = \"deepseek/x\"\nprompt = \"./prompts/p.md\"\n",
    )
    .unwrap();
    let m = Manifest::load(&file).unwrap();
    assert!(m.dir.is_absolute());
    assert_eq!(m.agent.prompt, Some(m.dir.join("prompts/p.md")));
}

#[test]
fn local_providers_take_no_key() {
    let m = read("[agent]\nname = \"a\"\nmodel = \"ollama/llama3.2\"\n").unwrap();
    assert_eq!(m.agent.model.base_url(), Some(OLLAMA));
    assert!(!m.agent.model.needs_key());
    assert_eq!(m.agent.model.config().base_url, OLLAMA);
    let m = read(
        "[agent]\nname = \"a\"\nmodel = \"local/qwen\"\nbase_url = \"http://127.0.0.1:1234/v1\"\n",
    )
    .unwrap();
    assert_eq!(m.agent.model.config().base_url, "http://127.0.0.1:1234/v1");
    assert_eq!(m.agent.model.config().provider, "local");
    assert!(read(MINIMAL).unwrap().agent.model.needs_key());
    assert_eq!(
        error("[agent]\nname = \"a\"\nmodel = \"local/qwen\"\n"),
        "3:9: local needs base_url: its OpenAI-compatible endpoint"
    );
    assert_eq!(
        error("[agent]\nname = \"a\"\nmodel = \"ollama/x\"\napi_key_env = \"K\"\n"),
        "4:15: ollama takes no key: leave out api_key_env"
    );
    assert_eq!(
        error(
            "[agent]\nname = \"a\"\nmodel = \"ollama/x\"\nbase_url = \"http://gpu.lan:11434/v1\"\n"
        ),
        "4:12: base_url \"http://gpu.lan:11434/v1\": https, or http on this machine (localhost)"
    );
}

#[test]
fn a_secret_after_a_flag_is_a_secret() {
    for args in [
        r#"["--token", "abc"]"#,
        r#"["--api-key=abc"]"#,
        r#"["-p", "x", "--password", "hunter2"]"#,
    ] {
        let e = error(&with(&format!(
            "[[mcp]]\nnpm = \"s@1.0.0\"\nargs = {args}\n"
        )));
        assert!(
            e.starts_with("6:") && e.contains("looks like a secret"),
            "{args}: {e}"
        );
    }
    let m = read(&with(
        "[[mcp]]\nnpm = \"s@1.0.0\"\nargs = [\"--token\", \"${TOKEN}\", \"--api-key=${KEY}\", \"--port\", \"8080\", \".\"]\n",
    ))
    .unwrap();
    let Transport::Stdio { args, .. } = &m.mcp[0].transport else {
        panic!("stdio")
    };
    let lookup = |n: &str| Some(format!("<{n}>"));
    let filled: Vec<String> = args
        .iter()
        .map(|a| a.resolve_with(lookup).unwrap())
        .collect();
    assert_eq!(
        filled,
        ["--token", "<TOKEN>", "--api-key=<KEY>", "--port", "8080", "."]
    );
}

#[test]
fn a_secret_in_a_url_query_is_a_reference() {
    assert_eq!(
        error(&with("[[mcp]]\nurl = \"https://x.example/mcp?api_key=abc&v=1\"\n")),
        "5:7: url: api_key looks like a secret: take it from the environment (`\"${NAME}\"`, or `\"Bearer ${NAME}\"`), never write it here"
    );
    let m = read(&with(
        "[[mcp]]\nurl = \"https://x.example/mcp?api_key=${KEY}&v=1\"\n",
    ))
    .unwrap();
    let Transport::Http { url, .. } = &m.mcp[0].transport else {
        panic!("http")
    };
    assert_eq!(
        url.resolve_with(|_| Some("k".into())).unwrap(),
        "https://x.example/mcp?api_key=k&v=1"
    );
    assert_eq!(m.mcp[0].name, "x.example");
}

#[test]
fn a_password_in_a_url_is_a_reference_wherever_it_is() {
    let refused = [
        "[[mcp]]\nnpm = \"s@1.0.0\"\nenv = { DATABASE_URL = \"postgres://u:hunter2@db/x\" }\n",
        "[[mcp]]\nnpm = \"s@1.0.0\"\nargs = [\"--db\", \"postgres://u:hunter2@db/x\"]\n",
        "[[plugin]]\npi = \"a@1.0.0\"\nconfig = { db = \"mysql://root:pw@localhost/app\" }\n",
    ];
    for text in refused {
        let e = error(&with(text));
        assert!(e.contains("password written in"), "{text}: {e}");
    }
    let allowed = [
        "[[mcp]]\nnpm = \"s@1.0.0\"\nenv = { DATABASE_URL = \"postgres://u:${DB_PASSWORD}@db/x\" }\n",
        "[[mcp]]\nnpm = \"s@1.0.0\"\nargs = [\"git@github.com:a/b.git\", \"https://user@host/x\"]\n",
        "[[plugin]]\npi = \"a@1.0.0\"\nconfig = { home = \"https://example.com/a:b@c\" }\n",
    ];
    for text in allowed {
        read(&with(text)).unwrap_or_else(|e| panic!("{text}: {e}"));
    }
}

#[test]
fn a_plugins_config_takes_secrets_from_the_environment() {
    assert_eq!(
        error(&with("[[plugin]]\ndsh = \"a@1.0.0\"\nconfig = { provider = \"bing\", apiKey = \"sk-1\" }\n")),
        "6:10: config.apiKey: apiKey looks like a secret: take it from the environment (`\"${NAME}\"`, or `\"Bearer ${NAME}\"`), never write it here"
    );
    let nested =
        "[[plugin]]\ndsh = \"a@1.0.0\"\nconfig = { engines = { tavily = { token = \"t\" } } }\n";
    assert!(error(&with(nested)).contains("config.token"));
    let bad_ref = "[[plugin]]\ndsh = \"a@1.0.0\"\nconfig = { x = \"${\" }\n";
    assert!(error(&with(bad_ref)).contains("is not closed"));

    let m = read(&with(
        "[[plugin]]\ndsh = \"a@1.0.0\"\nconfig = { apiKey = \"${SEARCH_KEY}\", hosts = [\"a\", \"${HOST}\"], n = 3 }\n",
    ))
    .unwrap();
    let filled = m.plugins[0]
        .config_with(|n| Some(format!("<{n}>")))
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::Value::Object(filled),
        serde_json::json!({ "apiKey": "<SEARCH_KEY>", "hosts": ["a", "<HOST>"], "n": 3 })
    );
    assert_eq!(
        m.plugins[0].config_with(|_| None),
        Err(Unset("SEARCH_KEY".into()))
    );
}

#[test]
fn deepseek_takes_a_budget() {
    // Priced in yoagent's bundled table since 0.25.6 (at its peak rate).
    let m = read(
        "[agent]\nname = \"a\"\nmodel = \"deepseek/deepseek-flash\"\n[limits]\nbudget_usd = 2.0\n",
    )
    .unwrap();
    assert_eq!(m.limits.budget_usd, Some(2.0));
}
