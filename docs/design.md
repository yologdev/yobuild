# Design

Draft for review. Names, fields and file layouts are proposals.

## 1. Shape

```text
            agent.toml  (the manifest: model, prompt, plugins, parts, frontend)
                 │
                 ▼
   ┌──────────── yobuild ───────────────────────────────────────────┐
   │ runner      resolve + install plugins, vet them, start the host │
   │ parts       ask_owner · report + facts · approval gate · limits │
   │ frontend    session · browser/phone · terminal · plugin host    │
   └──────────────────────────┬─────────────────────────────────────┘
                              │ crates.io
            ┌─────────────────┴──────────────────┐
            ▼                                    ▼
        yoagent  (the loop)               yoagent-rutis  (plugins → Extension)
        MCP client (stdio, HTTP)           pi adapter · DSH adapter · TS/Bun/Python/Go
            │                                       │
            ▼                        ┌──────────────┼───────────────┐
        MCP servers                  ▼              ▼               ▼
                                pi extensions   DSH plugins    rutis plugins
```

Three ways in, one engine:

| Entry | Who | What it does |
|---|---|---|
| `yobuild run agent.toml` | a person | runs an agent from its manifest |
| `yobuild check agent.toml` | a person, CI, a coding agent | installs and vets every plugin; reports what each adds and needs, without calling a model |
| the **skill** (`SKILL.md`) | a coding agent | teaches the method: pick parts, vet, write the manifest, run scripted, then live |

## 2. The manifest

TOML, one file per agent. Paths are relative to the manifest.

```toml
[agent]
name   = "handoff"
model  = "deepseek/deepseek-flash"   # provider/id; the key from the provider's env var
prompt = "prompts/handoff.md"        # system prompt; {cwd}, {scratch} interpolated

[limits]
max_turns  = 80
max_tokens = 4_000_000               # per run
max_minutes = 30
budget_usd = 2.0                     # refused if the model has no price

# Plugins, by ecosystem. Versions pinned; `yobuild check` refuses unpinned.
[[plugin]]
pi = "@juicesharp/rpiv-todo@2.12.0"

[[plugin]]
dsh    = "dsh-free-search@0.8.1"
config = { provider = "bing", disabledEngines = [], bingMarket = "en-US" }

[[plugin]]
rutis = "plugins/word_count.ts"      # a local TS/Bun/Python/Go plugin
runtime = "bun"

# MCP servers, through yoagent's own MCP client. Same rules as plugins.
[[mcp]]                              # stdio: the pinned package is installed (no scripts), its bin run
npm  = "@modelcontextprotocol/server-filesystem@1.2.3"   # version illustrative
args = ["."]

[[mcp]]                              # HTTP: the URL is the pin
url     = "https://mcp.example.com/mcp"
headers = { Authorization = "Bearer ${EXAMPLE_MCP_TOKEN}" }  # ${…} from the environment, never written here

[parts]
ask_owner = true                     # non-blocking questions; answers arrive as steering
report    = "handoff"                # or a list of required sections
facts     = true                     # host-counted facts after each report
gate      = "destructive"            # approval before deleting/overwriting; "off" to disable
scratch   = true                     # a scratch dir the gate leaves alone

[dsh]
host_services = false                # DSH's own fs/attachments/credentials/… for plugins that need them

[frontend]
web      = true
phone    = false                     # serve on the LAN, print a phone link
terminal = false
```

Rules:

- **Pinned or refused.** Every plugin and every stdio MCP server carries an
  exact version (or is a local path). Reproducible agents, and the vetting
  result stays valid. No `npx -y`: it resolves the latest version and runs
  install scripts.
- **One plugin, one line of intent.** Configuration is the plugin's own;
  yobuild does not translate it.
- **Unknown keys are errors.** A typo in a safety setting must not pass.
- **A budget needs a price.** `budget_usd` uses yoagent's `Budget::for_model`
  with the bundled price table (`prices::enable_bundled()`, called once at
  start). A manifest that sets it for an unpriced model is refused:
  `budget_usd set, but deepseek/deepseek-flash has no price`.

### Models

`model = "<provider>/<id>"` maps to a yoagent `ModelConfig` constructor;
`<id>` is passed as the model id. The API key comes from the provider's
environment variable, read by yobuild (yoagent reads none).

| Provider | Constructor | Protocol, base URL | Key from |
|---|---|---|---|
| `anthropic` | `ModelConfig::anthropic` | Anthropic Messages, `api.anthropic.com/v1` | `ANTHROPIC_API_KEY` |
| `openai` | `ModelConfig::openai` | OpenAI Chat Completions, `api.openai.com/v1` | `OPENAI_API_KEY` |
| `deepseek` | `ModelConfig::deepseek` | OpenAI-compatible, `api.deepseek.com`, `OpenAiCompat::deepseek()` | `DEEPSEEK_API_KEY` |
| `google` | `ModelConfig::google` | Google Generative AI | `GEMINI_API_KEY` |
| `groq`, `mistral`, `xai`, `qwen`, `zai`, `minimax`, `meta` | the constructor of the same name | OpenAI-compatible, the provider's base URL and compat flags | `<PROVIDER>_API_KEY` |
| anything else | `ModelConfig::openai_compat` | needs `base_url` (and compat flags) under `[agent]` | `api_key_env` under `[agent]` |

An unknown provider without `base_url` is an error that lists the known
providers. Prices come from the bundled table; a provider missing from it
runs, but without `budget_usd` or cost facts.

## 3. The parts

Small, generic, and each tested on its own. They come from the handoff
example and move into yobuild as a library (`yobuild::parts`), usable
without the manifest.

| Part | Plugs in as | Source today |
|---|---|---|
| `ask_owner` | a tool; returns at once, the answer comes back as steering (mid-run, or starting the next run) | `handoff_agent.rs` `AskOwner` + `Session::ask_later` |
| `report` | an `Extension`: `on_stop` sends the agent back until the required sections are there | `handoff_agent.rs` `Handoff::on_stop` |
| `facts` | the same extension: `on_event` counts tokens and tool calls, `finish` adds changes since the handoff (git snapshot), new files, open questions | `Handoff::on_event` / `finish` |
| `gate` | a `before_tool` policy: ask before selected destructive commands and overwrites; refused with nobody there | `confirm-dangerous.ts` (a pi extension today; a Rust port is a candidate, see the roadmap) |
| `scratch` | a private working folder named to the agent and the gate | `YOAGENT_SCRATCH` |
| `limits` / `budget` | yoagent's `ExecutionLimits` and `Budget` | yoagent |

What makes a part a part: more than one kind of agent wants it, it is
small, and it has no opinion about the task.

## 4. Plugins: resolve, install, vet

- **Install** per ecosystem into a cache (`~/.yobuild/plugins/<ecosystem>/<lock-hash>/`),
  from a lockfile yobuild writes next to the manifest (`agent.lock`), so a
  checked agent runs the same bytes later. Installs never run package
  scripts.
- **Vet** with the same probe that produces yoagent's
  [plugin compatibility](https://github.com/yologdev/yoagent/blob/main/docs/evals/plugin-compatibility.md)
  report: load, list tools/hooks/prompt additions, list services a DSH
  plugin waits for, refuse what the adapter refuses. Nothing is called.
  For an MCP server: connect, list its tools (`tools/list`), call none.
- **Report** per plugin: what it adds, what is ignored (commands, UI), what
  it needs that is missing — before the agent runs.

## 5. The frontend

yoagent-frontend moves here as `yobuild::frontend` (history kept via
`git subtree split`): `Session` / `Driver`, the browser page and phone link,
the pi-tui terminal UI, UI plugins, plugin dialogs (pi and DSH questions
reach the owner), `PluginHost` (rows, routing, reload, `env`, `terminal`),
`DshHome`. It is application code; yoagent keeps no frontend.

## 6. Safety model

Inherited from yoagent and the handoff work, and kept on by default:

- **Fail closed** at every plugin boundary (yoagent-rutis): a policy that
  times out, crashes or is unloaded denies; a redaction that fails
  withholds; a plugin hooking what the loop cannot enforce is refused.
- **A policy is required** when a gate is configured (`require_policy`): a
  run starting while the policy plugin is down denies every tool call.
- **Questions are never answered for the owner**; with no frontend, the
  safe default (deny, skip).
- **Private homes**: DSH's services get a home of their own (`DshHome`);
  the scratch folder is the agent's.
- **Message size**: calls to plugins stay under rutis's 16 MiB limit.

Not covered, and said so: plugins run with the process's rights; the
environment they inherit is visible to them; the gate checks selected
operations, not everything a script does. A sandbox (container, VM) is the
deployment's job.

## 7. What moves, what stays

| From yoagent | To yobuild |
|---|---|
| `integrations/yoagent-frontend/` (crate, plugins, web, tests) | `yobuild::frontend` |
| `examples/handoff_agent.rs`, `coding_agent.rs`, `minimal_browser.rs` | manifests `examples/handoff.toml`, `coding.toml`, `minimal.toml` (plus the parts they used) |
| `plugins/pi-extensions/confirm-dangerous.ts` and its tests | `yobuild::parts::gate` |
| `scripts/try-handoff.sh`, `try-coding-agent.sh` | `yobuild run` |
| CI: frontend job, pty check, handoff demo | yobuild CI |

Stays in yoagent: the loop, providers, tools, `Extension`; yoagent-rutis
(bridge, pi and DSH adapters, the compatibility matrix and its probe);
yoagent-workers.

## 8. Testing

- Every manifest runs **scripted** (yoagent's `MockProvider`) in CI: the
  agent's behaviour without a model or a key.
- `yobuild check` on every example manifest in CI.
- Manifests with an `[[mcp]]` entry run scripted against a small stdio test
  server, copied from the bash-scripted servers in yoagent's
  `tests/mcp_stdio_test.rs`.
- The terminal UI on a pseudo-terminal (the check that caught rutis 0.9's
  process-group regression).
- Live runs by hand (DeepSeek), recorded in the release notes.

## 9. When a part is written in Rust (proposed)

Prototype with plugins; promote to Rust when production needs it. Both
plug into the same `Extension` contract, so a promotion swaps one manifest
entry for a built-in part — the agent's behaviour, and its tests, stay.

**Performance is rarely the reason.** Measured on the handoff agent
(yoagent's `examples/README.md`, M2, release build): a live run of 30.5 s
spent 15.7 s in the model and 2.7–6.7 s per web search; the loop plus a
Node plugin policy costs about 3 ms per tool call (9–11 ms for the first).
A Rust policy saves milliseconds next to seconds.

**Write it in Rust when one of these holds:**

| Reason | Test | Example |
|---|---|---|
| **Safety** | it decides whether something runs, or what leaves a tool (a policy, a gate, a redaction) | the approval gate: in Node it holds only while that runtime is up (`require_policy` then denies everything — safe, but the agent stops) |
| **Hot path** | it runs on every tool call or event *and* its cost shows in measured turn time (rule of thumb: over 5 % of a turn, or a runtime started just for it) | an `on_event` meter, a per-call argument rewriter |
| **Footprint** | a whole runtime (a Node runtime is 137–244 MB; the Rust process with loop, bridge and web server, 21 MB) is kept alive for one small thing | a single TypeScript policy as the only reason Node runs |
| **Target** | it must run where the runtime cannot: Cloudflare Workers (wasm32), an embedded host | anything in a Worker |
| **Ownership** | the product depends on it and upstream changes faster than you can follow | a third-party plugin that became core |

**Keep it a plugin when** it is a capability reused unchanged from an
ecosystem (that is the point), it changes often, it is off the safety path,
or its time is dominated by the network or the model (search, fetch, an
office API).

**In between: Go.** A Go plugin (yoagent-rutis's `go` feature) is a single
compiled binary with no Node or Python — smaller and faster to start than a
JavaScript runtime, still out of process. A fit for a policy or tool that
should not need Node but is not worth a Rust part.

**How to decide:** measure first. `yobuild run` should print per-plugin
hook time and per-runtime memory (as the handoff demo prints loop + policy
and tool time today), so the table above is applied to numbers, not
guesses.

## 10. Later: agents that extend themselves

Two tools for a running agent: `find_plugin` (search + vet) and
`load_plugin` (install + hot-load through `PluginHost`). Both behind the
owner's approval (`ask_owner`) and the fail-closed rules. This is what
rutis was built for — software that adds its own plugins while it runs —
and the riskiest part of the plan, so it comes last.
