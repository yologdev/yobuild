//! The manifest read strictly: each rule, with where the error points.

use super::*;

fn read(text: &str) -> Result<Manifest, ManifestError> {
    Manifest::parse(text, Path::new("agent.toml"), Path::new("/agents"))
}

/// The error's `line:col: message`.
fn error(text: &str) -> String {
    let e = read(text).expect_err("should be refused");
    let (line, col) = e.at.expect("a place");
    format!("{line}:{col}: {}", e.message)
}

const MINIMAL: &str = "[agent]\nname = \"a\"\nmodel = \"deepseek/deepseek-flash\"\n";

#[test]
fn a_full_manifest_reads() {
    let m = read(
        r#"
[agent]
name   = "handoff"
model  = "anthropic/claude-sonnet-5-5"
prompt = "prompts/handoff.md"

[limits]
max_turns   = 80
max_tokens  = 4_000_000
max_minutes = 30

[[plugin]]
pi = "@juicesharp/rpiv-todo@2.12.0"

[[plugin]]
dsh    = "dsh-free-search@0.8.1"
config = { provider = "bing", disabledEngines = [] }

[[plugin]]
rutis   = "./plugins/word_count.ts"
runtime = "bun"

[[mcp]]
npm  = "@modelcontextprotocol/server-filesystem@1.2.3"
args = ["."]

[[mcp]]
url     = "https://mcp.example.com/mcp"
headers = { Authorization = "Bearer ${EXAMPLE_MCP_TOKEN}", Accept = "application/json" }

[parts]
ask_owner = true
report    = "handoff"
facts     = true
gate      = "allow-list"
scratch   = true

[dsh]
host_services = true

[frontend]
web = true
"#,
    )
    .unwrap();
    assert_eq!(m.agent.model.provider, "anthropic");
    assert_eq!(m.agent.model.id, "claude-sonnet-5-5");
    assert_eq!(m.agent.model.key_env, "ANTHROPIC_API_KEY");
    assert_eq!(
        m.agent.prompt,
        Some(PathBuf::from("/agents/prompts/handoff.md"))
    );
    assert_eq!(m.limits.max_tokens, Some(4_000_000));
    assert_eq!(
        m.plugins[0],
        Plugin::Pi {
            source: Source::Package {
                name: "@juicesharp/rpiv-todo".into(),
                version: "2.12.0".into()
            },
            config: None
        }
    );
    assert!(
        matches!(&m.plugins[1], Plugin::Dsh { config: Some(c), .. } if c["provider"] == "bing")
    );
    assert_eq!(
        m.plugins[2],
        Plugin::Rutis {
            path: PathBuf::from("/agents/./plugins/word_count.ts"),
            runtime: Runtime::Bun,
            config: None
        }
    );
    assert!(matches!(&m.mcp[0], Mcp::Stdio { args, .. } if args == &["."]));
    assert!(matches!(&m.mcp[1], Mcp::Http { headers, .. } if headers.len() == 2));
    assert_eq!(
        m.parts.report.as_deref(),
        Some(&["Done", "Waiting on you", "Decisions", "Next"].map(String::from)[..])
    );
    assert!(m.parts.gate && m.parts.ask_owner && m.dsh.host_services && m.frontend.web);
}

#[test]
fn the_smallest_manifest_has_the_gate_on() {
    let m = read(MINIMAL).unwrap();
    assert!(m.parts.gate);
    assert!(m.plugins.is_empty() && m.mcp.is_empty());
    assert_eq!(m.parts.report, None);
}

#[test]
fn unknown_keys_are_errors_where_they_are() {
    let e = error(&format!("{MINIMAL}[parts]\ngat = \"off\"\n"));
    assert!(e.starts_with("5:1: unknown field `gat`"), "{e}");
    let e = error("[agent]\nname = \"a\"\nmodel = \"x/y\"\nmodle = \"z\"\n");
    assert!(e.contains("unknown field `modle`"), "{e}");
    let e = error(&format!(
        "{MINIMAL}[[plugin]]\npi = \"a@1.0.0\"\nconfg = {{}}\n"
    ));
    assert!(e.contains("unknown field `confg`"), "{e}");
    assert!(error("[agnet]\n").contains("unknown field `agnet`"));
}

#[test]
fn plugins_and_servers_are_pinned_or_local() {
    for spec in [
        "rpiv-todo", "rpiv-todo@latest", "rpiv-todo@^2.12.0", "rpiv-todo@2.12", "@scope/x",
        "@scope/x@~1.0.0",
    ] {
        let e = error(&format!("{MINIMAL}[[plugin]]\npi = \"{spec}\"\n"));
        assert!(
            e.starts_with("5:6:") && e.contains("is not pinned"),
            "{spec}: {e}"
        );
    }
    for spec in [
        "x@1.2.3", "@scope/x@1.2.3", "x@1.2.3-beta.1", "./local", "../up/x.ts", "/abs/x",
    ] {
        read(&format!("{MINIMAL}[[plugin]]\ndsh = \"{spec}\"\n"))
            .unwrap_or_else(|e| panic!("{spec}: {e}"));
    }
    let e = error(&format!("{MINIMAL}[[mcp]]\nnpm = \"server@latest\"\n"));
    assert!(e.contains("is not pinned"), "{e}");
}

#[test]
fn a_plugin_names_one_ecosystem_and_a_rutis_one_its_runtime() {
    let e = error(&format!(
        "{MINIMAL}[[plugin]]\npi = \"a@1.0.0\"\ndsh = \"b@1.0.0\"\n"
    ));
    assert!(e.contains("exactly one of pi, dsh or rutis"), "{e}");
    let e = error(&format!("{MINIMAL}[[plugin]]\nconfig = {{}}\n"));
    assert!(e.contains("exactly one of pi, dsh or rutis"), "{e}");
    let e = error(&format!("{MINIMAL}[[plugin]]\nrutis = \"./x.ts\"\n"));
    assert!(e.contains("needs a runtime"), "{e}");
    let e = error(&format!(
        "{MINIMAL}[[plugin]]\nrutis = \"x@1.0.0\"\nruntime = \"node\"\n"
    ));
    assert!(e.contains("a local file"), "{e}");
    let e = error(&format!(
        "{MINIMAL}[[plugin]]\npi = \"a@1.0.0\"\nruntime = \"node\"\n"
    ));
    assert!(e.contains("runtime is for a rutis plugin"), "{e}");
    let e = read(&format!(
        "{MINIMAL}[[plugin]]\nrutis = \"./x.ts\"\nruntime = \"ruby\"\n"
    ))
    .unwrap_err();
    assert!(e.message.contains("unknown variant `ruby`"), "{e}");
}

#[test]
fn models_are_provider_and_id() {
    let e = error("[agent]\nname = \"a\"\nmodel = \"deepseek-flash\"\n");
    assert!(
        e.starts_with("3:9:") && e.contains("not `provider/id`"),
        "{e}"
    );
    let e = error("[agent]\nname = \"a\"\nmodel = \"acme/m1\"\n");
    assert!(
        e.contains("unknown provider \"acme\"") && e.contains("deepseek"),
        "{e}"
    );
    let m = read("[agent]\nname = \"a\"\nmodel = \"acme/m1\"\nbase_url = \"https://acme.example/v1\"\napi_key_env = \"ACME_KEY\"\n").unwrap();
    assert_eq!(m.agent.model.key_env, "ACME_KEY");
    assert_eq!(m.agent.model.config().base_url, "https://acme.example/v1");
    let e = error("[agent]\nname = \"a\"\nmodel = \"openai/gpt\"\nbase_url = \"https://x\"\napi_key_env = \"K\"\n");
    assert!(e.contains("openai is one it does"), "{e}");
    for (provider, env) in PROVIDERS {
        let m = read(&format!(
            "[agent]\nname = \"a\"\nmodel = \"{provider}/m\"\n"
        ))
        .unwrap();
        assert_eq!(m.agent.model.key_env, env);
        assert_eq!(m.agent.model.config().id, "m");
    }
}

#[test]
fn a_budget_needs_a_priced_model() {
    let e = error(
        "[agent]\nname = \"a\"\nmodel = \"deepseek/no-such-model\"\n[limits]\nbudget_usd = 2.0\n",
    );
    assert_eq!(
        e,
        "5:14: budget_usd set, but deepseek/no-such-model has no price"
    );
    let m = read("[agent]\nname = \"a\"\nmodel = \"anthropic/claude-sonnet-5\"\n[limits]\nbudget_usd = 2.0\n");
    assert_eq!(m.map(|m| m.limits.budget_usd), Ok(Some(2.0)));
}

#[test]
fn secrets_come_from_the_environment() {
    let e = error(&format!("{MINIMAL}[[mcp]]\nurl = \"https://x/mcp\"\nheaders = {{ Authorization = \"Bearer abc123\" }}\n"));
    assert!(e.contains("Authorization looks like a secret"), "{e}");
    let e = error(&format!(
        "{MINIMAL}[[mcp]]\nnpm = \"s@1.0.0\"\nenv = {{ API_KEY = \"abc\" }}\n"
    ));
    assert!(e.contains("API_KEY looks like a secret"), "{e}");
    read(&format!("{MINIMAL}[[mcp]]\nnpm = \"s@1.0.0\"\nenv = {{ API_KEY = \"${{MY_KEY}}\", MODE = \"fast\" }}\n")).unwrap();
}

#[test]
fn values_take_names_from_the_environment() {
    std::env::set_var("YOBUILD_TEST_TOKEN", "t0k");
    assert_eq!(
        Value("Bearer ${YOBUILD_TEST_TOKEN}!".into()).resolve(),
        Ok("Bearer t0k!".into())
    );
    assert_eq!(Value("plain".into()).resolve(), Ok("plain".into()));
    assert_eq!(
        Value("${YOBUILD_TEST_UNSET}".into()).resolve(),
        Err("YOBUILD_TEST_UNSET".into())
    );
}

#[test]
fn servers_are_stdio_or_http_not_both() {
    let e = error(&format!(
        "{MINIMAL}[[mcp]]\nnpm = \"s@1.0.0\"\nurl = \"https://x\"\n"
    ));
    assert!(
        e.contains("exactly one of npm (stdio) or url (HTTP)"),
        "{e}"
    );
    let e = error(&format!(
        "{MINIMAL}[[mcp]]\nurl = \"http://example.com/mcp\"\n"
    ));
    assert!(e.contains("over https"), "{e}");
    read(&format!(
        "{MINIMAL}[[mcp]]\nurl = \"http://localhost:8080/mcp\"\n"
    ))
    .unwrap();
    let e = error(&format!(
        "{MINIMAL}[[mcp]]\nurl = \"https://x\"\nargs = [\"a\"]\n"
    ));
    assert!(e.contains("args and env are for a stdio server"), "{e}");
}

#[test]
fn parts_are_read_strictly() {
    let e = error(&format!("{MINIMAL}[parts]\ngate = \"destructive\"\n"));
    assert!(e.contains("\"allow-list\" or \"off\""), "{e}");
    assert!(
        !read(&format!("{MINIMAL}[parts]\ngate = \"off\"\n"))
            .unwrap()
            .parts
            .gate
    );
    let m = read(&format!(
        "{MINIMAL}[parts]\nreport = [\"Summary\", \"Next\"]\n"
    ))
    .unwrap();
    assert_eq!(m.parts.report, Some(vec!["Summary".into(), "Next".into()]));
    let e = error(&format!("{MINIMAL}[parts]\nreport = \"weekly\"\n"));
    assert!(e.contains("\"handoff\" or a list"), "{e}");
    let e = error(&format!("{MINIMAL}[parts]\nreport = [\"\"]\n"));
    assert!(e.contains("non-empty"), "{e}");
}

#[test]
fn a_missing_file_is_named() {
    let e = Manifest::load("/no/such/agent.toml").unwrap_err();
    assert!(e.to_string().starts_with("/no/such/agent.toml: "), "{e}");
}
