# Design

Draft for review. Names, fields and file layouts are proposals.

## 1. Shape

```text
            agent.toml  (the manifest: model, prompt, plugins, built-ins, frontend)
                 │
                 ▼
   ┌──────────── yobuild ───────────────────────────────────────────┐
   │ runner      resolve + install plugins, vet them, start the host │
   │ built-ins   ask_owner · report + facts · approval gate · limits │
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

TOML, one file per agent (`yobuild::manifest`). Paths are relative to the
manifest's folder.

```toml
[agent]
name   = "handoff"
model  = "deepseek/deepseek-flash"   # provider/id; the key from the provider's env var
prompt = "prompts/handoff.md"        # system prompt; yobuild run fills in {cwd}, {scratch}

[limits]
max_turns  = 80
max_tokens = 4_000_000               # per run
max_minutes = 30
budget_usd = 2.0                     # dollars per run; refused if the model has no price

# Plugins, by ecosystem. Versions pinned, or a local path.
[[plugin]]
pi   = "@juicesharp/rpiv-todo@2.12.0"
allow = ["todo"]                     # tools the gate lets run without asking (see "Allowed tools")

[[plugin]]
dsh    = "dsh-free-search@0.8.1"
config = { provider = "bing", disabledEngines = [], bingMarket = "en-US" }
allow  = ["multi_search", "web_search"]

[[plugin]]
rutis = "plugins/word_count.ts"      # a local TS/Bun/Python/Go plugin
runtime = "node"                     # yobuild run: node only, for now
name  = "words"                      # optional: the default is the package's name or the file's stem

# MCP servers, through yoagent's own MCP client. Same rules as plugins.
[[mcp]]                              # stdio: the pinned package is installed (no scripts), its bin run
npm  = "@modelcontextprotocol/server-filesystem@1.2.3"   # version illustrative
args = ["."]                         # also `env`, and `bin` when the package has several
allow = ["read_text_file", "list_directory"]  # the server's own names; the agent sees `<server>__<tool>`

[[mcp]]                              # HTTP: the URL is the pin
url     = "https://mcp.example.com/mcp"
headers = { Authorization = "Bearer ${EXAMPLE_MCP_TOKEN}" }  # ${…} from the environment, never written here

[builtins]
ask_owner = true                     # non-blocking questions; answers arrive as steering
report    = "handoff"                # or a list of required sections
facts     = true                     # host-counted facts after each report
gate      = "allow-list"             # the approval gate (the default); "off" to disable
scratch   = true                     # a scratch dir the gate leaves alone

[dsh]
host_services = false                # DSH's own fs/attachments/credentials/… for plugins that need them

[frontend]
web      = true
phone    = false                     # serve on the LAN, print a phone link
terminal = false
```

### Allowed tools

The gate (`gate = "allow-list"`) asks before every tool it does not know
to be harmless: yoagent's `bash` and file tools are judged call by call,
its read-only tools and `ask_owner` run without asking, and **every plugin
and MCP tool asks**. An agent that plans and searches as it works (the
handoff agent: rpiv-todo's `todo`, dsh-free-search's searches) would ask
the owner before each of those calls. `allow` names the tools of one entry
that run without asking — entries of the gate's allow-list:

```toml
[[plugin]]
pi    = "@juicesharp/rpiv-todo@2.12.0"
allow = ["todo"]

[[mcp]]
name  = "files"
npm   = "@modelcontextprotocol/server-filesystem@1.2.3"
allow = ["read_text_file"]           # offered to the model as `files__read_text_file`
```

- **On the entry that offers the tools**, named as that plugin or server
  names them: the trust sits next to what it trusts, an MCP tool needs no
  `<server>__` prefix spelled out, and a name can be checked against what
  that entry offers.
- **Allowed means: run without asking**, not "the others are refused" —
  a tool not listed still runs when the owner answers Allow. List only
  tools that read or look things up: allowing is the manifest author
  vouching for the tool; the gate does not look inside a plugin or server.
  A tool that writes, deletes, sends or runs code stays asking.
- **Refused when the manifest is read** (an error with its place, like the
  others):
  - a name a tool cannot have (1 to 64 letters, digits, `_`, `-`), or
    repeated;
  - `allow` with `gate = "off"` (it would mean nothing);
  - on a plugin, `bash`, `write_file`, `write`, `edit_file`, `edit`: the
    gate judges these call by call and never allows them by name;
  - on a plugin, `ask_owner`: yobuild's built-in allows its own tool, and a
    plugin's tool of that name must ask;
  - on a plugin, a name with `__`: it marks an MCP server's tool
    (`<server>__<tool>`), which only that server's entry may allow;
  - a server's names are its own (`files__bash` is not yoagent's `bash`),
    but `__` in a server's name is refused: it would make prefixes
    ambiguous.
- **Checked when the agent starts:** an MCP server's `allow` names must be
  among the tools it lists (a typo, or a server whose tools changed, is
  refused, naming what it does list). A plugin's tools come with each run,
  so its `allow` names are checked after the first run (yoagent's
  `before_model` sees the tools offered each turn; across the whole run, so
  a plugin still starting is not mistaken for one without the tool): one no
  tool of that name was offered for is a Warning notice ("plugin rpiv-todo
  offered no tool `todos`"), once a session. A warning, not a refusal: a
  plugin may offer a tool only in some runs.
- **Scoped to the entry:** a plugin's allowed tool runs without asking
  only when that plugin offered it in the run (yoagent-rutis's
  `offered_by`, from 0.5), and never when the name is one of the agent's
  own tools (yoagent runs its own on a clash). For pi, the extension file
  that registered the tool must be one of that plugin's; DSH does not say
  which plugin registered a tool, so all DSH plugins count as one; a
  rutis plugin's handler name is its own choice, so any handler other
  than the two adapters' counts as the rutis owner (a manifest key naming
  its handler could narrow that). The adapters' handler names are made
  per process and unguessable, so no plugin can take one first and pass
  for pi or DSH.
- **Limits for pi** (pi's extensions share one adapter and one Node
  process):
  - Until yoagent-rutis's pi adapter runs exactly the tools it offered
    (yoagent PR #333, coming in 0.5.1), an extension that registers the
    same name after the run's tools were taken could run in the allowing
    one's place.
  - A pi tool can call other pi tools itself (`ctx.executeTool`); those
    calls never reach the gate (only pi's own handlers judge them). So
    allowing a pi tool also allows what it calls — a code runner, all of
    it. A host option restricting such calls to the calling extension's
    own tools (or turning them off) is asked of yoagent-rutis; yobuild
    will use "own extension".
  - Per-extension trust guards against honest name clashes, not against a
    hostile extension loaded in the same runtime: it shares the process.
- **MCP:** an MCP server's names are scoped by their prefix: each is
  checked against that server's own list; a server's tools are the
  agent's own (fixed at start), and yoagent keeps those over a plugin's
  tool of the same name; two tools of one name among them are refused; and
  no server name holds `__`.
- **What the gate gets:** the names as the model sees them — a plugin's
  as they are, an MCP server's as `<server>__<tool>` — through
  `Gate::with_allowed_tools`.

Considered and not taken: one list under `[builtins]`
(`allowed_tools = ["todo", "files__read_text_file"]`) — shorter, but it
separates the trust from the entry, makes MCP tools spelled with their
prefix, and cannot say which entry a name was meant for, so a typo is
only ever a warning. Patterns (`files__*`) — a server's write tools would
be allowed with its read tools. The name `whitelist` — the gate is already
an allow-list. `free` — the gate's internal word, read as cost or licence.

Rules:

- **Pinned or refused.** Every plugin and every stdio MCP server is an npm
  registry package (`name`, `@scope/name`) at an exact semver version
  (`1.2.3`, `1.2.3-beta.1`), or a local path (`./`, `../`, `/`; a rutis
  plugin is always a local file, so `plugins/x.ts` is one too). Tags,
  ranges, git, URL and `file:` specs are refused. Reproducible agents, and
  the vetting result stays valid. No `npx -y`: it resolves the latest
  version and runs install scripts.
- **Names.** Every plugin and MCP server has a name, unique in the
  manifest: `name`, else the package's name without its scope, the file's
  stem, or an HTTP server's host.
- **One plugin, one line of intent.** Configuration is the plugin's own;
  yobuild does not translate it.
- **Unknown keys are errors.** A typo in a safety setting must not pass.
  Every error names the file, line and column.
- **Secrets come from the environment.** One rule for every value the
  manifest hands a plugin or a server — a server's variable, header,
  argument and URL, and every string in a plugin's `config` (whose
  `${NAME}` references are filled in at start too):
  - a value named with a secret's word (split at `_`, `-` and case
    changes: key, apikey, token, secret, password, passwd, pwd, auth,
    authorization, cookie, credential(s)) is only `${NAME}` references,
    plus at most a scheme word (`Bearer ${TOKEN}`, `Basic`, `Token`). An
    argument is named by the flag before it (`--token X`, `--api-key=X`),
    a string in `config` by its key, a URL's query parameter by its own;
  - a URL in any value (`postgres://user:password@host`) has its password
    as a reference (`://user:${DB_PASSWORD}@`).

  A value written into the file is refused. References are checked when
  the file is read (closed, a plain name).
- **Endpoints over https** (MCP servers, a custom model's `base_url`):
  plain http only to this machine (`localhost`, `127.0.0.1`, `[::1]`), and
  no user in the URL.
- **Limits** are more than zero; the agent's name is one plain name (it
  names its scratch folder); `config` is a table (dates become text).
- **Frontends**: `phone` needs `web`; the terminal or the web, not both.
- **Plugins**: each `[[plugin]]` names exactly one of `pi`, `dsh`, `rutis`
  (a local file, with its `runtime`: node, bun, python, go), and its own
  `config`. Each `[[mcp]]` names exactly one of `npm` (stdio, with `args`
  and `env`) or `url` (HTTP, with `headers`).
- **A budget needs a price.** `budget_usd` (dollars, more than zero) uses
  yoagent's `Budget::for_model` with the bundled price table, which
  `yobuild run` enables once at start (`prices::enable_bundled()`); reading
  the manifest looks the price up without enabling anything. A manifest
  that sets it for an unpriced model is refused:
  `budget_usd set, but acme/m1 has no price`. (DeepSeek is priced since
  yoagent 0.25.6, at its peak rate: an upper bound.)

### Models

`model = "<provider>/<id>"` maps to a yoagent `ModelConfig` constructor;
`<id>` (everything after the first `/`) is passed as the model id. The API
key comes from the provider's environment variables (the first set), read
by yobuild and passed to the agent: without one, yoagent would fall back to
`YOAGENT_API_KEY` / `API_KEY`, so `yobuild run` refuses to start instead.

| Provider | Constructor | Protocol, base URL | Key from |
|---|---|---|---|
| `anthropic` | `ModelConfig::anthropic` | Anthropic Messages, `api.anthropic.com/v1` | `ANTHROPIC_API_KEY` |
| `openai` | `ModelConfig::openai` | OpenAI Chat Completions, `api.openai.com/v1` | `OPENAI_API_KEY` |
| `deepseek` | `ModelConfig::deepseek` | OpenAI-compatible, `api.deepseek.com`, `OpenAiCompat::deepseek()` | `DEEPSEEK_API_KEY` |
| `google` | `ModelConfig::google` | Google Generative AI | `GEMINI_API_KEY`, then `GOOGLE_API_KEY` |
| `groq`, `mistral`, `xai`, `zai`, `minimax` | the constructor of the same name | OpenAI-compatible, the provider's base URL and compat flags | `<PROVIDER>_API_KEY` |
| `qwen` | `ModelConfig::qwen` | OpenAI-compatible (DashScope) | `DASHSCOPE_API_KEY` |
| `meta` | `ModelConfig::meta` | OpenAI-compatible | `META_API_KEY`, then `MODEL_API_KEY` |
| `ollama` | `ModelConfig::ollama` | OpenAI-compatible, `base_url` (default `http://localhost:11434/v1`) | none |
| `local` | `ModelConfig::local` | any OpenAI-compatible server on this machine (LM Studio, llama.cpp, …); needs `base_url` | none |
| anything else | `ModelConfig::openai_compat`, default compat flags | needs `base_url` under `[agent]` | `api_key_env` under `[agent]` |

An unknown provider without `base_url` is an error that lists the known
providers; `base_url` without `api_key_env` says it is missing; either key
for a known provider is refused, and `api_key_env` for a local one (it
takes no key; `yobuild run` passes an empty one, so yoagent's fallback key
never reaches it). A `base_url` is https, or http on this machine. Prices come from the bundled table; a provider missing from it
runs, but without `budget_usd` or cost facts.

## 3. The built-ins

yobuild's own behaviours, not a plugin's: what every agent it builds can
switch on in `[builtins]`, whatever ecosystem its tools come from. Most plug
into the loop as hooks (yoagent extensions: `report`, `facts`, the
`gate`); `ask_owner` is a tool; `scratch` is a folder. Not plugins moved
into Rust: a capability an ecosystem already has stays a plugin (§9).
The library is `yobuild::builtins`.

Small, generic, and each tested on its own. They come from the handoff
example and move into yobuild as a library (`yobuild::builtins`), usable
without the manifest.

| Part | Plugs in as | Where |
|---|---|---|
| `ask_owner` | a tool, and for plugins a host service (proposed, below); returns at once, the answer comes back as steering (mid-run, or starting the next run) | `yobuild::builtins::AskOwner` (M1) |
| `report` | an `Extension`: `on_stop` sends the agent back until the required sections are there | `yobuild::builtins::Report` (M1) |
| `facts` | an `Extension`: `on_event` counts tokens and tool calls, `finish` adds changes since the start (git snapshot), new files, open questions | `yobuild::builtins::Facts` (M1) |
| `gate` | a `before_tool` policy: an allow-list of programs and their options; everything else (deleting, overwriting above all) asks; refused with nobody there; plugin and MCP tools ask unless their entry allows them (`allow`) | `yobuild::builtins::Gate` (M1; `confirm-dangerous.ts` stays as the pi example) |
| `scratch` | a private working folder named to the agent and the gate | `yobuild::builtins::Scratch` (M1) |
| `limits` / `budget` | yoagent's `ExecutionLimits` and `Budget` | yoagent |

What makes a built-in a built-in: more than one kind of agent wants it,
it is small, and it has no opinion about the task.

### `ask_owner` for plugins (proposed)

The agent asks the owner with the `ask_owner` tool. A plugin may need to
ask too — a pi extension that wants the owner to choose between plans, a
DSH plugin missing a setting — and today it can only ask through the
dialogs (`ui.request`), which block the plugin until the owner answers or
the question times out. `ask_owner` as a host service asks the way the
tool does: without waiting.

- **The service:** `ask_owner`, offered to every plugin runtime when the
  manifest has `[builtins] ask_owner = true` (like `ui` and `frontend`,
  shared by yobuild's plugin host). Without the built-in it is not there:
  a plugin that injects it as required does not start (rutis), so a
  plugin should look it up and go without when it is missing.
- **One method:** `ask({ question, why?, options? })` → the question's
  number, at once. The same shape as the tool's arguments, checked by the
  same `Question::from_json` (a blank question, unknown keys or a
  non-string option are refused with an error the plugin sees). Two or
  more options make it a choice; otherwise a free answer.
- **The answer reaches the agent, not the plugin:** as with the tool, it
  arrives as steering — `[Owner's answer to #N] <question>\n<answer>`, or
  `[Owner skipped #N]` — in the run under way, or as the next run's
  prompt. A plugin that needs the answer itself uses the dialogs instead.
  The question is shown as the plugin's (its title names it), so the
  owner knows who asks.
- **One numbering:** the service and the tool share the session's
  `AskOwner` (its clones share the count), so #N is unique, and facts'
  "N question(s) waiting on you" counts both.
- **Not judged by the gate:** asking does nothing the owner has not
  seen; like the tool, it is always allowed. A reset withdraws the open
  questions, as for the tool.
- **Node only, for now:** `yobuild run` starts Node runtimes only (§4).
  Whether Python and Go plugins can call host services is checked before
  they are supported (roadmap).

## 4. Plugins: resolve, install, vet

- **Until M2b**, `yobuild run` installs nothing: a pinned package must
  already be installed at exactly its version — pi packages, stdio MCP
  servers and the rutis runtime in `node_modules/` beside the manifest, DSH
  plugins in `adapters/dsh` (`--no-save`) — or the run is refused, naming
  the `npm install` to do. It checks what it can before starting: a DSH
  package's bundle patch and imports, its peers installed, and once the DSH
  runtime runs, the services its rows inject (a missing one is refused,
  not left waiting).
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

yoagent-frontend moved here as `yobuild::frontend` (M0, history kept via
`git subtree split`): `Session` / `Driver`, the browser page and phone link,
the pi-tui terminal UI, UI plugins, plugin dialogs (pi and DSH questions
reach the owner), `PluginHost` (rows, routing, reload, `env`, `terminal`),
`DshHome`. It is application code; yoagent keeps no frontend.

## 6. Safety model

Inherited from yoagent and the handoff work, and kept on by default:

- **Fail closed** at every plugin boundary (yoagent-rutis): a policy that
  times out, crashes or is unloaded denies; a redaction that fails
  withholds; a plugin hooking what the loop cannot enforce is refused.
- **The gate holds without a plugin runtime**: `yobuild::builtins::Gate` is a
  required extension in the Rust process, an allow-list: each program it
  lets run has a list of options, and what it writes, deletes or runs is
  judged; anything it does not know asks. A gate written as a plugin (the pi
  example) needs `require_policy`, so a run starting while that plugin is
  down denies every tool call.
- **Questions are never answered for the owner**; with no frontend, the
  safe default (deny, skip).
- **Private homes**: DSH's services get a home of their own (`DshHome`);
  the scratch folder is the agent's.
- **Message size**: calls to plugins stay under rutis's 16 MiB limit.

Not covered, and said so: plugins run with the process's rights; the
environment they inherit is visible to them; the gate judges command
lines and file tools, not what code does once it runs (a build or a test
runs the project's code, including what the agent edited); and it guards
the owner's files against being deleted or replaced (not against edits),
and does not keep their data from being read or sent. A sandbox (container, VM), without secrets the agent should not
have, is the deployment's job.

## 7. What moved, what it becomes

Moved from yoagent in M0 (`integrations/yoagent-frontend/`, its examples,
`scripts/try-*.sh`, the CI job). What each becomes:

| In yobuild now | Becomes |
|---|---|
| `src/frontend/` (`yobuild::frontend`), `frontend/web/`, `frontend/plugins/`, `tests/` | stays (M1: one crate) |
| `examples/handoff_agent.rs`, `coding_agent.rs`, `minimal_browser.rs` | manifests `examples/handoff.toml`, `coding.toml`, `minimal.toml` (plus the parts they used; M2a) |
| `frontend/plugins/pi-extensions/confirm-dangerous.ts` and its tests | `yobuild::builtins::gate` (M1; the TypeScript version stays as an example) |
| `scripts/try-handoff.sh`, `try-coding-agent.sh` | `yobuild run` (M2a) |
| CI: `check` (pty check, handoff demo) | grows with the built-ins and manifests |

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
([`examples/README.md`](../examples/README.md), M2, release build): a live run of 30.5 s
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

**How to decide:** measure first. `yobuild run --measure` (and `--once`) says, after each run,
each plugin handler's time per hook and per tool (measured at the bridge:
for a language plugin, the round trip to its runtime) and each plugin
runtime's memory, so the table above is applied to numbers, not guesses.
For example, a scripted run with a pi and a DSH fixture: "Plugin time this
run: pi-extensions 23 ms (before_tool 2× 11 ms …); dsh-tools 11 ms (…).
Runtime memory: pi 335 MB, dsh 157 MB."

## 10. Later: agents that extend themselves

Two tools for a running agent: `find_plugin` (search + vet) and
`load_plugin` (install + hot-load through `PluginHost`). Both behind the
owner's approval (`ask_owner`) and the fail-closed rules. This is what
rutis was built for — software that adds its own plugins while it runs —
and the riskiest part of the plan, so it comes last.
