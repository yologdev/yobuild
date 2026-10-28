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
| the **skill** (`SKILL.md`) | a coding agent | teaches the method: ask, pick parts, write the manifest, vet it, run scripted, then live (§11) |

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
budget_usd = 1.0                     # dollars per run; refused if the model has no price
session_budget_usd = 2.0             # dollars for the whole session, across runs

# Plugins, by ecosystem. Versions pinned, or a local path.
[[plugin]]
pi   = "@juicesharp/rpiv-todo@2.12.0"
allow = ["todo"]                     # tools the gate lets run without asking (see "Allowed tools")

[[plugin]]
dsh    = "dsh-free-search@0.8.1"
config = { provider = "bing", disabledEngines = [], bingMarket = "en-US" }
allow  = ["multi_search", "research"]

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
tools     = "coding"                 # yoagent's six tools (the default), or "read-only"

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
  plugin may offer a tool only in some runs. `yobuild check` (§4) says
  the same before any run.
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
  for pi or DSH. (Now by the row that registered the tool, as the host
  sees it — §4, "Who a plugin is"; what precedes is how it was.)
- **pi** (its extensions share one adapter and one Node process):
  - The adapter runs exactly the tools a run was offered (yoagent-rutis
    0.5.1): an extension registering the same name later never runs in
    the allowing one's place.
  - A pi tool can call other pi tools itself (`ctx.executeTool`); those
    calls never reach the gate (only pi's own handlers judge them).
    yobuild sets the adapter's `nestedCalls: "own-extension"`, so they
    reach only the calling extension's own tools: allowing a pi tool
    allows what its own extension offers, not the others' (a code runner
    is still all of it).
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
  (a local file, with its `runtime`: node, bun, python, go), its own
  `config`, and `policy = true` when it is the agent's approval policy (a
  pi or rutis plugin; §7). Each `[[mcp]]` names exactly one of `npm` (stdio, with `args`
  and `env`) or `url` (HTTP, with `headers`).
- **A budget needs a price.** `budget_usd` (dollars per run, more than
  zero) and `session_budget_usd` (across the session's runs) use
  yoagent's `Budget::for_model` with the bundled price table, which
  `yobuild run` enables once at start (`prices::enable_bundled()`); reading
  the manifest looks the price up without enabling anything. A manifest
  that sets it for an unpriced model is refused:
  `budget_usd set, but acme/m1 has no price`. (DeepSeek is priced since
  yoagent 0.25.6, at its peak rate: an upper bound.)

### Models

`model = "<provider>/<id>"` maps to a yoagent `ModelConfig`: the preset
whose provider and id match, when yoagent has one (`ModelConfig::preset`,
yoagent 0.25.10: the model's real context window, output limit and protocol —
a generic constructor's are often smaller — about 128K in and 4K out for
many providers, DeepSeek's and Meta's excepted — which cuts replies short
and compacts early; added 2026-10-19), else the provider's constructor
below (yoagent's `ModelConfig::generic`; a model with neither a preset nor
a provider whose constructor carries its models' limits — DeepSeek, Meta —
is noted, by `check` and as a Warning in `run`: its limits are guesses); `<id>` (everything after the first `/`) is passed as the model id. The API
key comes from the provider's environment variables (the first set), read
by yobuild and passed to the agent: without one, yoagent would fall back to
`YOAGENT_API_KEY` / `API_KEY`, so `yobuild run` refuses to start instead.

| Provider | Constructor | Protocol, base URL | Key from |
|---|---|---|---|
| `anthropic` | `ModelConfig::anthropic` | Anthropic Messages, `api.anthropic.com/v1` | `ANTHROPIC_API_KEY` |
| `openai` | `ModelConfig::generic`: `openai_responses` for a reasoning id, else `ModelConfig::openai` | OpenAI Responses API (reasoning ids: not `gpt-3*`, `gpt-4*`, `chatgpt-*`, `*-chat*`) or Chat Completions, `api.openai.com/v1` | `OPENAI_API_KEY` |
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
| `ask_owner` | a tool, and for plugins a host service (below); returns at once, the answer comes back as steering (mid-run, or starting the next run) | `yobuild::builtins::AskOwner` (M1) |
| `report` | an `Extension`: `on_stop` sends the agent back until the required sections are there | `yobuild::builtins::Report` (M1) |
| `facts` | an `Extension`: `on_event` counts tokens and tool calls, `finish` adds changes since the start (git snapshot), new files, open questions | `yobuild::builtins::Facts` (M1) |
| `gate` | a `before_tool` policy: an allow-list of programs and their options; everything else (deleting, overwriting above all) asks; refused with nobody there; plugin and MCP tools ask unless their entry allows them (`allow`) | `yobuild::builtins::Gate` (M1; `confirm-dangerous.ts` stays as the pi example) |
| `scratch` | a private working folder named to the agent and the gate | `yobuild::builtins::Scratch` (M1) |
| `limits` / `budget` | yoagent's `ExecutionLimits` and `Budget` | yoagent |

What makes a built-in a built-in: more than one kind of agent wants it,
it is small, and it has no opinion about the task.

### `ask_owner` for plugins

The agent asks the owner with the `ask_owner` tool. A plugin may need to
ask too — a planner that wants the owner to choose between plans, a
plugin missing a setting — and today it can only ask through the
dialogs (`ui.request`), which block the plugin until the owner answers or
the question times out. `ask_owner` as a host service asks the way the
tool does: without waiting.

- **The service:** `ask_owner`, offered to every plugin runtime (like `ui`
  and `frontend`, shared by yobuild's plugin host). A plugin injects it
  (`inject: ['ask_owner']`). It is there whatever the manifest says —
  rutis shows a host service only to a plugin that injects it, and has no
  optional inject: a plugin injecting a missing service would never start,
  and nobody would be told. Without `[builtins] ask_owner = true` every
  `ask` is refused, saying so (the plugin goes without); so is one after
  the session ended.
- **One method** (`from`'s meaning since changed — §4, "Who a plugin
  is"): `ask({ question, why?, options? }, from?)` → the
  question's number, at once. The same shape as the tool's arguments,
  checked by the same `Question::from_json` (a blank question, unknown keys
  or a non-string option are refused with an error the plugin sees); a
  plugin's question and each option are one line (`why` may have more),
  and a third
  argument is refused. Two or more options make it a choice; otherwise a
  free answer.
- **The answer reaches the agent, not the plugin:** as with the tool, it
  arrives as steering — `[Owner's answer to #N] <question>\n<answer>`, or
  `[Owner skipped #N]` — in the run under way, or, when the agent is idle,
  as the prompt of a new run (which costs what a run costs). A plugin that
  needs the answer itself uses the dialogs instead.
- **Who asks** (now named by the host from the calling row — §4, "Who a
  plugin is"; what follows is how it was before): a host service does
  not know its caller. `from` is the
  plugin's name as it gives it — one line, up to 64 characters, not
  verified — and the question reads "… (asked by plugin <from>)" to the
  owner and in the agent's steer. Without it, a plugin's question looks
  like the agent's own. Plugins are trusted code (§6): `from` tells the
  owner who asks, it is not a security boundary, and nothing limits how
  many questions a plugin asks. (The one-line rule keeps a question from
  adding lines that read as the owner's answer.)
- **One numbering:** the service and the tool share the session's
  `AskOwner` (its clones share the count), so #N is unique, and facts'
  "N question(s) waiting on you" counts both.
- **Not judged by the gate:** asking does nothing the owner has not
  seen; like the tool, it is always allowed. A reset withdraws the open
  questions, as for the tool.
- **rutis plugins only (decided 2026-10-18):** a `[[plugin]] rutis = …`
  plugin calls it directly. pi extensions and DSH plugins are not given
  it: neither ecosystem has a way to ask without waiting (pi's
  `ctx.ui.select`/`confirm`/`input` and DSH's `userQuestions` block until
  answered, and those already reach the owner through the adapters), so
  adapter support would mean a yobuild-only API in each — one no
  published plugin calls, and that a plugin written against it could not
  run anywhere else, against yobuild's point of running plugins
  unchanged. A plugin written for yobuild is a rutis plugin and has the
  service. Revisited only if a real pi or DSH plugin needs it.
  `yobuild run` starts Node runtimes only (§2); whether Python and Go
  plugins can call host services is checked before they are supported
  (roadmap).

## 4. Plugins: resolve, install, vet

Before M2b, `yobuild run` installed nothing: a pinned package had to be
installed by hand beside the manifest (DSH plugins in `adapters/dsh`).
Before starting, `run` checks what it can: a DSH package's bundle patch
and imports, and its peers installed. Once the DSH runtime runs, it
checks the services the plugin's rows inject; a missing one is refused,
not left waiting.

M2b (below) installs pinned packages into a cache (#30), records what
was installed in `agent.lock`, and adds `yobuild check` (#31). Unix only, as the
terminal UI and `--measure` are (Windows: not in 0.1, roadmap open question 7).

### Install: sets, the cache, `agent.lock` (M2b, decided 2026-10-18)

**What is installed:** the npm packages a manifest pins, which are its pi
plugins, DSH plugins and stdio MCP servers. Some things are not installed:

- **A rutis plugin.** It is the owner's code in the owner's package, and
  the owner installs its runtime there, as now.
- **A pi file.** It is loaded by path, as now.
- **A DSH file.** It runs in `adapters/dsh`, as now, and must resolve that
  folder's Cordis (as the adapter's fixtures do).
- **The adapters.** They stay in `adapters/`, as `Layout` says. Running
  without the repository is M4.
- **HTTP MCP servers.**

**Sets.** A manifest's pins fall into up to three sets, one per
ecosystem. Each set is an npm project of its own:

| Set | Holds | Peers |
|---|---|---|
| `pi` | the pi packages, plus every peer of theirs that is not pi's own (required or optional; e.g. `@juicesharp/rpiv-i18n`, an optional peer of `rpiv-todo`) | pi's own packages — a fixed pattern, `@earendil-works/pi-*` and `typebox`, not the adapter's whole dependency list — are never installed here: the adapter supplies them (its loader resolves them). So the set resolves with `legacy-peer-deps`, and yobuild adds the other peers as dependencies of the set, at the range the package declares; the lock fixes their versions |
| `dsh` | the DSH adapter's base (its `package.json` and lock: DSH's packages, Cordis, the rutis runtime) plus the DSH packages | resolved by npm as usual (no `legacy-peer-deps`); DSH's own packages come from the base |
| `mcp` | the stdio servers | resolved by npm as usual |

The `dsh` set exists because a DSH plugin must resolve the base's Cordis,
and the runtime refuses one that does not. So when a manifest pins a DSH
package, the `dsh` runtime starts in the set's folder instead of
`adapters/dsh`, as the compatibility matrix does. Two rules follow:

- A manifest with both DSH files and DSH packages is refused for now:
  "DSH files and DSH packages cannot run together yet: the packages run in
  the cache, the files in `adapters/dsh`". This is one rule on purpose,
  even when every DSH package is one the base already holds.
- A DSH package the base already pins (`dsh-free-search`, DSH's
  `dsh-tool-ask-user` and `dsh-user-questions`) adds nothing to the set at
  the same version. At another version it is refused: "pinned by the DSH
  base at 0.8.1".

Two entries of one set pinning one package at different versions are
refused when the manifest is read, since one npm project holds one
version at the top. Sets are separate projects: a pi pin and an
`[[mcp]]` pin of one package do not meet. A pi peer that is also pinned
in the `pi` set takes the pin. A set with no pins is not made.

**`agent.lock`** sits beside the manifest, named after it (`agent.toml`
→ `agent.lock`, `handoff.toml` → `handoff.lock`), so manifests sharing a
folder keep their own. yobuild writes it, and it is meant to be checked
in:

```json
{
  "lockVersion": 1,
  "sets": {
    "pi": {
      "requested": { "@juicesharp/rpiv-todo": "2.12.0" },
      "packages": { "": { … }, "node_modules/@juicesharp/rpiv-todo": { "version": "2.12.0", "resolved": "https://registry.npmjs.org/…", "integrity": "sha512-…" }, … }
    },
    "dsh": {
      "base": "<sha256 of adapters/dsh's package.json and package-lock.json>",
      "requested": { "dsh-free-search": "0.8.1" },
      "packages": { … }
    }
  }
}
```

- **`requested`** holds the manifest's pins in the set; `pi`'s added peers
  are not listed (they follow from the pins).
- **`packages`** is npm's own lock, as `package-lock.json` (v3) holds it:
  each package's version, `resolved` URL and `integrity` hash. yobuild
  writes it back as a `package-lock.json`, and `npm ci` checks each
  tarball against its hash, so a checked agent runs the same bytes later,
  on any machine. The pins fix only the top level; the lock fixes what
  they pull in. The file is JSON because it carries npm's format, which
  npm reads; a TOML copy would be a translation.
- **`base`** is a checksum of the base's packages, not of the
  yoagent-rutis version: an upgrade that leaves DSH's packages as they
  were leaves the examples' locks current.

A manifest with no pins gets no `agent.lock`, and `--locked` passes
without one. A set whose pins are all gone is removed from the file, and
a file left with no sets is deleted. Under `--locked`, a set the
manifest no longer needs is stale too: an error.

**Resolving.** A set is current when its `requested` equals the
manifest's pins for that ecosystem, and, for `dsh`, its `base` equals
the adapter's. A missing or stale set is resolved again in a temp folder:

1. Write the set's `package.json` and `.npmrc` (below). Its `name` is
   fixed per set (`yobuild-pi`, `yobuild-mcp`; for `dsh`, the adapter's),
   so agents with the same pins get the same lock and share a folder.
   - For `dsh`, it is the adapter's `package.json` with the packages
     added, and the adapter's lock is the start, so DSH's base stays as
     the adapter pinned it.
   - For `pi`, it lists the pins plus their peers. yobuild reads each
     pin's `peerDependencies` (and `peerDependenciesMeta`) with
     `npm view <name>@<version> --json` and adds each peer that is not
     pi's own, at its declared range; a peer that is itself pinned keeps
     the pin. Two ranges for one peer are intersected: each pair of their
     `||` alternatives joined by a space (`a1 b || a1 b2 || …`), since a
     space binds tighter than `||` in a range. Only the pins' own peers are added: with
     `legacy-peer-deps`, a dependency's peers are not installed, and one
     that needs them fails when imported (`check` shows it). A peer's
     version is the newest its range allows at the first resolve (e.g.
     `@juicesharp/rpiv-i18n`, declared `*`), then fixed by the lock.
2. Run `npm install --package-lock-only`. Nothing is unpacked; it fetches
   package metadata, so it needs the network.
3. Take the lock's `packages`, checked whole: every package's
   dependencies and optional dependencies are in it (npm 11.5 drops a
   peer-only package's optional platform packages, and a newer `npm ci`
   refuses such a lock; it is refused here, naming the npm version). An
   install writes the `package.json`
   again from fixed fields — for `dsh` the adapter's (`"type": "module"`
   among them: the runtime's own files are ESM `.ts`), for `pi` and
   `mcp` `name`, `"private": true`, `"type": "module"` — plus the
   dependencies of the lock's root entry (`packages[""]`), which holds
   only `name`, `dependencies` and `engines`. So `npm ci` finds the two in
   step.

Only that set changes. The file is rewritten, and an Info notice says
what changed, packages added and removed ("agent.lock: `pi` resolved
again: +@x/y 1.0.0, −@x/z 0.3.1"). The file is written to a temp file and
renamed into place, so two yobuilds writing it at once leave one whole
version, not a mix.

With `--locked` (on `run` and `check`; new to `run`'s options), a missing
or stale set is an error instead. This is for CI and for an agent someone
checked: it never resolves anew unseen. `--locked` still installs a
current set whose folder is missing, which needs the network or npm's
cache.

**npm's settings.** The settings that decide the result are passed on
the command line, which wins over every npm config file and variable:
`--ignore-scripts`, `--legacy-peer-deps` for `pi` (`=false` otherwise),
`--package-lock=true`, `--install-strategy=hoisted`, `--strict-peer-deps=false`,
`--install-links=false`, `--include=dev --include=optional
--include=peer` (nothing omitted; `include` wins over an `omit` set
anywhere, and an empty `--omit=` is warned about as invalid), plus
`--no-audit --no-fund --loglevel=error`. The set's `.npmrc` repeats the
deciding ones, for anyone running npm there by hand. The child's environment drops every
`npm_config_*` variable, matched without regard to case as npm reads
them, except the registry and its login (`npm_config_registry`,
`npm_config_//…:_authToken` and the like). Dropped on purpose: the
variable copies of `before`, `os`, `cpu` and `libc` too, while the
owner's file keeps them (below).

The owner's `~/.npmrc` still applies to what is not passed: the registry
and its login (a private registry's token stays there; yobuild neither
writes nor reads tokens), the proxy and certificates, and a few keys
that could still change a result, which yobuild does not override:
`before` (resolve as of a date), and `os`/`cpu`/`libc` (which optional
platform packages `npm ci` installs; the lock still fixes their
versions). Resolving uses no `--prefer-offline`, so it reads the
registry's current metadata; installing does use it, since the lock
fixes every version and npm checks every hash.

**Installing.** A set lives at `~/.yobuild/plugins/<set>/<hash>/`
(`YOBUILD_HOME` replaces `~/.yobuild`). Every agent whose lock holds the
same set shares the folder.

`<hash>` is the first 16 hex digits of a sha256 over:

- the set's name;
- the set's `base` and `packages` in the lock (what decides the folder's
  content: not `requested`, so two agents whose pins resolve alike, such
  as two pinning the DSH base's own packages, share a folder), in one
  canonical form: compact JSON with object keys sorted by code point at
  every level, written by one function with a fixed test vector (the
  crate's `serde_json` keeps insertion order, so sorting is explicit);
- for `dsh`, the checksum of the adapter files the set copies (below), so
  an adapter edited under the same version gets a folder of its own.

A missing folder is made in `<hash>.partial-<random>` beside it:

1. Write `package.json`, `package-lock.json` and `.npmrc`, as above.
2. For `dsh`, copy in, at the root, the adapter files the runtime loads
   from there: `dsh-tools-adapter.ts`, `host-dialogs.ts` and
   `probe-services.ts`. `run`, `probe_services`, `inspect-package.mjs`
   (given the set folder as its base) and `probe::dsh` all use the set
   folder. `probe::dsh` copies its two files into its base only when they
   are missing, so it never writes into an installed set.
   `host-services.json` is read in place from `adapters/dsh`; it names
   packages, which resolve from the set.
3. Run `npm ci`.
4. Write a stamp, `.yobuild-installed`, holding the folder's hash, then
   rename the folder into place.

If another yobuild made the same folder first, the rename fails and this
copy is dropped: the same lock gives the same content. On use, the
stamp must be there and hold the folder's name; the files themselves are
not checked again (that would read the whole tree on every run). The
stamp is written before the rename, so a folder in place without it, or
with another hash, was changed by hand: it is not used, and is removed
and made again. An edit to the files inside goes unseen. A `.partial-*`
folder older than an hour is removed, since a crashed install leaves one.
A folder in place is never changed. Deleting `~/.yobuild/plugins` is
always safe while no agent runs, since every lock can make it again.
Cleaning old folders is not in M2b.

**No package scripts, ever.** Every npm call has `--ignore-scripts`, and
so does the folder's `.npmrc`. `check` names each installed package
that has install scripts (npm's `hasInstallScript`): "has install
scripts, which were not run". It is said under each entry it belongs to:
the entry's pin, the pin's dependencies in the lock, and, in the `pi`
set, the peers yobuild added for that pin (and their dependencies). One
package can belong to several entries. The `dsh` base's own packages are
said once for the agent, not under each plugin. The adapters' own dependencies with
scripts (esbuild, fsevents, koffi, protobufjs, `@google/genai`) work
without them. This was checked on 2026-10-18: the whole suite passed with
`adapters/pi`, `adapters/dsh`, `frontend/plugins` and `examples`
installed `--ignore-scripts`.

**What a run uses:**

- a pi package from `<pi set>/node_modules/<name>`, then
  `resolve-package.mjs` as now;
- the `dsh` runtime, started in the `dsh` set's folder;
- a server's `bin` from the `mcp` set.

`node_modules/` beside the manifest is no longer read, and
`examples/package.json` goes. So do its install in CI (`npm ci --prefix
examples`), in `scripts/try-coding-agent.sh --setup-only` and in the
tests.

**Tests.** A test cannot fetch a package that is not on npm (the
fixtures `fake-pi`, and `srv`, the MCP server for the `bin` test). Such
tests seed a cache instead: a helper (`install::seed`) writes a set's
folder with its stamp (the fixture copied into `node_modules/`) and a
matching lock, through the same hash function, in the tests' cache;
these tests run with a current lock, so nothing is resolved. The "wrong version
refused" tests become two, on a real package from npm
(`@juicesharp/rpiv-todo`, which the handoff example pins), whose lock is
seeded at another version:

- `--locked` with that stale set is refused, naming the set;
- without it, the set is resolved again and the notice says what
  changed.

Tests that install real packages — the handoff and coding demos, the DSH
package tests in `run_test` (their `dsh` set holds the whole DSH base) —
share one cache, `CARGO_TARGET_TMPDIR/yobuild-home`, kept across runs,
so each set is installed from npm once; CI empties it first, so every
install runs there, and has the network.

### Vet: `yobuild check agent.toml` (M2b, decided 2026-10-18)

`yobuild check <agent.toml> [--locked] [--json]` needs no model, no key
and no task:

1. **Read and install.** Read the manifest (its errors as `run` gives
   them), lock and install as above, then `plan` as `run` does. Of `run`'s
   first step, the key, the budgets' prices and the prompt are skipped;
   the rest (the plugins' plan, each MCP server's launch) is the same.
2. **Each plugin alone, through yoagent-rutis's `probe`.** This is the
   compatibility matrix's probe, so the verdict is the matrix's
   ([plugin compatibility](https://github.com/yologdev/yoagent/blob/main/docs/evals/plugin-compatibility.md)),
   with one difference noted under pi:
   - **pi** (a package or a file): `probe::pi` gives its tools. yobuild
     passes no skills, since `run` loads none, and `check` names a
     package's skills, prompts and themes as ignored, as `run`'s note
     does. The probe retries a tool refused for sharing a built-in's name
     with that built-in left out (`without`), and calls that a clean
     load. `run` does not retry (the adapter refuses such a tool), so for
     yobuild a non-empty `without` is a problem: "its tool `read_file` is
     named like yoagent's built-in: `run` refuses it".
   - **DSH:** `probe::dsh` over the `dsh` set gives its rows, tools,
     prompt guidance, the services its rows inject (present or missing)
     and import errors. For a DSH file, the base is `adapters/dsh`. A
     plugin whose rows are packages the probe's own base loads
     (`dsh-web`, `dsh-system-prompt`, `dsh-tools`, `dsh-user-questions`,
     the host services) would be loaded twice, which DSH refuses, so it is
     not probed alone: a note says so, and the agent together checks it
     (found with #31: `coding.toml` pins `dsh-user-questions`).
   - **rutis** has no probe. The plugin is loaded alone in its runtime, as
     `run` loads it (with `run`'s services, on an open session with
     nobody there), and `check` lists the handlers its row registers (once
     they stop changing, up to 3 s) and the tools a run is offered. A row
     that stops, or a run that would stop at its first request, is a
     problem; a tool offered from no row (a child plugin's) is a note. One
     waiting for a service nobody provides alone — another plugin may
     provide it — is checked together only.
   - **An MCP server** is launched as `run` launches it and asked for
     `tools/list`. No tool is called.
3. **The agent together, through `run`'s own start.** This is `run`'s
   step 2, with no frontend. It shows what only appears with everything
   loaded under the manifest's settings, which the probe does not use.
   The probe always gives DSH plugins DSH's host services and shares only
   registered service names; `run` gives host services only with
   `[dsh] host_services`, and shares every name. What `run` refuses at
   start (a missing service, a policy's judge that never registers) is a
   problem, and ends this step: what comes later needs the agent
   started. What `run` only warns about is a note. It catches:
   - a DSH plugin that needs `[dsh] host_services` (a problem);
   - a `policy = true` plugin whose judge never registers (a problem);
   - two plugins offering one tool name (a note naming them: a run gets
     one), and a plugin's tool named like one of the agent's own (a note:
     yoagent keeps its own);
   - a rutis plugin that registered handlers alone but none together, or
     a plugin that stops (problems);
   - an `allow` naming a tool its entry does not offer (a note, as in
     `run`, since a plugin may offer a tool only in some runs). Today
     that is a Warning only after the first run; `check` says it before
     any.

   Step 2 reports every plugin, whatever the others do; step 3 runs only
   when step 2 found no problem, since a plugin that fails alone fails
   together too.

   **The one run.** The agent's extensions are not run. `check` calls the
   bridge's hooks directly, as the probe's `offered` does: `start_run`,
   `tools`, one `before_model`, then `finish`. That gives the plugins'
   tools, each with its owner (`offered_by`, read before `finish`: the
   registry forgets a run's offers then). Their names are checked
   against the agent's own tools (the built-ins, `ask_owner`, each
   server's prefixed tools), which are known without running anything.
   A run stopped or failed at its first request (`before_model`) is a
   problem: every run would be. A rutis plugin that registered handlers
   alone is given up to 3 s to register them together too (a TypeScript
   row runs before its async `apply` ends). No model is asked, and facts,
   report, budget and measure take no part.

Loading runs the plugins' code, as `run` would. `check` is not a sandbox.

**The report** has one block per entry, then the agent. This is
`handoff.toml`'s (#31; the tool lists shortened):

```text
plugin rpiv-todo  pi @juicesharp/rpiv-todo@2.12.0  ok
  tools: todo (allowed)
plugin todo-plan  rutis …/frontend/plugins/todo-plan.ts  ok
  handlers: none (it offers no tool)
plugin dsh-free-search  dsh dsh-free-search@0.8.1  ok
  tools: advanced_search (allowed), …, free_search_test (asks), multi_search (allowed), …
  prompt guidance: 1543 characters
agent handoff: ok
  tools: bash (judged per call), read_file (free), …, ask_owner (allowed), free_search_test (asks), …, todo (allowed)
  install scripts not run (the DSH base): esbuild, fsevents, koffi
```

A tool is one of five kinds: **free** (read-only, never asks),
**judged** per call by the gate (`bash`, the writers and editors, by
name, whoever offers them), **allowed** by an entry's `allow` (or the
built-in's), **asks**, or, with `gate = "off"`, **runs** as the agent
asks. A block can also show what is ignored (skills, prompts, themes),
what is missing and why, and the install scripts not run that belong to
it; an entry checked together only says "not checked" when the agent was
not started. `--json` gives the same as one object, for the skill (M3):
camelCase, with `version`, `ok`, and the agent's `together` (`skipped`,
`refused`, `started`). The exit code is 1 when anything has a problem
(an entry, or the agent: refused at start, or not started); notes are
not problems. What stops `run` before anything starts (the prompt, an
install, a plugin that cannot be planned) stops `check` too, as an error.

CI runs `yobuild check --locked` on every example manifest (§8).

### Who a plugin is: the row the host sees

Since rutis 0.12 the host sees which loader row makes each call into it
(`caller_row`), and yoagent-rutis 0.6 records that row's id as the owner of
every handler a language plugin registers through a row that injects
`yoagent` (`HandlerInfo::owner`, `offered_by(run, tool).owner()`). The
plugin does not say it; the host does. A plugin that reaches `yoagent`
without declaring it in its `inject` gets no row: its owner is "language
plugin (session …)", which is no row's id and never counts as a plugin's.

yobuild's rows: a manifest's rutis plugin is `plugin:<name>` (in its
package's runtime); a DSH plugin is `plugin:<name>` (a file) or
`plugin:<name>:<row id>` (a package's rows), in the `dsh` runtime; the
adapters are the rows `pi` and `dsh`; the helpers `pi-host-ui`,
`dsh-host-dialogs`, `dsh-web`, `dsh-system-prompt`, `dsh-tools` (DSH's
package of that name: a row id, not the adapter's handler),
`terminal-ui` and `yobuild:dsh-services` register no tool handlers.
Names hold no `:`, so `plugin:a:x` is never another plugin's `plugin:a`;
owners are compared whole, never by prefix. yobuild uses no instanced
groups (whose copies of a row would share its id), and no Rust plugin of
yobuild's takes a row's id (they share one name space).

- **Allowed tools** (§2), by the kind of entry, not the shape of the id:
  - rutis: the owner is the plugin's own row, `plugin:<name>` — not, as
    now, any handler that is not an adapter's.
  - pi: the `pi` row, and the extension file (`source`, said by the
    adapter) one of that plugin's — as now; pi's extensions share the
    row.
  - DSH: the `dsh` row (the adapter, which all DSH plugins share, so they
    still count as one), or one of that plugin's own rows when the
    plugin registers a handler itself.
  - Any other owner — another plugin's row, or no row — is not this
    entry's: the tool asks. The first time an allowed name is offered
    only by another owner, a Warning says so ("plugin A allows `x`,
    offered by `plugin:B`" / "offered by no row: does it inject
    `yoagent`?"), next to today's warning for a name nobody offered.
- **The adapters' handler names stay random** (#18): an owner can no
  longer be faked by a name, but a plugin that registers a fixed
  `pi-extensions` first would make the real adapter's registration fail
  (yoagent-rutis refuses a second handler of a name). Start-up waits for
  the handler named so *and* owned by the `pi` / `dsh` row.
- **A policy plugin** (`policy = true`, §7), at start: a rutis one is
  waited for until a handler that judges calls (`before_tool`) is owned by
  its row `plugin:<name>` (any one of its handlers will do) — not, as now,
  until as many judging rutis handlers exist as rutis policy plugins,
  whoever owns them. A pi one is covered by the wait for the `pi` row's
  handler, as now. One that registers without injecting `yoagent` is
  never its row's: start-up fails after the wait, hinting at `inject`.
  During the session nothing changes: `require_policy` still asks only
  that *some* plugin judges calls (§7), not that this one does.
- **`ask_owner` for plugins** (§3): the service reads the calling row in
  `invoke`. From `plugin:<name>` the question reads "… (asked by plugin
  <name>)", to the owner and in the agent's steer; `from` is then
  ignored (a Warning once a session if it names another plugin, so
  plugins written for #22 keep working). From the `pi` or `dsh` row — were an
  adapter to forward the service for its extensions, which is not planned
  (§3: pi and DSH get no `ask_owner`) — `from` is trusted as the
  extension's name, as `source` is for pi's tools: "(asked by pi
  extension <from>)". A call from no row — a plugin that reached the
  service without injecting it — is refused.
- **What `--measure` shows:** rutis handlers by their owner
  (`plugin:<name>`), not the names they chose; the adapters as
  `pi-extensions` / `dsh-tools`, as before.
- **A plugin that waits for a service nobody provides** is said, not
  left waiting unseen: the host shares every service name (rutis 0.12's
  default), so such a row waits in rutis, and yoagent-rutis's
  `rows::wait_running` names what it lacks (from what the plugin
  injects, looked up in rutis). Rows that wait only for missing services
  are refused at 15 s — a provider may start slowly — not after the
  minute a loading row gets; DSH plugins' are said as the DSH check says
  them, with its hints. The suite (DSH's rows providing each other
  `tools`, `systemPrompt`, `userQuestions`; host services; pi; every
  example) runs with every name shared. Sharing every name also means a
  service a plugin provides can be injected by another runtime's plugins,
  and a second provider of one name fails: plugins are trusted code (§6)
  and DSH's own services live in its private home, but this is wider than
  before.
- **Child plugins:** a plugin written with the rutis SDK (`definePlugin`)
  cannot start one (its context has no `ctx.plugin`); a plain Cordis
  plugin can — a DSH plugin, or a rutis row whose file is a Cordis plugin.
  A child registers through its parent's row when the parent injects
  `yoagent`: its tools are the parent's (its code started the child). When
  the parent does not, the child reaches `yoagent` through no row (other
  rows of the runtime provide it) — nobody's: the parent's `allow` does not
  free it, it asks, and the warning says so — or, where no row provides
  it, never registers (tested for DSH, both ways).

What it does not change: a row is the unit the host can see, not the
code. A call is its row's even when another plugin's code makes it (a
leased service handed on, a service re-provided as `host-ui.ts` does),
and plugins sharing a runtime share a process — per-row trust guards
against honest mistakes and name clashes, not against a hostile plugin
in the same runtime (§6). An honest rutis plugin that injects `yoagent`
(as it must to register at all) sees no difference; an `allow` that
relied on another plugin offering the tool now asks, and is warned about.

## 5. The frontend

yoagent-frontend moved here as `yobuild::frontend` (M0, history kept via
`git subtree split`): `Session` / `Driver`, the browser page and phone link,
the pi-tui terminal UI, UI plugins, plugin dialogs (pi and DSH questions
reach the owner), `PluginHost` (rows, routing, reload, `env`, `terminal`),
`DshHome`. It is application code; yoagent keeps no frontend.

### Answers as Markdown on the page (built 2026-10-21)

The model writes its answers in Markdown (the report's `## Sections`,
lists, tables), and the page showed only part of it: `markdownParts` read
inline marks (`**bold**`, `` `code` ``, https links), so headings, lists
and tables arrived as raw text. The page learns the block level; the
terminal UI already draws answers with pi-tui's own Markdown component
(its text cleaned: "Untrusted text in the terminal UI", below).

- **What is read** — a subset of CommonMark with GFM tables, enough for
  what models write:
  - **Blocks:** ATX headings (`#` to `######`, a closing run of `#`
    dropped); paragraphs, separated by blank lines, each single line break
    kept (as chat pages do: a model's lines are meant as lines); bullet
    lists (`-`, `*`, `+`) and numbered lists (`1.`, `1)`, the first number
    kept), nested by indentation (a leading tab counts four), a numbered
    one interrupting a paragraph only from 1 (`2024.` on a paragraph's
    next line is the paragraph's); fenced code (```` ``` ```` or `~~~`,
    the language shown as a label, a backtick fence's info holding no
    backtick — a one-line ```` ```js``` ```` is code in a paragraph, not a
    fence swallowing the answer — an unclosed fence running to the end,
    as while a reply streams in, and a fence in a list item taking no lazy
    line); block quotes (`>`); rules (`---`, `***`, `___`, spaced or
    not); GFM tables (a header row, the `|---|:--:|` row with its
    alignments, then rows — ending at a blank line, a line without `|`,
    or, for piped rows, one not starting with `|`; a row with fewer cells
    padded, more kept in its last cell; a `|` inside a code span or
    escaped is the cell's).
  - **Inline**, in passes, as CommonMark orders them: code spans (any run
    of backticks closing at the next run of exactly as many; backslashes
    literal inside), then links and images (balanced brackets; a
    destination with balanced parentheses and an optional title; the link
    text read for marks, with no link in it), then emphasis by
    CommonMark's delimiter rules (`*`, `_` — not inside a word — and `~~`;
    emphasis around strong and strong around emphasis), so a mark inside
    code or a URL closes nothing; bare `https://…` (a sentence's stop and
    a closing parenthesis it did not open left out); a backslash escapes
    punctuation.
  - **Not read, shown as text:** images (the page would fetch whatever URL
    the model names — a way to send what it read anywhere: never loaded,
    nor offered as a link, inside a link too; one not yet whole, or with a
    space in its URL, keeps its URL as text), links other than https or
    to a URL the page would not open (whole, as written), raw HTML,
    reference links, footnotes. An empty link text shows the URL.
- **Kept as data** — `lib.js`: `markdownBlocks(text)` returns blocks —
  `heading{level, inline}`, `paragraph{inline}`, `list{ordered, start,
  items: blocks[]}`, `code{lang, text}`, `quote{blocks}`, `rule`,
  `table{align, header, rows}` — and `markdownParts` the inline parts
  (`text`, `br`, `code`, `link{parts, url}`, `strong`/`em`/`del{parts}`).
  Blocks and the marks within them nest together at most `MAX_DEPTH` (8)
  deep; past it the rest is text, kept. Every step is linear in the text:
  no backtracking regex (headings, rules and table separators read by
  hand), code spans and brackets found through indexes, emphasis matched
  over a linked list with CommonMark's floor per kind — the page reads the
  answer again with each delta.
- **Drawn node by node** — `drawMarkdown(doc, element, text)` in `lib.js`
  (the document passed in, so a test draws into a fake one): every node
  made with `createElement`, its text set as text, never `innerHTML` — a
  reply cannot add markup, scripts or styles; a table's alignment set on
  `style.textAlign` from `left`/`center`/`right` only; links https only
  (as before), checked again by `safeUrl`, opening in a new tab,
  `rel="noreferrer"`. A kind it does not know is drawn as its text, never
  dropped; if drawing throws, the raw text is shown. A streaming reply is
  drawn again as it grows (its deltas come ≤ 30 ms apart, merged).
  Thinking text and tool cards stay as they are.
- **Styles** — headings a little larger, not page-sized; tables with
  borders and their own horizontal scroll; code blocks scrolling, not
  wrapping; lists indented.
- **Tool lines** — a line too long for the page (a plugin's card title
  with an absolute path) is cut at the page's width with an ellipsis, its
  ✓/✗ kept; on hover, the tool and its arguments (up to 400 characters).
  A tool call with no arguments (or a big integer in them) still gets its
  line.
- **Tried** in headless Chrome on a report like the gallery's: headings,
  nested lists, the table's right-aligned column, a code block with its
  language, a quote, a rule, a long tool line cut.
- **Tests** — `node --test frontend/web/lib.test.js`: each block and
  inline kind, nesting both ways, links (marks in their text, balanced
  parentheses, titles, empty text), tables (alignments, odd rows, pipes in
  code, their end), fences (in list items, one line, unclosed), CRLF,
  tabs, what interrupts a paragraph, images and HTML and other schemes
  kept as text exactly, the depth limit (the text kept), hostile inputs
  each under a second, every prefix of a streaming answer read without a
  throw or an unsafe link, the drawing into a fake document (list starts,
  tight and loose items, alignment, the language, the link's attributes,
  HTML as text, no image), and `app.js` holding no HTML sink; `web_test`
  unchanged (the protocol is).

### Untrusted text in the terminal UI (built 2026-10-21)

The terminal UI draws answers as Markdown (pi-tui's `Markdown`), but it
prints the session's text as it comes, and a terminal runs the escape
sequences in it. Checked on 2026-10-21 with pi-tui 1.1.0's `Text` and
`Markdown`: `ESC]0;…` (the window title), `ESC[2J` (a screen clear) and
`ESC]8;;…` (a link that shows one thing and opens another) pass through
unchanged; terminals that honour `OSC 52` let text write the clipboard.
Where pi-tui detects hyperlink support (`getCapabilities().hyperlinks`: a
known terminal, or `PI_HYPERLINKS=1`), it also turns `[x](javascript:…)`
into a clickable link, and shows only a link's text — so
`[https://bank.example](https://evil.example)` shows one address and
opens another; without it, the target is printed after the text when the
two differ. A model's text carries what it read (a web page, a file), so
this is a way in, not a theory. pi-tui's other paths stay text: it does
not decode entities, renders raw HTML and images as text, highlights no
code here (no `highlightCode`), and builds LaTeX from the text it is
given.

- **One rule, for display only** — every string drawn that did not
  originate in `terminal-ui.ts` goes through `terminalText(s)` (as
  `t()`) at the call that draws it, before the UI's own styling — each
  `say(…)` argument (`say` itself does not clean), the tool line's
  `Text` (and its `setText` when a card title replaces it), the answer's
  `Markdown` (given the whole accumulated answer each time, not each
  delta), the editor's prefill (`editor.setText`: pi-tui's `Editor` passes
  escapes too) and its one line on stderr; the status line holds only the
  UI's own text. That covers the
  answer, a prompt from any frontend, tool names and arguments, a tool
  card's title (`viewTitle`, a plugin's), notices, a question's title,
  message and options, a `notify`, a run's end with its `error` (a
  plugin's stop reason, a provider's error), the `answered:` echo, and the
  UI's own error lines quoting an exception. The session's messages
  themselves are left as they came: `parseAnswer` and the `uiResponse`
  use the request as received (a cleaned option would not be one the
  session accepts, and the question would never close); an `input`
  question's prefill is shown cleaned, so an answer typed from it is the
  cleaned text. Thinking is not printed by the terminal UI.
- **`terminalText(s)`** — in `lib.js`, tested by `node --test`:
  - escape sequences removed whole, each within 4096 characters
    (`TERMINAL_SEQUENCE_MAX`): CSI (`ESC [`, parameters, a final byte — one
    cut by a control, DEL or a character past ASCII loses only `ESC [`),
    OSC (`ESC ]` … BEL or ST), DCS, SOS, PM and APC (`ESC P`, `X`, `^`,
    `_` … ST; BEL does not end these), nF (`ESC ( B`: intermediates, then a
    final, at most 16 characters), and any other `ESC` with its next
    character, a whole code point (`ESC c`); a string sequence left
    unterminated ends at the next control or at the bound, only its
    introducer removed and the rest shown — injected text cannot hide what
    follows it; an `ESC` before a control goes alone, so a line break after
    it stays;
  - the other C0 controls but `\n` and `\t` (BEL, BS, CR — which could
    overwrite a line — and the rest), DEL, and the C1 controls
    (U+0080–U+009F, the one-byte CSI and OSC among them) removed, each on
    its own (what followed a C1 CSI is shown as text);
  - the bidirectional embeddings, overrides and isolates (U+202A–U+202E,
    U+2066–U+2069) and marks (U+200E, U+200F, U+061C) removed, so text
    cannot reorder what is shown;
  - everything else kept, Unicode included. Its result has no control
    character but `\n` and `\t`, so pieces cleaned apart join clean, and
    cleaning it again changes nothing.
- **Links in answers** — no terminal hyperlinks at all:
  `setCapabilityOverrides({ hyperlinks: false })`, which outranks the
  environment (`PI_HYPERLINKS=1` included). pi-tui then makes no `OSC 8`
  link, and prints a link's target after its text whenever the two differ
  — so `[x](javascript:…)` is the text `x (javascript:…)`, and
  `[https://bank.example](https://evil.example)` shows where it goes; its
  autolink of `www.…` shows `(http://www.…)` (an address's `mailto:` is
  its text: nothing to add). (Proposed first: unlinking
  non-https `OSC 8` links in pi-tui's rendered lines and printing targets
  there — but pi-tui throws on a line wider than the terminal, and lines
  it has fitted cannot grow; turning its links off gets the same, stricter
  — no link opens without its target in view — and nothing is re-wrapped.)
  A terminal that links bare URLs by itself still does, on text that shows
  the URL.
- **Not changed** — the page (its text is set as text, design §5
  "Answers as Markdown"; UI plugins are trusted code), the protocol, the
  plugins.
- **Tests** — `lib.test.js`: each kind of sequence and each of its paths
  (CSI with private parameters, cut by the end, a control, non-ASCII, DEL;
  OSC by BEL and ST; DCS not ended by BEL; a string sequence cut by an
  `ESC` or a control; nF with and without its final, past its 16; an `ESC`
  before a control or an astral character), the one bound for CSI and OSC
  at its edge, each C0, DEL, C1 edge and bidi character, text and Unicode
  kept, and a seeded property test (20,000 random inputs: no control left
  but `\n`/`\t`, no lone surrogate, cleaning again changes nothing).
  `tests/terminal_text_pty.py` (in CI) runs a fixture agent
  (`tests/fixtures/hostile-text`: terminal frontend, `ask_owner`) on a
  pty in a 120-column window, with and without `PI_HYPERLINKS=1`: its task
  holds an `OSC 0` title and a bidi override (the prompt line); its first
  reply asks a question whose title, message and options hold sequences (the
  question's lines, and the tool line's arguments); its answer holds an
  `OSC 0` title with a unique word, an `OSC 52` clipboard write, a
  title-report query `ESC[21t`, a screen clear, C1 controls, a bidi
  override, a `javascript:` link, a link showing one address and going to
  another, and a `www.` autolink. Checked: the payloads' words absent
  (not only their `ESC`), the answer's exact text with each sequence gone
  whole (the screen clear among them — pi-tui sends `ESC[2J` itself, so
  bytes cannot tell), the task's and the question's lines cleaned, no
  `OSC 0`, `OSC 52`, C1 or bidi byte, no `OSC 8` link with a target, the
  answer read as Markdown (no link source left), the three targets shown
  as text, and Ctrl+C quitting with 0. Against the UI before this, 9 of
  its 16 checks fail without `PI_HYPERLINKS` and 13 with it.

## 6. Safety model

Inherited from yoagent and the handoff work, and kept on by default:

- **A pi extension sees the conversation**: `message_end`, `context` and
  the observer events carry every message and every tool's arguments and
  results, to every extension; there is no grant for it (design §12).
- **Fail closed** at every plugin boundary (yoagent-rutis): a policy that
  times out, crashes or is unloaded denies; a redaction that fails
  withholds; a plugin hooking what the loop cannot enforce is refused.
- **The gate holds without a plugin runtime**: `yobuild::builtins::Gate` is a
  required extension in the Rust process, an allow-list: each program it
  lets run has a list of options, and what it writes, deletes or runs is
  judged; anything it does not know asks. A gate written as a plugin (the pi
  example) needs `require_policy` — `policy = true` on its `[[plugin]]`
  (§7) — so a call made while that plugin is down is denied.
- **Questions are never answered for the owner**; with no frontend, the
  safe default (deny, skip).
- **Private homes**: DSH's services get a home of their own (`DshHome`);
  the scratch folder is the agent's.
- **Message size**: calls to plugins stay under rutis's 16 MiB limit.

Not covered, and said so: plugins run with the process's rights. On unix
plugin runtimes and stdio MCP servers start with a kept environment — the
path, home, locale, terminal, proxies and the like — and what their
entries grant, not the owner's whole environment; every credential
yoagent reads and every other variable (`GITHUB_TOKEN`, a search API's
key) is left out unless the manifest hands it over, and the adapters'
helpers get the kept set only (§12, "Step 4", "Step 5"; off unix only
the credentials, set empty; the agent's own `bash` keeps everything):
against honest code acting on the owner's accounts unasked, not a sandbox
— a plugin can read a key where the owner keeps it (`~/.aws/credentials`,
a shell profile, a `.env`); the gate judges command
lines and file tools, not what code does once it runs (a build or a test
runs the project's code, including what the agent edited); and it guards
the owner's files against being deleted or replaced (not against edits),
and does not keep their data from being read or sent. What the model read (a
web page, a file) reaches the owner's screen in its answers: the page sets
it as text, the terminal UI cleans it of control sequences (§5,
"Untrusted text in the terminal UI"); what it says is still the owner's to
judge. A sandbox (container, VM), without secrets the agent should not
have, is the deployment's job.

## 7. What moved, what it becomes

Moved from yoagent in M0 (`integrations/yoagent-frontend/`, its examples,
`scripts/try-*.sh`, the CI job). What each becomes:

| In yobuild now | Becomes |
|---|---|
| `src/frontend/` (`yobuild::frontend`), `frontend/web/`, `frontend/plugins/`, `tests/` | stays (M1: one crate) |
| `examples/handoff_agent.rs`, `coding_agent.rs`, `minimal_browser.rs` | manifests `examples/handoff.toml`, `coding.toml`, `minimal.toml` (M2a; `coding_agent.rs` stays for `--watch`; see "The examples as manifests" below for what a manifest gains) |
| `frontend/plugins/pi-extensions/confirm-dangerous.ts` and its tests | `yobuild::builtins::gate` (M1; the TypeScript version stays as an example) |
| `scripts/try-handoff.sh`, `try-coding-agent.sh` | `try-handoff.sh` runs `yobuild run examples/handoff.toml` (M2a); `try-coding-agent.sh` still runs `coding_agent` |
| CI: `check` (pty checks, the examples' demos in `cli_test`) | grows with the built-ins and manifests |

Stays in yoagent: the loop, providers, tools, `Extension`; yoagent-rutis
(bridge, pi and DSH adapters, the compatibility matrix and its probe);
yoagent-workers.

### The examples as manifests

`handoff_agent.rs`, `coding_agent.rs` and `minimal_browser.rs` became
`examples/handoff.toml`, `coding.toml` and `minimal.toml`, each with its
prompt file and its scripted replies (`examples/scripts/`). The handoff
and minimal `.rs` examples are gone; `coding_agent.rs` stays for its
`--watch` (plugins reloaded as their files change), which neither a
manifest nor `yobuild run` has yet. What a manifest gained for them:

- **Which of yoagent's tools** (`minimal_browser`: reading only, in the
  agent's folder). `[builtins] tools = "coding"` (the default: yoagent's
  six — bash, read, write, edit, list, search) or `"read-only"` (read,
  list, search; paths confined to the agent's folder). Only yoagent's own
  tools: plugin and MCP tools, and `ask_owner`, are what their entries
  give, so an agent with plugins is not read-only.
- **A budget for the whole session** (`handoff_agent`: $2 across runs).
  `[limits] session_budget_usd = 2.0`, beside `budget_usd` (per run);
  either, both, or neither; refused for an unpriced model, like
  `budget_usd`.
- **A plugin that is the agent's approval policy** (`coding_agent`: the pi
  extension `confirm-dangerous.ts`). `policy = true` on its `[[plugin]]`
  turns on yoagent-rutis's `require_policy`: runs start, but while no
  plugin handler judging calls (`before_tool`) is up, every tool call is
  denied rather than run unjudged — fail closed (§6). It is for all
  plugins at once, not the flagged one: the pi adapter judges calls
  whenever any pi extension is loaded, so with pi the guarantee is "the
  pi runtime is up", not "this extension judged". A policy that fails or
  does not answer denies the call; its wait is the dialogs' (`UI_TIMEOUT`)
  plus 30 s, so the owner can answer the question it asks. Refused on a
  DSH plugin: DSH's adapter judges no calls, so every call would be
  denied. Usually with `gate = "off"` — the plugin is the gate; with both,
  a call may be asked about twice. `gate = "off"` with no policy plugin
  leaves nothing judging calls: allowed, for an agent in a sandbox, and
  said as a Warning at start. A policy plugin that dies mid-session
  denies the calls it would have judged (its handler is gone) and the
  owner is told it stopped.
- **The UI plugins** (`todo-plan.ts`, `search-links.ts`: browser drawing,
  offered through the `frontend` service) are rutis plugins
  (`rutis = "../frontend/plugins/todo-plan.ts"`, `runtime = "node"`),
  where they were. A rutis plugin runs in the Node runtime of its own
  package — the nearest folder above it with a `package.json`, where its
  imports resolve (rutis refuses a plugin whose Cordis is not its
  runtime's) — one runtime per package. `examples/` has a `package.json`
  of its own for the pi package a manifest pins (`@juicesharp/rpiv-todo`),
  installed by `scripts/try-coding-agent.sh --setup-only`: "installed
  beside the manifest", until M2b installs it into the cache (§4) and
  the folder goes; the examples' `agent.lock` files are checked in.

**The demos.** `--scripted` replaces the model; the owner is a test. The
handoff demo (asks without waiting; the owner answers #1 after run 1, so
run 2 starts; it writes `notes.md`; two reports) runs as a CI test that
starts `yobuild run examples/handoff.toml --scripted … --web 127.0.0.1:0`
and answers over the page's WebSocket, as the web tests do — no flag
only a test would use. The coding demo runs `--once`: its pi policy,
with nobody there, blocks `rm -rf build`. The minimal one reads. The
terminal UI on a pty stays checked through `coding_agent.rs`
(`terminal_pty.py`: pi's question answered from the keyboard) and
`yobuild run` on the echo fixture (`run_terminal_pty.py`); `coding.toml`
in a pty is not yet.

## 8. Testing

- Every manifest runs **scripted** (yoagent's `MockProvider`) in CI: the
  agent's behaviour without a model or a key.
- `yobuild check --locked` on every example manifest in CI, and the
  handoff demo installing into an empty `YOBUILD_HOME` (M2b's "done
  when").
- The gallery's agents (§11): each `yobuild check --locked` and run
  scripted in CI.
- Manifests with an `[[mcp]]` entry (the gallery's MCP agent aside: it runs
  the pinned filesystem server) run scripted against a small stdio test
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

Two tools for a running agent: `find_plugin` (search) and `load_plugin`
(install without running it, ask the owner, start it, check its tools,
ask again). The design is §14 (M5).

## 11. The skill, and what to do when a run fails (M3; decided 2026-10-19)

### The skill

A coding agent (Claude Code first; the files are plain Markdown, so others
can read them) builds an agent for a person with yobuild. The skill
teaches it the method. That method is the product's main way in: a person
describes an agent, and the coding agent asks, picks the parts, writes
the manifest, vets it, runs it scripted, then live.

**Where it lives.** `skill/yobuild/` in this repository, in Claude Code's
skill layout:

- `SKILL.md`: short — what the skill does and when to use it, the method,
  the rules. Its frontmatter has `name: yobuild` (lowercase and hyphens,
  as the folder) and a `description` saying what it does and when to use
  it ("build an agent", "an agent that …", "run plugins from pi or DSH as
  an agent"), short: until the skill triggers, the description is all a
  coding agent loads.
- `reference/manifest.md`: every manifest key, with the example
  manifests as patterns.
- `reference/scripted.md`: the `--scripted` reply file (a JSON list of
  `{ "text": … }` or `{ "tools": [{ "name", "arguments" }] }`, nothing
  else) and its traps: with `report`, the last reply must hold every
  section name, else the continues use the replies up and the run ends
  `Error` ("(no more mock responses)"); `ask_owner`'s questions are never
  answered under `--once`.
- `reference/parts.md`: capabilities → parts — the built-ins first, then
  plugins that load, then MCP servers. Each plugin row names the version
  `yobuild check` passed and the date; the gallery's CI checks the ones
  it uses, the rest are as of that date.
- `reference/failures.md`: every failure `check` and `run` give, what it
  means and what to do (below).

`SKILL.md` links each reference by its path, relative and one level
deep, so a coding agent reads one only when it needs it. It lives in this
repository, built into the binary, and installed by `yobuild skill
install` (§13; until then, by copying the folder, decided 2026-10-19).

**A working `yobuild`.** Installed, on PATH (§13: until 0.1, `cargo
install --git`), with Node and npm for plugins; `SKILL.md` says so first,
and points at `yobuild new --list` / `--from` to start from a built-in
agent. (Until M4 it ran from a set-up checkout, called by path.)

**The method `SKILL.md` teaches:**

1. **Ask** what the agent is for: its task; who answers its questions and
   where (the web page, a phone, the terminal); what it must never do;
   how much it may spend; where it works (a folder, a git repository).
2. **Pick parts by capability**, fewest first: the built-ins (`ask_owner`
   for questions without waiting, `report` for a required ending, `facts`
   for what changed, the gate, a scratch folder, `tools = "read-only"`),
   then a plugin from `reference/parts.md`, then an MCP server.
3. **Write** the manifest and its prompt file. Pin exact versions (`npm
   view <package> version`); `allow` only tools that read or plan (never
   `bash` or the writers: the gate judges those call by call); never a
   secret in the file (`${NAME}` from the environment); a budget.
4. **Check**: `yobuild check agent.toml --json`. When it prints nothing on
   stdout, an error stopped it before anything started — read stderr
   (`yobuild: …`), which names the file, the entry or the setting. Else
   fix each problem (`reference/failures.md`; once `run/hints.rs` lands,
   each problem carries its `fix`) and decide on each note, until `ok` is
   true.
5. **Run scripted** (`reference/scripted.md`): a reply file that calls
   the tools the task needs; `yobuild run agent.toml --scripted
   script.json --once "<task>"`. Expect every tool call ✓ — or ✗ where
   the gate should ask: under `--once` nobody answers, so `bash` and the
   writers are refused unless the scratch frees them — and `Completed`.
6. **Run live**, after asking the person (it costs money): the provider's
   key in the environment; `yobuild run agent.toml`; tell the person how
   to answer the agent's questions. Commit the manifest, its lock, the
   prompt and the reply file.
7. **When a run fails**, read how it ended (`--once`: the outcome on
   stdout; else `yobuild: the last run ended …` on stderr), find it in
   `reference/failures.md`, change the manifest or the prompt, and run
   again — scripted first.

**The rules:** never `gate = "off"` unless the agent runs in a sandbox;
never write a secret into a file; never add plugin packages to
`adapters/` or edit a lock by hand (yobuild installs pinned plugins into
its cache from the lock it writes); never skip `check`; ask before a live
run.

### When a run fails: what each says and what to do

A failure is one of three kinds:

- **Refused before anything starts**: an error on stderr (`yobuild: …`),
  exit 1, no agent started (packages may have been installed — never
  with their scripts). The message names the file, the entry or the
  setting. `check` gives the same error, and no report.
- **Refused at start**: a plugin, a server or a built-in `run` could not
  start (`check`: the agent `refused`).
- **During a run**: the run ended other than `Completed`, or a tool call
  was refused, or a Notice said something stopped. The agent and the
  owner see it; `run` exits 1 when the last run did not complete.

How a run ended is `Completed`, `Stopped: …`, `Error: …`, `Rejected: …`
or `Aborted: …` (`--once`'s last line; the page's `runEnd`). A stop by a
limit, a budget or the loop check is yoagent's marker:
`[Agent stopped: <reason>]`. A built-in that fails the run is
`[Extension failed: <name>] <reason>`. The tables quote the reason inside
those brackets; `run/hints.rs` matches the reason, so its patterns hold
whatever wraps them.

**During a run — the built-ins, the gate's policy plugins and the loop:**

| Part | What it says | Where | What it means | What to do |
|---|---|---|---|---|
| gate | `Tool call denied: the owner blocked it` | the tool's result | the owner answered Block | nothing, if meant |
| gate | `Tool call denied: not allowed: the owner declined, or nobody answered` | the tool's result | no frontend, or the question timed out; under `--once` nobody is ever there | open the page (or the terminal) and answer. A plugin's or server's tool that only reads or plans: `allow` it on its entry. `bash`, `write_file`/`write`, `edit_file`/`edit` cannot be allowed: the gate judges each call; with `[builtins] scratch = true` (off by default) what the agent writes in the scratch folder is free |
| gate (an `allow` that did nothing) | Warning `plugin A allows \`x\`, but it was offered by \`plugin:B\`, not by it: the tool asks`, or `… which no plugin offered` | a Notice after the first run | a plugin's allowed tool is freed only when its own row offers it | fix the name, or allow it on the entry that offers it (`yobuild check` says which before a run) |
| policy plugin | `Tool call denied: no plugin policy is loaded to judge this call` | the tool's result | `policy = true`, and the plugin that judges calls went away mid-session (at start, `run` waited for it) | restart; the Error Notice `A plugin stopped: …` and the log say why it went |
| policy plugin | `plugin handler \`X\` is no longer available: its plugin unloaded` | the tool's result | the same: its runtime died or it was unloaded | as above |
| policy plugin | `plugin handler \`X\` did not answer \`before_tool\` within …` | the tool's result | the policy took longer than its wait (the dialogs' plus 30 s) | answer its question sooner; the plugin may hang |
| policy plugin | its own reason (e.g. `confirm-dangerous.ts`'s Block) | the tool's result | the policy refused the call | as the policy says |
| plugins | `a required plugin handler failed this run: …` | the tool's result | a plugin's hook failed | the log; `yobuild check` |
| plugins | `the plugin host is not running (…)` | the tool's result; input `Rejected` | the plugin host shut down | restart |
| plugins | Error Notice `A plugin stopped: …`, `the \`X\` runtime is not running (its plugins wait: …)` | a Notice | a plugin or its runtime died | the log; restart |
| report | `[Extension failed: report] the answer was still not accepted after 3 continues` | `Error` | the last answer never named every required section (checked anywhere in it, any case; the prompt already asks for them) | a model that follows the prompt better; fewer, plainer section names (`report = ["Done", "Next"]`); or remove `report`. Scripted: the last reply must name them |
| limits | `[Agent stopped: Max turns reached (50/50)]` (also `Max tokens reached`, `Max duration reached`) | `Stopped` | the run hit its limit — yoagent's defaults (50 turns, 1M tokens, 10 min) when `[limits]` sets none | send "continue": a new run, with fresh limits (not under `--once`, which ends after one run); or raise `max_turns`/`max_tokens`/`max_minutes` |
| budget | `[Agent stopped: budget of $1.00 spent ($1.0012)]` | `Stopped` | the run's `budget_usd` | as limits: each run has its own |
| session budget | `[Agent stopped: session budget of $2.00 spent (…)]` | `Stopped` | all the session's runs together | "continue" stops at once (the session's total stays spent); restart — which starts a new conversation: the history lives in the process — or raise `session_budget_usd` |
| loop | `[Agent stopped: repeated tool call — X was called repeatedly with identical arguments after being asked to change approach.]` | `Stopped` | the agent called one tool with the same arguments over and over | change the prompt, or what the tool returns; send "continue" |
| ask_owner | a question open (with `facts`: `; 1 question(s) waiting on you`) | the facts line | the owner has not answered | answer in the page or the terminal: during a run the answer steers it; when idle it starts the next run |
| facts | `Stopped early: <reason>.` | a Warning Notice | any `Stopped` run (it had no report) | `What to do: …` follows it (#33: facts no longer gives the fix itself) |

**Not failures, though they read like one:** facts' `not a git
repository`, `changes unknown: no snapshot at the start (git: …)`,
`changes since the start unknown (git: …)` are Info: facts cannot tell
what changed (`git init` the folder for facts to). `[builtins] ask_owner
is off` is the error a plugin's own `ask_owner` call gets: the person sees
it only if the plugin shows it (`ask_owner = true` to let it ask).

**During a run — the model and the session:**

| What it says | How it ended | What it means | What to do |
|---|---|---|---|
| the provider's error (a bad key: 401; a rate limit: 429; the network) | `Error: …` | the model could not be asked | the key's variable (`yobuild run` names it when missing); wait and send again; the network |
| `input rejected: …` | `Rejected: …` | an extension refused the prompt (the plugin host not running, above) | as its reason says |
| `stopped with Ctrl+C` | `Aborted: …` | the person stopped it | nothing |
| `The agent's run failed unexpectedly (see the host's logs); later runs may be missing tools.` | `Error` | the agent loop panicked | the log; restart |
| `the terminal UI stopped: …` | exit 1 | the terminal UI's runtime died | the log file it names |

**Refused at start:** a DSH plugin waiting for services nobody provides
(`[dsh] host_services = true`, or install the peer the hint names); a
policy plugin whose judge never registers (does it inject `yoagent`?); an
MCP server with no tools, or an `allow` naming a tool it does not list; a
plugin that fails to load; `cannot make the scratch folder …` (the temp
folder is not writable: fix `TMPDIR`, or `scratch = false`).

**Refused before anything starts:** every manifest error
(`file:line:col: message`); a missing key (`… needs its key in VAR`); a
budget for an unpriced model; the prompt (missing, or `{scratch}` without
the scratch); the lock (`--locked: run without it to resolve the set
again`; an incomplete lock from an old npm: upgrade npm; a version not on
the registry); what `plan` refuses (not installed, a DSH pin the base
holds at another version, DSH files beside DSH packages).
`reference/failures.md` lists each with its fix.

### Making failures actionable in yobuild itself

Part of M3 (decided 2026-10-19), not only the skill's `failures.md`.

The tables help only where the failure is seen. So yobuild says the fix
too:

- **One table in code**, `src/hints.rs` (#33; each hint scoped to where
  its message is seen — a run's stop, its failure or rejection, a refused
  tool call, a refusal at start — and matched on the message's own fixed
  text, so no fix is given for another failure): a pattern of the reason (inside
  yoagent's brackets) → what to do. A test runs both ways: each hint's
  pattern matches a real message the tests produce (from the run and
  built-in tests that already make them: a limit, the report, the gate,
  a session budget), and appears in `reference/failures.md`; and each
  row of `failures.md` that names a fix has a hint. A yoagent upgrade
  that rewords a message fails that test.
- **`run`**: when the last run did not complete, the line that says so
  (`yobuild: the last run ended Stopped: [Agent stopped: Max turns
  reached (50/50)]`, and `--once`'s outcome) is followed by `  what to
  do: …`. A run that ends `Stopped` or `Error` gets the same as a Notice,
  so the page shows it. Facts' `Send "continue"` is dropped for the
  hint's (after a session budget: restart) — done in #33.
- **`check`**: each problem with a hint gets it. In `--json`, problems
  become objects, `{ "message", "fix" }` (`fix` absent when there is
  none), and the report's `version` becomes 2; in the text, a `  fix: …`
  line under the problem. Errors before anything starts keep their own
  wording, which names the fix in most cases (`needs its key in
  DEEPSEEK_API_KEY`, `--locked: run without it to resolve the set
  again`); the few that do not gain one.

### The gallery

Agents built through the skill by a coding agent (M3's "done when": three
of them; a fourth, code-health, for M3b: "pi-lens in the gallery", below), each in `gallery/<name>/`: the manifest, its prompt, its lock,
a scripted reply file, and a README (what it does, how to run it). Each
is `yobuild check --locked` and run scripted in CI, as the examples are
(§8). Agents yobuild's own repository holds; others' agents, shared, are
M4's (vision). The first three (as planned; what was built is in "The
three, in detail" below):

- **Web research** — a search plugin + `ask_owner` + `report`: a brief on
  a question, sources cited (planned on `pi-web-access`; built on
  `dsh-free-search`, the examples' own).
- **Your MCP server** — an `[[mcp]]` entry (the filesystem server, its
  read tools allowed) + `ask_owner`: the pattern for any server.
- **Spreadsheet clerk** — `dsh-univer-office` (a DSH package new to
  yobuild) + `ask_owner` + `report`, if `check` passes it; else the
  fallbacks below. (The choice of these three was left to the
design, 2026-10-19.)

#### How a gallery agent is made and kept (designed 2026-10-19)

**Made through the skill, by a coding agent.** For each agent, a fresh
Claude Code agent gets the skill (`skill/yobuild/`, as if installed, and
what it links to — the examples among them), a working `yobuild` (the
checkout's, called by path) and the person's request in a sentence or
two. It works in a new folder outside the checkout, as the skill says,
and is told not to read `docs/` (this design). It follows the method
(ask → parts → write → `check --json` → scripted `--once`); its questions
are answered as the person would. Its folder — manifest, lock (named
after the manifest, so it moves with it), prompt, reply files — is then
copied unchanged into `gallery/<name>/`. What it got wrong goes back into
the skill (or `hints.rs`), not hand-fixed in the agent, and the agent is
built again. The PR says, per agent, where the coding agent stumbled and
what changed in the skill. Added by hand: `README.md`, after a live
run, and a `scripts/<x>.task` per reply file (the task CI gives
`--once`).

**The folder:**

```text
gallery/web-research/
  agent.toml          the manifest (model deepseek/deepseek-flash, a budget)  ┐
  agent.lock          written by its first `check` or `run`                   │ the coding
  prompts/agent.md    the prompt                                              │ agent's
  scripts/<x>.json    the scripted replies, `demo` among them                 ┘
  scripts/<x>.task    each one's task                                         ┐ by hand
  README.md           what it does; what it needs (keys, network, files it    │
                      reads or writes outside its folder); how to run it; one │
                      live run's result: date, model, cost (`--once` says it) ┘
```

The agent works in the folder `yobuild run` starts in, never in its
gallery folder: the README says to `cd` into the work, then `yobuild run
path/to/gallery/<name>/agent.toml`.

The cost a README records is the one `--once` prints on stderr when each
run ends (decided 2026-10-19, after the first gallery agent's live run
had none to record): `  Cost: $0.0021 (12345 tokens in, 678 out; 3
turns)` — yoagent's `total_cost_usd` and the tokens of the same scope
(`total_usage`), in counted as facts counts them (input + cache reads +
cache writes). `not priced` when part of the run has no price (a scripted
model); with no turn, `unknown` for a run aborted or failed (its figures
are lost with it: Ctrl+C), else `nothing spent` — never a zero that is
not one. The web page already shows each run's cost; stdout stays the
answer and the outcome.

**Kept by CI** — one test per agent in `tests/gallery_test.rs` (the
binary, the shared `YOBUILD_HOME`, as `cli_test`; each `yobuild` in a
temp folder with a temp `HOME`, so a plugin's cache there is the test's,
and killed after 5 minutes): `yobuild check --locked --json` → `ok`, no
note, each name an entry's `allow` lists `allowed`, no tool that writes
with `tools = "read-only"`; then every reply file `run --scripted
scripts/<x>.json --once --locked "<x.task>"` → `Completed` with the
script's last `text` as the answer (with a `report`, `Completed` means
its sections were there: a run without them fails), each scripted call
made once, no `Error:` notice, every call `✓` — but a plugin's or
server's own `✗` (a third party's day, `scripted.md`), never the
bridge's (its plugin unloaded or stopped answering) and never a denial;
then every tool `check` lists as asking, called with nobody there:
denied. A gallery agent whose plugin cannot load any more fails CI, as an
example's does. `skill_test`'s **CI** pins read `gallery/*/agent.toml`
(as `run` reads them) as well as the examples: a **CI** row may name a
gallery agent's pin, and every gallery agent's pin is a **CI** row in
`parts.md` (the filesystem server's entry becomes a table row, its pin
and mark on one line). Each agent's PR adds it to CLAUDE.md, as the
examples are.

**The three, in detail:**

- **Web research** (`gallery/web-research/`): planned on
  `pi-web-access`; the coding agent, following the skill ("**CI** rows
  first"), chose `dsh-free-search@0.8.1` — tested, keyless, no third-party
  search service — and the owner kept that (2026-10-19). Its seven
  searches allowed, `free_search_test` asking; `ask_owner` (a question
  too broad is sent back to narrow, not guessed), `report = ["Short
  answer", "Sources used", "Waiting on you"]`, `tools = "read-only"`, a
  budget. Three reply files: `demo` (a question researched), `broad` (too
  broad: `ask_owner`, the run ending with it waiting) and `answer` (the
  owner's answer as the task, in the form yobuild sends it, researched).
  Its plugin keeps a page cache in `~/.cache/dsh-free-search/` (the
  README says so). Its searches really search
  (Bing) in CI: a search that fails on its own does not fail the test.
  `pi-web-access` stays in `parts.md` untested, for a later agent.
- **Your MCP server** (`gallery/notes-keeper/`, the coding agent's name
  for it: the request was a notes folder through the filesystem server):
  `@modelcontextprotocol/server-filesystem@1.0.0` as `files`, `args =
  ["."]` (the folder `yobuild run` starts in), its ten read tools allowed,
  its four writers asking (no delete tool); `tools = "read-only"`, so the
  server is the only way to write and the gate asks each time; no
  `ask_owner` (the request did not need one); $0.25 a run, $1 a session.
  2.0.0 came out on 2026-10-19: kept at the checked 1.0.0. The README is
  the pattern for any server (which lines change for another). Reply
  files: `demo` (reads, then an edit, a new file and a rename, each denied
  under `--once`) and `question` (an answer, and the server's own refusal
  of a path outside the folder: its `✗`, tolerated as a server's); the
  test puts the two notes they read in the run's folder. Unlike §8's other
  MCP manifests it runs the real server, from npm.
- **The third**: first a DSH plugin yobuild has not used before, since
  the web researcher's `dsh-free-search` is already in
  `examples/handoff.toml`. Checked by hand on 2026-10-19, neither DSH
  candidate works: `dsh-univer-office@0.3.7` passes `check` (its 14 tools
  listed) but every call fails `Error [SESSION_SCOPE_UNAVAILABLE]: Univer
  tools require a calling agent with a workspace` — DSH's own agent
  sessions, which yobuild's DSH adapter does not give, with host services
  or without; `@dfy-plugins/dsh-visualize@0.1.5` does not install (its
  peers conflict with the DSH base: npm `ERESOLVE`). Both are recorded in
  `parts.md` as not usable. So the third is the roadmap's **Code-quality
  reviewer** (`gallery/code-reviewer/`). The coding agent picked
  `pi-lens@4.4.3` from the skill and found it unusable too: it treats
  the project as untrusted under yobuild, starts no language server, and
  reports every file clean (its own log said so). That was before the
  `trust` grant (step 3): see "pi-lens in the gallery", below. It took ESLint's MCP
  server instead, `@eslint/mcp@0.3.15` (`lint-files`, absolute paths, the
  project's config), with `tools = "read-only"`, `ask_owner`, `report =
  ["Fix first", "Fix soon", "Worth a look", "Not checked", "Waiting on
  you"]`, `facts`; $0.50 a run, $2 a session. No part runs `tsc` without
  letting the agent write, so type errors are found by reading and the
  report says what was not run. Reply files: `demo` (`{cwd}/src/…` for
  `lint-files`: reply files fill `{cwd}`, as prompts do, since
  2026-10-19) and `no-writes` (every write, edit, command and read outside
  refused: the test's `refusing`). Its first live run showed ESLint's
  findings missing: yoagent 0.25.7's providers send the model only the
  first text block of a tool result, and ESLint answers in three. yobuild
  joined them (an `after_tool` extension) until yoagent 0.25.9 sent every
  block (yologdev/yoagent#347, all five providers);
  the second live run reported all three of ESLint's errors. Lessons:
  `check` loads plugins and lists their tools, it does not call them, and
  a `✓` says a tool ran, not that it worked — two of four candidates
  passed `check` and did not work. A gallery demo calls every tool its task
  needs, and a live run reads what came back.

**Order of PRs**: this design; then one PR per agent (web research, your
MCP server, the third), each with the skill fixes its build found.

#### pi-lens in the gallery (proposed 2026-10-23)

A fourth gallery agent, for M3b rather than M3.

**Why.** M3b's "Done when" asks that `pi-lens` work in a gallery agent,
with its tools' results checked, not only `✓`. The code reviewer above
left pi-lens because yobuild then told every pi extension the project was
untrusted. In that state pi-lens starts no language server, and its
diagnostics come back empty: the coding agent read that as "every file
clean", and pi-lens today says "unavailable". `grants = { trust = true }`
(step 3, #46) changes that answer. pi-lens reads it at every
`session_start`, and since #61 `check` names the fixed answer
`ctx.isProjectTrusted() → false` when `trust` is missing. pi-lens was
checked by hand on 2026-10-21, but no gallery agent uses it, and no test
reads what a tool answered.

**Measured on 2026-10-23** (macOS). The setup:
- `pi-lens@4.4.3`, `trust = true`, read-only;
- a two-file TypeScript project whose `src/sum.ts` passes `"2"` where a
  `number` goes;
- a fresh `HOME`, `--scripted … --once`.

What happened:

| | cold `HOME` | warm `HOME` |
|---|---|---|
| `yobuild check` | 15 s; `~/.pi-lens` 76 KB: nothing downloaded while loading | — |
| `read_symbol`, `module_report` (tree-sitter, in process) | ~0.2 s each | the same |
| `lens_diagnostics` `{source: "lsp", scope: "paths"}` | 23 s; `~/.pi-lens` grows to 87 MB | 17 s (the server starting) |
| its finding | `typescript:2345` "Argument of type 'string' is not assignable to parameter of type 'number'", line 4 | the same |

- **What the first LSP probe downloads.** Into `~/.pi-lens/tools`:
  `typescript` and `typescript-language-server`, from npm, at `^` ranges
  (not pinned). Into `~/.pi-lens/bin`: `opengrep` and `typos-lsp`, from
  GitHub.
- **The first probe found the error in each cold run tried (two).**
  `parts.md` says a first call "may come back 'inconclusive' while those
  install". That was not reproduced, so the plan below does not rely on
  it either way.
- **`symbol_search` is not ready at first.** On a cold project its first
  call may answer "Word index is building in the background for this
  workspace — retry this query shortly" (`✗`). In another cold run it
  answered at once.
- **The kept environment held.** `PI_LENS_DISABLE_LSP_INSTALL=1`, set in
  the parent but not granted, did not reach pi-lens, as step 5 means: it
  installed all the same.
- **A bug found along the way.** `run` said `ctx.model`'s fixed answer
  twice, because the answer differs: `undefined` at `session_start`, the
  model's id in a run. `Said` dedups on the whole note.

**The agent: `gallery/code-health/`, new.**
- **Why new.** The code reviewer stays, as the pattern for a linter's
  MCP server.
- **How it is made.** Through the skill by a coding agent, as the
  others are. The request: "a code-health checker for a TypeScript
  project: type errors and an outline of what is where, read-only". The
  coding agent works in a folder holding exactly the two TypeScript files
  the test puts in the run's folder (below), told they are the sample
  project.
- **Expected parts,** which the skill's `parts.md` leads to:
  - `pi-lens@4.4.3`, `grants = { trust = true, model = "info" }`. pi-lens
    reads `ctx.model`, and `parts.md` will say to grant `model` so it
    gets an answer rather than a fixed one (and so `check` has no
    fixed-answer note);
  - its reading tools allowed: `lens_diagnostics`, `project_report`,
    `module_report`, `read_symbol`, `read_enclosing`, `symbol_search`,
    `effective_config`;
  - `pi_lens_activate_tools` left asking: it changes which tools the run
    is offered, and `parts.md` will say so. Whatever the coding agent
    decides for it is kept, as for any part;
  - `tools = "read-only"`, `ask_owner`, a `report`, and a budget.
- **Its README** (by hand) says what `trust` lets pi-lens do: download
  and run language servers and scanners, from npm at unpinned ranges and
  from GitHub, and run the project's own code among what they load. It
  also says the first diagnostic is slow, about 25 s cold.

The earlier text above ("found it unusable too") and the code reviewer's
README (hand-written) gain a line: that was before `trust`; see this
section.

**Results checked: `expect` on a scripted call.** A reply file's call may
say what its result must contain:

```json
{ "tools": [{ "name": "lens_diagnostics",
              "arguments": { "source": "lsp", "scope": "paths", "paths": ["src/sum.ts"] },
              "expect": "typescript:2345" }] }
```

- **The value.** `expect` is a string or a list of strings. It is
  matched case-sensitively against the result's text blocks joined with
  `\n`. Each string must appear.
- **How a call finds its expectation.** `run/script.rs` puts it in the
  mock call's `provider_metadata` (`{"yobuild_expect": [...]}`). The mock
  gives every call an id `mock-tool-<index>` that repeats on every turn,
  so the id cannot carry it. The metadata travels with the call into the
  assistant message (`Content::ToolCall`).
- **Who checks it.** A scripted run adds the extension `expect`. Its
  `after_tool` finds its call in the last assistant message, by the
  call's id within that turn (unique there, parallel calls included), and
  reads the expectation. It checks error results too.
- **A miss.** The result becomes an error whose first line is "the reply
  file expected `X` in the result: <the result's first line>". The
  original text follows, and `is_error` is set. `--once` shows it as `✗`,
  and the run goes on, as any refused call's does.
- **A call that never runs.** No `after_tool` is called for one denied
  in `before_tool` (the gate, a policy, nobody there) or for an unknown
  tool. Its expectation is said at the run's `finish`, as a Warning: "the
  reply file's expectation for `x` was not checked: the call did not
  run". The extension learns each turn's expected calls from its
  `MessageEnd` (sent before its tools run) and strikes each off in
  `after_tool`; what is left when the next turn's calls come, or at
  `finish`, never ran.
- **Its place.** Last among the extensions, so it sees the text the
  model sees. The gate's `after_tool` comes before it, so a file a call
  made is recorded as the agent's even if `expect` then fails the call.
  That is harmless: the call ran.
- **The run's outcome.** A miss does not change it: the run can still
  end `Completed` and exit 0. A person's own scripted check reads the
  `✗` lines. `scripted.md` says so.

Why in yobuild, not only in a test: `gallery_test` must run the binary,
because each run needs a folder of its own and parallel tests cannot
change the process's folder (`run()` works in the process's folder:
`Options` has no `cwd`). `--once` prints only each call's `✓` or `✗`
first line, so no test can read a result from outside. Checking it as it
comes back also works for any reply file: the gallery's, the examples',
and a person's own.

**In CI, with the network.**
- **`demo`.** It calls `project_report`, `module_report`, `read_symbol`
  and `read_enclosing` (each with an `expect` naming what the two files
  hold), then `lens_diagnostics` with `source: "lsp"` twice, the
  `expect: "typescript:2345"` on the second only (in case a first call
  ever comes back "inconclusive"; yoagent steps in only after three
  identical calls in a row). `symbol_search` stays allowed but uncalled.
  If a reply file ever calls it and gets "retry shortly", that `✗` is the
  plugin's own, tolerated as such.
- **A `HOME` shared across the agent's runs.** This is an exception to
  "Kept by CI"'s temporary `HOME` per `yobuild`. All of code-health's
  `yobuild` runs (`check`, each reply file, the asking tools) use
  `CARGO_TARGET_TMPDIR/pi-lens-home`.
  - **Speed.** A CI job downloads pi-lens's tools once (about 23 s),
    and later local runs are warm (about 17 s per LSP probe; the agent's
    test takes about a minute and a half warm).
  - **Not cached by CI.** The workflow removes `target/tmp/pi-lens-home`
    before testing, as it removes `target/tmp/yobuild-home`: the build
    cache may bring `target/tmp` back.
  - **Kept locally, with what that means.** A warm local `HOME` keeps
    the `^`-range versions it first downloaded, and whatever pi and
    pi-lens keep there (pi-lens's per-project index) carries over
    between runs. Two `cargo test` runs at once would share it. Deleting
    the folder resets it.
  - **The other agents** keep a temporary `HOME` each.
- **The cost.** The network is already a CI dependency (npm, the web
  researcher's Bing). Unlike a search, a failed download here fails the
  test: pi-lens then says "unavailable" and the `expect` misses. That is
  the point: the agent's whole value is that probe.
- **The test's project.** Two files, inline in the test (as
  `notes_keeper`'s notes are): `tsconfig.json` and `src/sum.ts`, the
  measurement's. The coding agent works against the same two. Changing
  them means building the agent again, as for any change to what its
  reply files were written against.

**`gallery_test` changes:**
- `holds` takes an optional shared `HOME`.
- It refuses the miss wording ("the reply file expected `") wherever it
  would tolerate a plugin's own `✗`, and the "was not checked" Warning
  like an `Error:` notice.
- Among `check`'s notes it accepts "its N skills are not loaded"
  (pi-lens ships 4).
- `code_health` requires that `demo`'s last `lens_diagnostics` call
  carries an `expect` holding `typescript:2345`, so a reply file without
  one cannot pass by checking nothing.

**`Said` dedups fixed answers per question.** One note per extension and
question (`who` and the question, as `Said::pi` reads them), whatever
the answer, the first answer said. It applies in `run` and in `check`,
with a unit test.

**Said where.**
- `scripted.md` (PR 2):
  - `expect`, with an example `skill_test` parses;
  - "an unknown key is an error" naming it;
  - "a `✓` says the tool ran" pointing at `expect`.
- `parts.md` (PR 2):
  - the first LSP probe's downloads and time;
  - `symbol_search` warming up;
  - `PI_LENS_DISABLE_*` passing only when granted;
  - `model = "info"`;
  - why `pi_lens_activate_tools` asks;
  - and its "inconclusive" sentence made "may".
- `parts.md` (PR 3): pi-lens's **CI** mark. `skill_test` fails a **CI**
  row no agent pins, so it comes with the agent.
- CLAUDE.md: the gallery's fourth agent, `holds`' notes and shared
  `HOME`, `expect`. The roadmap's M3b too.

**Tests.**
- `run/script.rs`: `expect` read (a string, a list) into the call's
  metadata, and refused in any other shape.
- `run_test`, a scripted call through a real MCP server:
  - its `expect` met (`✓`);
  - its `expect` missed: a `✗` with the wording above;
  - missed on an error result;
  - a denied call's expectation said "not checked".
- `plugin_log`: one note per question.
- `gallery_test` `code_health`: as above.

**Decisions for review.**
1. *A new agent, `code-health`* (recommended: the code reviewer stays the
   linter-MCP pattern), or the code reviewer rebuilt on pi-lens.
2. *`expect` in reply files, checked as results come back* (recommended:
   works for every scripted run, written by the coding agent through the
   skill), or a test-only reading of results (the binary would need a way
   to print them).
3. *The LSP probe in CI, with a shared `HOME`* (recommended), or only
   pi-lens's tree-sitter tools in CI and the LSP probe checked by a live
   run. That second option would not meet "results checked" for the tool
   that matters.

**Built** (2026-10-23): `expect` and the dedup (#63); the agent, built
through the skill twice. The first build's `unclear` reply file probed
`lens_diagnostics` once and, in a folder new to pi-lens, found nothing
(its `demo`, probing twice, passed): `parts.md` now says to probe twice
everywhere, the `expect` on the second, and `scripted.md` has a trap for
tools that warm up. Its other stumbles, now in the skill:
- `project_report`'s cold "retry shortly";
- `read_enclosing` finding nothing for a top-level line;
- `module_report` leaving out exported constants;
- pi-lens's `git status` in the project;
- where an agent that reads the whole project lives (beside it, in
  `SKILL.md`).

The second build ran each reply file twice on fresh copies, each passing
locally, but on CI's Linux runner, with pi-lens's servers fetched cold,
both probes of one reply file came back with no finding: each waits 15 s a
file for the server by default (`waitMs`), and a server still starting
outlasts that, answering `inconclusive` on its `Outcomes:` line. The skill
now says `waitMs: 60000` on every probe and to read that line (never
"clean" from `inconclusive`), and step 5 of `SKILL.md` names warm-up `✗`s.
The third build passed each reply file three times, once with an empty
`HOME`.

CI still failed with the third build. The cause was not the wait.
- **What CI showed.** Its logs, printed on failure, showed pi-lens taking
  the runner's global `tsc` ("ensure typescript: already available at
  tsc") instead of installing its own.
- **Why that fails.** It then admits no absolute tsserver ("no admitted
  absolute TypeScript compiler"), so its TypeScript server is unavailable
  whatever the trust.
- **Reproduced locally** with a `tsc` on `PATH`. pi-lens 4.4.3 has no
  setting for it.
- **The fix.** CI removes the runner's global `tsc`/`tsserver` before
  testing. `parts.md` and the agent's README say it: run it where no
  global `tsc` is on `PATH`. The agent's prompt already reports an
  unconfirmed probe as not checked.
`holds` builds the asking tools' arguments from their schemas
(`accepted`): pi-lens validates before the gate, and `{}` was refused as
invalid rather than asked.

**Order of PRs:**
1. this design;
2. `expect`, the `Said` dedup, `scripted.md` and `parts.md`'s notes,
   and the forward line in the code reviewer's README;
3. the agent, built through the skill, with its test, `parts.md`'s
   **CI** mark, the CI workflow's `rm`, and the skill fixes its build
   finds.

## 12. The host layer: what a plugin's own app gives it (proposed 2026-10-19)

The gallery's third agent found it twice in a day: a plugin that loads,
lists its tools and passes `check`, then does nothing useful, because it
expects something its own app gives it and our adapters answer with a fixed
stub. `dsh-univer-office@0.3.7` reads its workspace from the DSH agent making
the call (`exec.agent.session.header.cwd`), and the DSH adapter calls tools
with no agent: every file tool (13 of its 14) fails
`SESSION_SCOPE_UNAVAILABLE`. `pi-lens@4.4.3` asks pi whether the project is
trusted, the pi adapter answers a fixed `false`, and it starts no language
server and calls every file clean. Neither is a missing frontend: they are
the **host** — the agent, session, model and trust a plugin's own app
provides at run time. Surveyed on 2026-10-19 (text matches in the published
bundles of 29 DSH plugin packages; pi 1.1.0's `ExtensionAPI` and
`ExtensionContext` against the adapter):

- **DSH**: 16 of the 29 read `exec.agent.session.header.cwd`, 11 the
  session's id, 11 the agent's id — among them univer, visualize, the two
  image generators, dsh-ops, dsh-palimpsest, dsh-remote, dsh-tiddlywiki,
  dsh-mnemosyne, dsh-mcp-connector (the 29 include DSH's app plugins, which
  this layer will not cover, below). The base's `sessionPersistence` is an
  abstract service definition with no backend (no `create`, `open`,
  `list`); approvals need an open turn (`turn/start` in the session log);
  `dsh-user-questions` accepts only an agent that is a live root in `agents`.
- **pi**: fixed answers for `isProjectTrusted` (`false`, where pi trusts any
  folder without project-level pi resources such as a `.pi` config),
  `model`/`modelRegistry` (none), `sessionManager` (an in-memory one without
  the conversation); `sendMessage` recorded, `sendUserMessage` refused;
  observer events `turn_*`, `agent_end`, `message_start`/`message_update`
  and `tool_execution_*` never fired (`message_end`, `context`,
  `session_start`, `session_shutdown` are, the first two as read-only
  checks) — and `pi.exec` runs any command **ungated** today (closed by
  default since yoagent-rutis 0.7.0).

The fix is generic, in three levels, each where the project's rule puts it
(the loop's in yoagent, an ecosystem's vocabulary in its adapter, policy and
UI in yobuild).

### L1 — host services (yobuild, Rust; ecosystem-neutral)

Offered to every plugin runtime through the bridge beside `frontend`,
`ask_owner` and `ui`, always provided (rutis starts no plugin whose injected
service is missing) and answering `Err` for what the manifest did not grant —
the `provide_ask_owner(None)` pattern:

| Service | What it gives | Backed by |
|---|---|---|
| `workspace` | the agent's folder; whether the project is trusted | `run`'s cwd; the `trust` grant |
| `session` | `{ id, generation }`: an id yobuild mints per session, its generation bumped by each Reset; run boundaries; wake (a granted plugin's text, tagged as its own) | `Session` (new: an id, a generation); yoagent's `RunContext` (`run_id`, `depth`, `parent_run_id`), seen by plugins as yoagent-rutis's `RunInfo`; `Session` steering |
| `approval` | judge one command, or ask the owner — the question naming the plugin: a rutis plugin by its row; for the `pi`/`dsh` rows, the extension's or plugin's name the shim passes (as `ask_owner`'s `from`: per extension for pi, for DSH once attribution holds) | the gate, through a new public entry — `judge::Ctx` is private and built only from a line — such as `Gate::check_argv(argv, cwd) -> Check` (a `Ctx` at the call's cwd, each argument a literal word); files such a command makes are not recorded as the agent's own; `Session::ask` |
| `llm` (later) | model info; model calls charged to the run | `Model`; yoagent's provider; the run's spend (yoagent 0.25.10: `RunContext::spend_handle()`, a `SpendHandle` whose `add(&usage, &model)` the run's budget counts at its next request and its stats keep under `extensions`) |

yobuild also sets `Agent::with_run_label(<session id>)`, for logs only: a
label is informational in yoagent, not an identity.

**Built (M3b step 1, 2026-10-19)** as `run/host.rs`, the services named
`host_workspace`, `host_session` and `host_approval` — every shared service
reaches every runtime, and DSH has its own `approval` and `sessions`. Model
info is `host_session.model()`, not an `llm` service yet. Every method
refuses arguments past its own:

- `host_workspace.cwd()` (`workspace`), `.trusted()` (`trust`; `false`
  without it);
- `host_session.info()` (`{ id, generation }`, any plugin), `.model()`
  (`model = "info"`: provider, id, context window, output limit — the
  preset's when there is one; the manifest's model under `--scripted`
  too), `.wake(text)` (`wake`; one line, no control character, so nothing
  after it passes for the owner's words): `[plugin <name>] <text>` handed
  to the session, which
  decides — steering for a run going on; else a run of its own, never
  before the owner's first run, at most 3 a session for all plugins
  together and none under `--once` (the session's allowance, set once). A
  wake the run never read (sent as it ends) counts as one that starts a
  run — unless the owner's own late steering starts it anyway, which the
  wake joins — so a plugin waking at each run's end cannot keep the agent
  going; one a run stopped (aborted, reset, failed) before reading is
  dropped, told by this run's messages, each copy of a repeated text
  counted; each one not delivered is a Warning notice. It answers
  `"sent"`, and refuses at once what surely cannot be delivered;
- `host_approval.judge(argv, cwd?)` (any plugin): the agent's own gate (a
  clone, knowing the files the agent made) through `Gate::check_argv` —
  each word single-quoted into `cd <cwd> && <words>` and judged as `bash`,
  so exactly those words, the first never read as an assignment or a
  keyword (nor a wrapper's next word, `nohup x=./evil`, the line's own
  rule); `cwd` the agent's folder, or one relative to it (not UTF-8: it
  asks); `{ free: true,
  judged: false }` with `gate = "off"` (a `policy` plugin may still judge
  the agent's own calls);
- `host_approval.ask(question, details?)` (a rutis plugin's row only — a
  DSH plugin's own `plugin:<name>…` rows wait for its host): Allow/Block
  naming the plugin, answered `"allow"`, `"block"` or `"unanswered"`
  (nobody there — said as a Warning notice — a timeout, or dismissed); an
  error once the session ended.

Grants are read for rutis plugins only (a refusal to anyone else says no
grant is theirs yet); a pi extension's and a DSH plugin's are refused when
the manifest is read (at its `grants` table)
until their hosts come (steps 2 and 3). `check` lists each entry's grants.
Fixed answers and the `exec` grant come with the pi host (`observe`,
first planned, is dropped: steps 2 and 3, below).

**Who enforces a grant.** L1 sees only the calling row, and every pi
extension calls through the one `pi` row, every DSH plugin through the one
`dsh` row (design §4, "Who a plugin is"). So L1 enforces grants per row —
which is per plugin for a rutis plugin — and the **shims** enforce them for
pi and DSH:

- **pi**: per extension, by the extension's source file. The adapter knows
  it for a tool call (`sourceIn`), and can give each extension its own
  `ctx` for events (today one shared `context()`). But `pi.exec`,
  `sendMessage` and `sendUserMessage` live on the `pi` API object, not on
  `ctx`: pi's loader calls its own `execCommand` directly, and messages go
  to the shared runtime with no extension's name. So the adapter loads each
  extension through a factory wrapper of its own — importing the module
  and calling pi's `loadExtensionFromFactory((api) => factory(wrapped(api,
  path)), …)`, where `wrapped` overrides `exec`, `sendMessage` and
  `sendUserMessage` for that extension. (Shipped in 0.7.0 as `loadGated`,
  overriding `exec` only; messages are still the shared runtime's: steps 2
  and 3, below.)
- **DSH**: per plugin only if the adapter can tell which plugin registered a
  tool — DSH's `tools` registry keeps no owner, but `tools.register` runs in
  the registering plugin's Cordis context, which names its fiber. 0.7.1's
  host does not use it: one stand-in serves every DSH call, so a DSH grant
  applies to **every DSH plugin in the runtime**, and the manifest writes it
  on every one (the one rule, in steps 2 and 3 below).

### L2 — ecosystem shims (yoagent-rutis adapters)

They translate L1 into each ecosystem's contract, as `host-dialogs.ts` and
`host-ui.ts` already do for questions.

**DSH host** (loaded only when a DSH grant asks for it; once loaded, every
DSH call goes through it — as 0.7.1 ships it, in steps 2 and 3 below,
which supersede what differs here):

- One stand-in DSH `Agent` per yobuild session **generation**: a Reset ends
  it (its turn closed, its session left) and the next run makes a new one,
  so a plugin's state (univer's worktrees, approvals DSH remembers) does not
  carry over a Reset. It is registered as a root in DSH's own `agents`
  (`@deepseek-ai/dsh-agent`, `agents.register`); its `id` is its session's
  id (`enter()` checks); its `Session` comes from DSH's own `sessions`
  (`dsh-session`, `meta.cwd` from `workspace`); its `ctx` is
  `createScope(root, agent)` (`@deepseek-ai/dsh-scope`); its `steer` and
  `followup` (0.7.1: a waking `send` and `inject` too) route to `session`
  wake (with `wake` granted; else refused); its `inbox` is empty
  (`nextTurn`/`nextStep` none). Continued questions
  (`dsh-user-questions` answering a timed question later with
  `agent.steer`, released on a session `user/message` it waits for) are out
  of scope: it tracks them only after a `request/header` event, which an
  empty transcript never has, so its `answer()` declines — a question
  waits for its answer in the call, as today. `cancel` is a no-op it
  reports, `whenIdle` resolves at once, `runMaintenance` runs its task at
  once (0.7.1);
  `create`/`resume` stay unavailable (no factory).
- **Turns**: opened lazily, at a run's first DSH call (`turn/start`), closed
  at `finish` (`turn/end`); an open turn left by a run whose `finish` never
  came (Ctrl+C, a Reset, a runtime that died, `finish`'s 5 s limit) is
  closed before the next `turn/start`, and at the generation's end.
- **The session log**: `sessions.create` persists nothing (DSH's agent loop
  attaches the writer), so the shim writes the session's `SessionEvent`s to
  `@deepseek-ai/dsh-session-persistence-jsonl`, rooted in the private
  `DshHome` (with `YOAGENT_DSH_HOME` set, the user's own DSH home: said by
  `check`). The stand-in's rows — `dsh-session`, `dsh-agent`,
  `dsh-user-approval`, the jsonl backend — load with a DSH grant, whatever
  `[dsh] host_services` says; the jsonl backend takes the place of
  `host-services.json`'s abstract `dsh-session-persistence` row (in 0.7.1
  by `dsh_host_rows`, same id; the file is unchanged), since both register
  `sessionPersistence` and a second provider fails — DSH
  plugins that inject `sessionPersistence` get it from the jsonl backend,
  so they need a grant too. Its transcript stays empty: no conversation is
  mirrored into it.
- **Dialogs**: `host-dialogs.ts` today wraps `tools/pre-execute` for
  agentless `yoagent:` calls and steps aside on agent-scoped
  `user-questions/request`. It gains an `approval/request` answerer (DSH's
  own `dsh-user-approval` loaded) and answers the agent-scoped
  `user-questions/request` for the stand-in, both through the `ui` service;
  the agentless path stays while the host is not loaded. (In the probe the probe
  script answered them.)
- **Sub-agents**: when yobuild delegates (it has no delegation tool today):
  a child agent with its own session (`enter(child, parent)`, then
  `announce`); DSH refuses questions from a child (`DELEGATED_CALLER`).
- Later: a model-info-only `llm` adapter, a read-only `settings`.

**pi host**: each fixed answer becomes a grant, applied per extension
(`model` runtime-wide: the one rule, below):
`isProjectTrusted` from `trust`; `model`/`modelRegistry` read-only from
`model = "info"`; `sendMessage`/`sendUserMessage` from `wake`; `pi.exec`
from `exec` — `"off"` (the default: refused, said as a Warning naming the
extension, once a session), `"gate"` (its argv judged as one command's
words through `Gate::check_argv`, not as a shell line), or `"free"`. The
observer events (they carry the whole conversation and every tool's
arguments and results, a `.env` read included; langfuse and langsmith send
them off the machine) were to need an `observe` grant; it is dropped
(below).

### L3 — the frontend (later; its own milestone)

Only what is shown: a plugin origin — a second listener, another origin than
the page (whose `/ws` token drives the agent), plugin routes reverse-proxied
there with forced `CSP: sandbox allow-scripts`, a cookie bound to host and
port, Host/Origin checks, no CORS, off in phone mode unless every request
carries the cookie — and framed UI plugins (`renderTool` in a sandboxed
iframe over `postMessage`), which changes the contract of today's trusted UI
plugins (`search-links`, `todo-plan`). DSH's full web client is not hosted.

### The manifest: `grants`, closed by default

```toml
[[plugin]]
dsh    = "dsh-univer-office@0.3.7"
config = { telemetry = false }
grants = { workspace = true }               # a stand-in DSH agent with the agent's folder

[[plugin]]
pi     = "pi-lens@4.4.3"
grants = { trust = true, model = "info" }   # it may run project code (its language servers)
```

`grants` (named so, not `host`: `[dsh] host_services` is another thing), a
table read with `deny_unknown_fields`, every key off when absent:

| Key | Values | pi | DSH | rutis | Turns on |
|---|---|---|---|---|---|
| `workspace` | bool | — (pi has `cwd`) | yes | yes | the stand-in agent (DSH); `workspace` (rutis) |
| `trust` | bool | yes | — | yes | `isProjectTrusted`; `workspace`'s trust |
| `model` | `"info"` (later `"call"`) | yes | later (a model-info `llm`) | yes | `ctx.model`/`modelRegistry`; for rutis, `llm`'s model info |
| `wake` | bool | yes | yes (the stand-in's `steer`/`followup`) | yes | `session` wake |
| `exec` | `"off"` / `"gate"` / `"free"` | yes | — | — | `pi.exec` |
| `env` | names, or prefixes ending in `_*` (step 5; a credential or code loader only by its full name) | yes (every pi plugin or none) | yes (every DSH plugin or none) | yes (every plugin of its package or none) | those variables left in its runtime |

A key an ecosystem does not take is refused with its place; it goes in
`src/manifest/tests.rs` and `skill/yobuild/reference/manifest.md` (whose
test names every key). Existing manifests keep working: nothing is granted,
and only `pi.exec` changes (refused where it ran — said, below).

**Wake** is prompt injection and unattended spend, so it is bounded: the text
reaches the agent tagged as the plugin's (`[pi extension X] …`, `[DSH
plugin] …` — the DSH host cannot say which), never as the owner's; mid-run it steers; with the agent idle it
starts a run only up to a cap per session (then refused, said), counted by
the budgets; under `--once` it starts none (refused, or dropped and said); a run
it starts is a run like any other for the exit status of web and terminal
modes ("the last run ended …").

### Fixed answers, said

The adapters record each fixed answer they give in a grant's place (what
was asked, what was answered, which grant would change it) and report it
through `yoagent.log` with a fixed tag. `run` turns each into a Warning
notice once a session ("a pi extension asked whether the project is
trusted: answered no — grant `trust`"; how, against 0.7.1's log, in steps
2 and 3 below); `probe::pi`/`probe::dsh` return those seen
while loading, and `check` lists them per entry, or says "not exercised by
the probe" for those asked only in a tool call. A `hints` row and its
`failures.md` entry point at the grant. (As built for `run`: no `hints`
row, `failures.md` carries them. For `check`, superseded by "Fixed
answers in `check`", below: the binary's own watcher rather than the
probe's return, and nothing said for a tool's own questions.)

### What a grant hands a plugin

- **Honour-system** — `trust`, network, installs, and also what goes around
  `exec` and `llm`: a plugin already has Node's whole power; it can spawn
  through `child_process`, or read a key where the owner keeps it (the
  runtimes start with the kept environment, every credential and other
  variable left out unless granted: steps 4 and 5, below; §6). A grant changes how honest code behaves (pi-lens installs
  language servers into `~/.pi-lens` and runs the project's code); nothing
  enforces it. Said by `check` and in §6.
- **Mediated** — calls through the host: `pi.exec` judged by the gate,
  `llm` charged to the budget, wake bounded, approvals naming the plugin.
  Metering holds for honest code since step 4 took the credentials yoagent
  reads out of plugin runtimes and stdio servers (below).
- **Never granted**: provider routing and payload rewriting, context
  rewriting, DSH's `loader` (plugins loading plugins past the lock),
  plugins' own model adapters reaching yoagent, DSH's app plugins
  (agent-teams, dsh-im, dsh-pet, model-proxy: yoagent is the loop).

### Probed on 2026-10-19

A plain Node Cordis root — the DSH base at 0.2.0-rc.2 plus `dsh-agent`,
`dsh-scope`, `dsh-session`, `dsh-user-approval`,
`dsh-session-persistence-jsonl` and `dsh-univer-office@0.3.7`; not yet
inside rutis, which the first implementation PR repeats (rutis's row-level
`inject` and wait rules, §4, apply there). With the stand-in registered as a
root, univer created a file, a worktree, imported a CSV, inspected it,
exported a valid `.xlsx`, and merged — its `ask` reaching DSH's own
`approval/request`, answered `allowed-once` (`rejected` refused it);
`ask_user_question` with the agent passed `assertLiveRoot`. Without the
agent, its file tools failed as in yobuild. None of DSH's app was needed
(no typert, invariants, app boot); univer's parts that inject what was not
there — its web viewer (`webServer`, `sessions`, `connection`), its skills,
its settings — wait, as read in its source (no warning was logged).
Learned there: the stand-in's `id` is its session's id; approvals fail
outside a turn ("approval.request() outside an open turn"); `sessions.create`
persists nothing; the two `sessionPersistence` providers collide;
`schemas(agent)` equals `schemas()`; univer's native bindings
(`@univerjs-pro/*-binding`) have no install scripts — but
`dsh-session-persistence-jsonl` depends on `koffi`, which has one, so the
base's `check` list grows; its Gateway (a child on 127.0.0.1) starts lazily
on the first call that needs it and exits with the plugin; with
`telemetry = false` it still writes a state file under `DSH_HOME`. The probe
script becomes a test in yoagent-rutis with the first implementation PR, so
this can be re-run on each DSH bump (0.2.1-rc.2 is out, with an 825-line
change to `dsh-tools`' types).

### Steps 2 and 3 against yoagent-rutis 0.7.1 (proposed 2026-10-20)

yoagent-rutis 0.7.1 (with yoagent 0.25.11) ships the DSH host and part of
the pi host; the pi adapter's factory wrapper (`loadGated`) and
`projectTrusted` were in 0.7.0, and new in 0.7.1 are `execFor`, `model`,
`models` and the observer events. Read against its sources: what each part
gives, what yobuild does with it, and what is still missing before a grant
can be honoured.

**One rule for both ecosystems** (decided 2026-10-20): a grant a shim can
hold per plugin is per plugin — pi's `exec`, `trust` and `wake`, by
extension file. A grant it can give only to the whole runtime — every DSH
grant (one stand-in, and with the host loaded every DSH call goes through
it), pi's `model` (one row config) — is written on **every** plugin of that
runtime or on none: the manifest then says exactly what each plugin gets,
and nothing is granted that is not written on it. A manifest that writes it
on some is refused at the first entry that differs: "every DSH plugin
shares the DSH host: give each the same grants (`a` has workspace, `b`
none)" / "every pi extension sees one model: grant `model = \"info\"` to
each pi plugin or none". Should a shim later tell plugins apart, that grant
becomes per plugin and the rule stops applying to it.

**Callers become kinds.** `Host::caller` returns what the calling row is —
`Rutis { name, grants }`, `DshHost { grants }` (step 2), `Pi { plugin,
grants }` (step 3) — and each method lists the kinds it serves, instead of
"any caller with grants": `host_approval.ask` serves `Rutis` and `Pi`;
`trusted` and `model` serve `Rutis`; `cwd` and `wake` serve all three, each
under its grant. A refusal names the kind ("not for the DSH host").

#### Step 2, the DSH host

What 0.7.1 ships: `plugins/dsh/dsh-host.ts` (row `dsh-host`), its rows in
`dsh-host.json` (`dsh-session`, `dsh-agent`, `dsh-user-approval`, the jsonl
backend under the id `dsh-session-persistence`, `dsh-host`), and
`adapters::dsh_host_rows(dsh, sessions)`, which merges them into
`host-services.json` — the jsonl backend replacing the storage-less row of
that id (rutis-loader keeps the first row of an id, so the files are never
appended by hand). The shim calls `host_session.info()` and
`host_workspace.cwd()` to make one stand-in per generation, each host call
bounded at 30 s and a call's whole wait for its stand-in at 120 s; a
stand-in it cannot make (relative cwd, malformed info, a refused or slow
call) fails the tool call with "the DSH host has no agent for this call:
…". With the host loaded, **every** DSH tool call asks it for the stand-in
(`dsh-tools-adapter.ts`) — granted plugin or not, which the one rule makes
moot. A DSH plugin's `steer`, `followup`, waking `send` and `inject` go to
`host_session.wake(text)`, fire-and-forget: a refusal or timeout is a warn
line in the log, and the plugin is not told; the text is the message's
text blocks joined with newlines. Approvals reach `ui` through
`host-dialogs.ts` as a select: Allow once → `allowed-once`, Deny →
`rejected`, no answer or a host failure → `unavailable`, withdrawn by the
call's signal → `cancelled`; with no frontend attached it steps aside and
DSH's own answer stands.

yobuild's part:

- **Grants**: a DSH plugin takes `workspace` and `wake`, `wake` only with
  `workspace` (the stand-in reads the folder first — refused at `wake`: "a
  DSH plugin's `wake` needs `workspace` (the stand-in agent)"); `trust`,
  `model`, `exec` stay refused; each DSH plugin lists the same grants (the
  one rule). `check` lists them as today.
- **Which rows load**: with the grants, the `dsh` runtime's rows are
  `dsh_host_rows(plan.dsh_dir, <DshHome>/sessions)` when `[dsh]
  host_services = true`; with `false`, only the rows `dsh-host.json` names
  (as `dsh_host_rows` gives them, the jsonl one with its root) — an
  explicit `false` never brings DSH's file, credential or storage services
  in through a grant. Whether `dsh-session`/`dsh-agent` need a row of
  `host-services.json` to apply is checked in the code PR; if one does, it
  is named here first. The effective setting (host services loaded or not)
  is what `dsh_services`, `dsh_waiting` and `dsh_missing` get, so their
  "`[dsh] host_services = true`" hint is never given when they loaded. A
  `DshHome` is made whenever either is on; with `YOAGENT_DSH_HOME` set the
  session log is written in the user's own DSH home, said by `check` as an
  agent note.
- **Where the files are**: `dsh-host.ts` imports `@deepseek-ai/dsh-scope`,
  which must resolve the runtime's Cordis, so it sits at the root of the
  runtime's folder as the adapter does. `DSH_FILES` gains `dsh-host.ts`,
  `dsh-host.json` and `host-services.json` (what `dsh_host_rows` reads from
  that folder), so the `dsh` set's hash covers them — every existing `dsh`
  set is installed once more — and the plain `[dsh] host_services` path
  (`plugins.rs`, `check.rs`) reads `host-services.json` from
  `plan.dsh_dir` too, one copy instead of two that could differ. Without
  pins, `adapters/dsh` already holds them.
- **That it started**: `dsh-host.ts` injects `agents`, `sessions`,
  `sessionPersistence`, `host_session`, `host_workspace` and `yoagent`; a
  row waiting for one reads `Active` to the host while DSH's loader holds
  it, and the adapter, finding no host, would then run every call
  agentless — the plugins' grants silently not honoured. So `start` adds
  `dsh-host.ts` and the host rows' modules to the DSH services probe
  (`probe-services.ts`, its `names` with `yoagent-dsh-host`) and refuses
  "the DSH host did not start: <what it waits for>" — a `hints` row and
  its `failures.md` entry. `check` together gets it through `run::start`.
- **`check` alone**: `probe::dsh` builds its own base, without the host,
  so a granted DSH plugin probed alone gets the note "probed without the
  DSH host: its grants are not exercised alone"; together is where the
  host is checked (above). A tool call is made by neither.
- **Who is granted**: `DshHost` is the exact row id `dsh-host`, granted the
  DSH plugins' (shared) grants; the DSH plugins' own `plugin:<name>…` rows
  and the `dsh` row stay refused. Any DSH plugin can still reach the
  `dsh-host` row — the stand-in's `ctx` is a scope over the shim's Cordis
  context, handed to every plugin as `exec.agent.ctx` — so the bound is
  that **no row gets more than the DSH plugins' shared grants**; a handler
  registered through it is offered by `dsh-host`, which no entry's `allow`
  accepts (it asks).
- **Wake**: from `dsh-host`, its line breaks turned into spaces (the shim
  joins blocks with newlines; the one-line rule is there so nothing after
  a break passes for the owner's words, and a space keeps that), tagged
  `[DSH plugin] <text>` (the shim cannot say which plugin), under the
  session's rules and its one allowance. Since the shim only logs a
  refusal, yobuild says it: a wake refused from a `DshHost` or `Pi` caller
  is a Warning notice, once per reason a session ("a DSH plugin's wake was
  refused: <why>").
- **The bump**: yoagent 0.25.11 (a tool result's text blocks share one
  `tool_output_max_lines` budget: `run_test`'s "a tool result's text blocks
  all kept" is rechecked against it) and yoagent-rutis 0.7.1, whose DSH
  base gains the five packages, so the examples' and gallery's locks are
  regenerated in that PR; `koffi`'s install script is said by `check` (the
  base's list) and is not run (`--ignore-scripts`): the code PR checks the
  jsonl backend works without it, else says so here first.
- **Done when**: `run_test` — a DSH fixture plugin reading
  `exec.agent.session.header.cwd` (the agent's folder) and waking with two
  lines (reaching the agent as one `[DSH plugin] …` line, which also shows
  the call arrives as `dsh-host`), a refused idle wake said as a Warning,
  differing grants refused, the host not starting refused at start; `check`'s
  alone note; and a gallery agent built through the skill with
  `dsh-univer-office@0.3.7` (`parts.md` moves it from "not usable" to
  **CI**), its scripted replies in CI and one live run, asked for first.

Step 2 needs nothing more from yoagent-rutis.

**Built (M3b step 2, 2026-10-20)**, with yoagent 0.25.11 and
yoagent-rutis 0.7.1, as above. Checked on the way: `koffi` loads without
its install script (`--ignore-scripts` stays); the host's rows need
nothing from `host-services.json` (`dsh-session`, `dsh-agent` and
`dsh-user-approval` inject `invariants` only in companion plugins of their
own, which wait without holding the row). `run_test` covers a granted
plugin's stand-in, its two-line steer, a refused wake said, and no agent
without grants, and `check` together; the host not starting is
unit-tested (`dsh_host_refusal`, its hint on that text), not caused end to
end. The gallery agent with `dsh-univer-office` is the next PR.

Found in review: rutis 0.12's catalog shares every service name across
runtimes, so a rutis plugin could inject `yoagent-dsh-host` and reach the
stand-in. The name is now **runtime-local** (`PluginHostBuilder::local`:
registered for a Rust type nobody provides, so never shared; the DSH
adapter still finds it in its own runtime's Cordis) — a plugin of another
runtime injecting it never starts. The session log's row is found by its
package, as yoagent-rutis finds it, and given its folder at start or the
run is refused: an empty folder is never written.

#### Step 3, pi grants

The pi adapter's row in 0.7.1:

| Config | What it does | Per extension |
|---|---|---|
| `exec` / `execFor` | `pi.exec`: `false` (default), `true`, or commands matched by name; a refusal is code 126, reported at warn once per extension and command | yes (`execFor`, keyed as `extensions` spells the file, resolved against `cwd`) |
| `projectTrusted` | `ctx.isProjectTrusted()` | no |
| `model` / `models` | `ctx.model`, a read-only `ctx.modelRegistry` | no |
| (none) | observer events fire for every extension (the types subscribed from the handlers at load) | no |
| (none) | `sendMessage` recorded only; `sendUserMessage` throws | — |

The factory wrapper overrides only `exec`; `sendMessage` and
`sendUserMessage` are set on the shared runtime, so a per-extension wake
needs them wrapped too. Per grant:

- **`exec`**: `"off"` → `execFor[file] = false`, `"free"` → `true`, the key
  spelled as yobuild passes the file in `extensions` (not canonicalized:
  another spelling refuses the load). `"gate"` has no form: the adapter
  matches command names, it does not judge words. Asked of yoagent-rutis:
  `execFor[file] = "gate"` — each call's `[command, ...args]` and
  `options.cwd` through `host_approval.judge`; not free →
  `host_approval.ask` naming the extension, run on `"allow"` only.
- **`trust`**: `projectTrusted` is one answer for every extension, and
  `trust` lets a plugin run project code. Asked of yoagent-rutis:
  `projectTrustedFor`, by file, as `execFor`.
- **`model = "info"`**: one `model` for the row, so runtime-wide: on every
  pi plugin or none (the one rule). yobuild sets the row's `model` (from
  `host_session.model()`'s figures) when they all grant it. No adapter
  change.
- **`wake`**: not mapped. Asked of yoagent-rutis: per extension
  (`wakeFor`), `sendUserMessage(text)` and a `sendMessage` with
  `triggerTurn` or `deliverAs` → `host_session.wake(text, file)`, both
  wrapped per extension; refused as today without the grant.
- **`observe`**: 0.7.1 fires observers for every extension, and cannot be
  made to refuse one. What they carry (the conversation, tool arguments and
  results) every extension already reads through `message_end`, `context`
  and `tool_result`, and sending it anywhere is Node's own power
  (honour-system, above). **Dropped** (decided 2026-10-20): `observe` is no
  grant (refused as an unknown key; `RawGrants`, `manifest.md` and
  `skill_test` lose it); `check` notes a pi entry that registers observer
  events, and §6 says every pi extension sees the conversation.

On yobuild's side the `pi` row becomes the `Pi` caller for `wake` and
`ask`, naming the calling extension by a **last** argument —
`wake(text, file)`, `ask(question, details | null, file)` (`ask`'s second
is `details`) — required from the `pi` row and refused from any other.
yobuild canonicalizes it and maps it (`Plan::pi_files`, canonical) to its
plugin, then checks that plugin's grants; a file no plugin loads is
refused. Enforced in yobuild, trusting the adapter only to name the caller
(per-extension trust does not hold against a hostile extension in the same
runtime, §2). Tagged `[pi extension <plugin>] <text>`, a refused wake said
as for DSH.

So step 3 is a pair again: yoagent-rutis (`execFor` `"gate"`,
`projectTrustedFor`, `wakeFor` with `sendMessage`/`sendUserMessage`
wrapped, the file passed to the host), then yobuild (the grants mapped
onto the row's config, the `Pi` caller, `observe` refused, the one rule,
fixed answers said).

**Built (M3b step 3, 2026-10-20)** with yoagent-rutis 0.8.0
(`execFor` `"gate"`, `projectTrustedFor`, `wakeFor`, and the rows
`host-approval.ts` / `host-session.ts` re-providing the two services in the
pi runtime, loaded only when a grant uses them). Found on the way: the
adapter's calls arrive as those re-providing rows', not the `pi` row's —
`wake` from `pi-host-session`, `ask` from `pi-host-approval` — so the `Pi`
caller is taken from exactly those, each for its own service. `ask` from
pi is served only for an extension with `exec = "gate"`. Not done: `check`
noting a pi entry that registers observer events — the probe does not
report an extension's handlers; that needs the adapter to say them.

Found in review: the re-providing rows' names (`pi-host-session`,
`pi-host-approval`) were shared to every runtime by rutis 0.12's catalog,
so a rutis plugin could inject `pi-host-session` and wake the agent as a
granted pi extension; they are runtime-local now, as `yoagent-dsh-host`
(step 2). An extension file is matched as planned or canonical, and one
file is one plugin's (two would have the adapter and yobuild apply
different grants).

#### Fixed answers, said (both steps)

As planned (0.7.1): pi logged `[pi] fixed answer: …` at **info**, naming no
extension; `dsh-host.ts` logs `[dsh-host] …` at warn, info or debug.
`yoagent.log` reaches yobuild only as `tracing` events (target
`yoagent_rutis::plugin`), whose default filter (`warn`) drops info. So
yobuild adds a `tracing` layer of its own on that target that sees info
whatever `RUST_LOG` says (`run::plugin_log`).

**Built** with step 3, on yoagent-rutis 0.8.0, whose pi lines name the
extension's file (`[pi] fixed answer (<file>): <question> → <answer>`): a
line counts only from the row that writes it — `pi` for `[pi] …`,
`dsh-host` for its warnings, read from the call it is logged in, since any
plugin may call `yoagent.log` and a forged line would be yobuild's advice
to grant more. Each becomes a Warning notice once a session, naming the
extension and, where one would change it, the grant: "pi extension lens
asked ctx.isProjectTrusted(): answered false (grant `trust = true` to
change it)"; a `pi.exec` refused for want of `exec`, or by the gate's other
refusals (not the owner's Block); a pi wake the host never got; the DSH
host's warnings. `check`: "Fixed answers in `check`", below (built
2026-10-23).

#### Fixed answers in `check` (built 2026-10-23)

**Why.** `check` names every grant an entry has, but not what a plugin
was answered for want of one. pi-lens without `trust` loads, lists its
tools and passes `check`, then every diagnostic says "unavailable". It
reads `ctx.isProjectTrusted()` at every `session_start` (its CHANGELOG,
4.4.3: "pi-lens now reads `ctx.isProjectTrusted()` at every
`session_start`"), and when told no it installs and spawns nothing. Today
the owner learns why only from `run`'s Warning, after spending a run. The
roadmap's M3b asks that `check` name every fixed answer.

**What there is to see.** `check` already starts the pi adapter in two
places. `run`'s watcher, `run::plugin_log`, would read both (verified
against yoagent-rutis 0.8.1 and the adapter):
- **Alone.** `probe::pi_with` loads the plugin's extension files (as
  planned: `Plan::pi_files`) in a row whose id is `pi`, as `run`'s is.
  - The probe's root lives in `check`'s process, so its lines reach the
    binary's `tracing` subscriber. `check` passes the probe no `Logs`
    collector, so nothing competes.
  - The adapter fires `session_start` as it starts.
  - The probe then calls one run's hooks (`start_run`, `tools`,
    `before_model`, `finish`), so `before_agent_start` handlers run too.
  - On unloading it fires `session_shutdown`.
  - It may restart the adapter (up to 6 times, on a tool name refused),
    which repeats the same lines.
  - **The probe gives the row no grants.** Its config is `extensions`,
    `cwd` and `withoutBuiltins` only: no `projectTrustedFor`, `execFor`,
    `wakeFor` or `model`. Alone, a plugin granted `trust = true` is still
    answered `isProjectTrusted() → false`, and its `pi.exec` is refused
    "not allowed by the host".
- **Together.** Only when no entry has a problem. `run::start` loads the
  `pi` row with the manifest's grants (as `run` does). `offered` then
  calls one run's hooks, `before_agent_start` among them, and `shut`
  unloads it, which fires `session_shutdown`.

So a fixed answer given as an extension loads, or in `session_start`,
`before_agent_start` or `session_shutdown`, reaches `check` as a
`tracing` event on the same target, from the same row, naming the same
file. A question asked only in a tool call or in `tool_call`,
`tool_result` or `input` is not asked: `check` calls no tool and sends no
prompt.

**How.**
- **Watching.** `check` holds a `plugin_log::watch` from before the first
  probe until after the agent together has shut down, so a
  `session_shutdown` line still counts.
- **Collecting.** The sink runs on the runtimes' threads. It pushes each
  `Said::note` into a shared list (a `Mutex<Vec<…>>`). Once everything
  has shut down, `check` takes the list and appends each note to its
  entry's `notes`, or to the agent's, after the notes `check` already
  makes, in the order seen.
- **Who it was.** `Said::note` returns the plugin's name beside the text,
  as `(Option<String>, String)`:
  - a pi line naming a file a pi plugin loads gives that plugin (`who`
    already matches the file as planned or canonical);
  - a line with no file, a file no pi plugin loads, or the DSH host's
    warnings gives none, and the note goes on the agent. The DSH host
    starts only together and serves every DSH plugin.
- **Read with the grants.** `Said` learns each pi plugin's grants
  (`Said::checking`, from the pi plugins' `grants`, matched by the plugin
  `who` finds for the line's file; `run` keeps `Said::new`, whose session
  runs granted). In
  `check` it drops what the probe alone said for want of a grant the
  plugin has:
  - the `isProjectTrusted() → false` note when its plugin has `trust`;
  - the "not allowed by the host" `pi.exec` refusal when it has `exec`
    other than `"off"`;
  - the model notes when the pi plugins have `model`, as now.

  Together, a granted plugin is answered as granted, and then gives no
  line or another one. Lines are deduplicated on the whole note, as now,
  so the probe's repeats and alone-then-together say each once.
- **What `check` leaves out.** It drops what only its own setting makes:
  - the gate's refusals of a granted `pi.exec` ("unanswered", the judge
    or the ask failing): nobody is there to answer;
  - a pi wake: yobuild refuses every wake before the owner's first task,
    as a plugin loads (`host_session.wake`, already filtered), and
    alone there is no host. A wake reaches no one in `check`.

  `check` keeps the fixed answers a grant would change (`trust`,
  `model`) and a `pi.exec` refused for want of `exec`. The wording is
  `run`'s, for example "pi extension lens asked
  ctx.isProjectTrusted(): answered false (grant `trust = true` to change
  it)", and "pi extension lens ran pi.exec("git"): refused (grant
  `exec = "gate"` or `"free"` to allow it)".
- **A note, not a problem.** The plugin works as answered, and `ok` is
  unchanged.
- **What stays the same:** the report's shape (version 3: notes are
  text), the probes, and yoagent-rutis.

**Why the layer.** `plugin_log::layer` reads each line's row
(`caller_row`): any plugin may call `yoagent.log`, and a forged line
would be yobuild advising a grant. yoagent-rutis's own collector
(`probe::Logs`) keeps level and text but no row, so `check` cannot use
it.

**Limits, said.**
- The layer is the binary's. `check()` called as a library says these
  notes only if its caller installs the layer, so the tests use
  `cli_test`.
- A question asked only in a tool call, or in a hook `check` does not
  call, goes unsaid by `check`; `run` says it. `failures.md` says so beside
  the note. This replaces the earlier plan's "not exercised by the probe"
  on each entry ("Fixed answers, said", above): the adapter cannot tell
  which questions a tool would ask, so that note would land on every pi
  plugin and tell the owner nothing.
- When an entry has a problem, `together` is skipped and only the probes'
  lines exist.

**Said where.**
- `check`'s notes (text `  note: …`, JSON `notes`).
- `failures.md`: its two Warning rows (a fixed answer and a refused
  `pi.exec`) gain "or a `check` note on the plugin", with what to do (ask
  the person, then grant) and the limit. No new row, and no hint: `hints`
  matches failures, and these are notes.
- `CLAUDE.md`.

**Tests.**
- `cli_test`, each `check --json`:
  - an extension asking `ctx.isProjectTrusted()` in `session_start` and
    running `pi.exec("git", ["status"])` there gets both notes on its
    entry, each once (the probe and together seen alike), and `ok` stays
    true;
  - the same extension granted `trust = true` and `exec = "gate"` gets
    neither. The probe alone's lines are dropped by the grants; together,
    `git status` is freed by the gate, which runs it, and trust answers
    true;
  - with `exec = "gate"` and `rm -rf x` in place of `git status`: no
    note (the gate asks, nobody answers: left out);
  - an extension asking only in a tool gets no note.
- Unit tests:
  - `Said::note` naming the plugin for a planned and a canonical file,
    and none for no file or an unknown file;
  - the grant filters;
  - `check`'s leaving out of the gate's refusals and wakes.

**Built** (2026-10-23) as proposed, decisions 1–3 as recommended:
`plugin_log::Said::checking(pi_files, model, grants)` (`PiGrants {
plugin, trust, exec }`), `Said::note` → `(Option<plugin>, text)`
(`plugin_log::Note`); `check` holds the watch from before the probes to
after `together`, collecting into a list it attaches at the end. `Said`
keeps each file as given and canonical. After review: in `check` only the
gate's refusals its setting makes (unanswered, the judge or the ask
failing) are left out; a bad `cwd` and a missing approval service are said
as in `run`. Tests: `plugin_log`'s `check_reads_lines_with_each_plugins_grants`
and `a_file_is_its_plugins_planned_or_canonical`, `cli_test`
`check_says_what_a_pi_extension_was_answered_for_want_of_a_grant` (no note
on the agent; `ctx.model` and `session_shutdown`; granted; a gated `rm`;
an entry with a problem, together skipped).

**Decisions for review.**
1. *A note, not a problem* (recommended: the plugin loads and works,
   answered as it would be without a host), or a problem (`ok` false
   until granted or the plugin removed). A problem would refuse every
   honest manifest that leaves `trust` off on purpose.
2. *Probe-alone lines filtered by the plugin's grants in yobuild*
   (recommended: nothing changes in yoagent-rutis), or yoagent-rutis's
   `ProbeOptions` taking the pi grants. That is closer to `run`, but needs
   a release there and the yoagent session's agreement.
3. *The DSH host's warnings as agent notes* (recommended: one host serves
   every DSH plugin), or on each DSH plugin.

### Step 4: plugins without the provider keys (built 2026-10-21)

**Why.** Every process yobuild starts inherits its environment, and with it
every model key the owner has exported: `DEEPSEEK_API_KEY` for the agent,
and `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, … left over from other work. A
plugin that finds one can call that provider itself. The call is outside
the run: no budget stops it, `Cost:` and facts do not count it, and the
owner sees nothing. Honest code does this too: an SDK that reads
`OPENAI_API_KEY` when it is there, a search plugin that summarises with
Gemini when `GEMINI_API_KEY` is set. M3c's metered `llm` is a plugin's way
to a model; this step closes the unmetered way for honest code.

**What it is not: a sandbox.** A plugin runs as the owner. It can read a
key where the owner keeps it (a shell profile, a key file, pi's
`~/.pi/agent/auth.json`, `~/.aws/credentials`), use `AWS_PROFILE` (not a
key: kept; step 5 proposes leaving it out), reach a cloud machine's
metadata endpoint, and on Linux read `/proc/<yobuild's pid>/environ`.
And the list holds only what yoagent knows: `GITHUB_TOKEN`, `NPM_TOKEN`
and the like still pass. It is the honour-system kind of rule ("What a
grant hands a plugin"): taking the variables away stops a library from
picking a key up by default, nothing more. §6 and the skill say so in
these words.

**Which variables** (`KEY_VARIABLES` in `manifest.rs`, beside `PROVIDERS`):
- yoagent's `provider::api_key_variables()` as it is (yoagent 0.25.12: 26
  names) — every provider's, not only the manifest's model's: the
  `PROVIDERS` variables, OpenRouter's, Cerebras's, OpenCode's, Azure's,
  Bedrock's four (`AWS_BEARER_TOKEN_BEDROCK`, `AWS_ACCESS_KEY_ID`,
  `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`), TypeSafe's and
  Cloudflare's two (`CLOUDFLARE_API_TOKEN`, `CLOUDFLARE_AUTH_TOKEN`), and
  the fallbacks `YOAGENT_API_KEY` and `API_KEY`;
- the manifest's `api_key_env`, when it names one.

AWS's and Cloudflare's are an account's credentials, not only a model's:
AWS's open the whole account (S3, IAM, machines, billing), and for a
Bedrock user they are the model's credentials, so leaving them would undo
this step for exactly that user. A server or plugin that needs them is
handed them on purpose (below). They are called *credentials yoagent
reads* in what yobuild says, not "model keys". A unit test checks that
`KEY_VARIABLES` equals yoagent's list, so a provider yoagent adds is
withheld from the release that picks it up.

**Which processes.** Each third-party process yobuild starts:
- the plugin runtimes (`ui`, `pi`, `dsh`, `rutis-<n>`), in `run`, and in
  `check` (alone and together);
- the stdio MCP servers;
- the adapters' helper scripts (`inspect-package.mjs` imports a DSH
  bundle's code).

Not npm (`--ignore-scripts`: no package code runs). Not the agent's own
`bash` either: its commands are the agent's, judged by the gate, and a
project's build or test may need a key. §6's "the environment they
inherit is visible to them", said of plugins, changes with this step to
"the credentials yoagent reads are taken out of it".

**How: removed.** rutis's `Launcher` and yoagent's stdio MCP transport can
only add variables, so yobuild puts `env` in front:
- *Runtimes.* `PluginHost` takes the launcher `Launcher::node` builds and
  makes it `/usr/bin/env -u NAME … <its program> <its arguments>` (the
  fields are public), so nothing of rutis's command is copied. `env` execs
  the runtime: one process, the same pid and process group, so shutdown,
  the terminal UI's group and `--measure`'s `ps` (rutis appends the
  channel and the anchor `package.json` after the launcher's arguments)
  see what they see today. rutis's channel (an inherited descriptor, its
  `RUTIS_…` variables) is not on the list, so it passes.
- *MCP servers.* The same prefix before a stdio server's command (yoagent's
  transport takes a command and arguments).
- *Helper scripts.* yobuild builds their `Command` itself: `env_remove`, on
  every platform — every listed credential, a `keys` grant or not, though
  `inspect-package.mjs` imports a DSH package's code: a plugin that reads
  its granted key as it is imported sees it unset there, and set in `run`.

`env -u NAME` removes `NAME` whoever set it — also a variable set on the
command, as a server's `env` is. So the `-u` list is made per process: the
listed variables set in yobuild's environment, minus the names that
process is handed (its runtime's `keys` grant; a server's `env` names; a
runtime's `env` entries, as `DSH_HOME`). A name not in yobuild's
environment is not listed (there is nothing to remove).

`PluginHost::builder()` gets `withhold(name)` (every runtime) and
`keep(runtime, name)` (that runtime's exception — a `keys` grant; an
`env(runtime, name, …)` entry keeps its name too). `keep` for a runtime
not started is an error, as `env` is.

Two places set the variables empty instead (an empty key reads as unset
to SDKs, or fails authentication; they do not fall back to another
variable, since each on the list is withheld; only code that tests
whether the variable exists, rather than its value, sees a difference):
`check`'s pi and DSH probes alone, whose launcher yoagent-rutis builds
(`probe::pi_with` / `probe::dsh_with` with `ProbeOptions::env`, yoagent-rutis
0.8.1, only add); and the runtimes and MCP servers on Windows, which has
no `env` (the notes say "set empty" there, not "taken out": a plugin may
read an empty key differently from none, and nothing tests that path, CI
being Linux and macOS). `check`'s rutis plugin alone (`rutis_alone`) goes through
`PluginHost`, so removed.

Checked with a prototype on 2026-10-21: the runtimes see the variables
unset in `run`, in `check` together and in `rutis_alone`; an MCP server too,
but for the one its `env` hands over; `tests/run_terminal_pty.py` and
`tests/terminal_pty.py` pass with keys set (the terminal UI through `env`,
Ctrl+C, a killed UI, pi's question answered from the keyboard).

**Giving one back, on purpose.** A manifest can still pass a credential,
and then `check` notes it on the entry, one wording everywhere: "it is
given `X`, a credential yoagent reads: what it spends with it is not
metered".
- An MCP server's `env` naming a variable on the list
  (`env = { OPENAI_API_KEY = "${OPENAI_API_KEY}" }`), or a reference to one
  in its arguments, URL or headers, hands its value to that server.
- A DSH or rutis plugin's `config` takes `${NAME}` references (the `guard`
  rule), filled at start; a reference to a variable on the list.
- A `keys` grant on a `[[plugin]]`, `grants = { keys = ["AWS_ACCESS_KEY_ID",
  …] }`: those variables are left in the plugin's runtime as yobuild has
  them. Names only from the list, each once; another is refused, "`X` is
  not a credential yoagent reads: nothing withholds it". A runtime's
  environment is the whole runtime's, so the "one rule for both
  ecosystems": the same `keys` on every pi plugin or none, on every DSH
  plugin or none, and on every rutis plugin of one package (one runtime
  per package) or none — else refused at the first that differs, as
  `model` and the DSH grants are. Named `keys`, not `env`: a server's `env`
  sets values, this lets names through. It is the way a pi extension (no
  config) gets a key.

`keys` is the launcher's alone. The DSH host starts for `workspace` and
`wake` only (`Plan::dsh_host`: the DSH grants other than `keys`), so a
`keys`-only DSH manifest starts no DSH host and gets no "probed without
the DSH host" note; the DSH refusal becomes "a DSH plugin takes
`workspace`, `wake` and `keys`, not `x`". The alone probes leave a granted
name alone (they set empty only what is withheld from that plugin).

**Said.** §6 ("Not covered") and the honour-system bullet say the
credentials are removed, and that this is not a sandbox. The skill's
`manifest.md` says how to pass one (a server's `env`, `keys`). Which
listed plugins lose a feature without one is for `parts.md` once one is
found to. When a plugin or stdio server would run, the withheld ones are
named — by `check` in an agent note (adding that its pi and DSH probes
alone get them empty), by `run` in an Info notice as the session starts —
since a plugin that fails for want of a key says so in its own words, and
no hint can match them.

**Where `env` falls short** (unix; found in review). `env` reads a word
holding `=` before the program as an assignment, so a program whose path
holds one runs through `/bin/sh -c 'exec "$0" "$@"'` (still one process).
With no `/usr/bin/env` (or `/bin/sh`, for a program whose path holds
`=`), starting a runtime or server that has a credential taken out is
refused rather than the variables left in, saying that yobuild needs it
(hint `no-env`). Whenever a credential yoagent reads is set in yobuild's
environment (usually the agent's own key), a runtime or server losing it
— the terminal UI's always — does not start on an image without it:
install it. On macOS with System Integrity Protection on (the default;
GitHub's macOS runners have it off) `/usr/bin/env` is protected, and
starting it drops `DYLD_*` variables, so they do not reach what starts
through it either — only while a credential is taken out of that
runtime's or server's environment; not worked around, but said: `run`
(an Info notice) and `check` (an agent note) name the `DYLD_*` variables
it will not get (`run::dyld_note`: this process's, and a stdio server's
own `env` entries that go through `env`), when something does start
through `env` — a plugin whose `keys` do not keep every one, a stdio
server whose `env` does not set every one, or the terminal UI, which
keeps none (`Platform::terminal`: `run` without `--once` on a terminal
manifest; `check` says what such a `run` would do).

**Found after the merge** (the yoagent session's review of #55, then
two rounds of #56's own review).
- Spawning `env` succeeds whatever the program is, so a server or runtime
  program that is not there or cannot run showed only as `env` exiting
  127 or 126 ("MCP server '/usr/bin/env' exited …", "Cordis process exited
  before connecting …"). `frontend::host::startable` now looks for it
  first, as `execvp` will — a path from the child's folder, a bare name
  (`node`) on the child's `PATH` (the launch's own when it sets one, else
  this process's, else `/bin:/usr/bin`; an empty or relative entry from
  the child's folder) — asking the system whether this user may run it
  (`access(X_OK)`: an ACL counts, another user's 0744 file does not; a
  `noexec` mount is not seen), and refuses with "yobuild cannot start X:
  no such file" / "it is not a file" / "it is not executable" / "it is not
  on PATH" / "it is on PATH but not executable" / "it is not on PATH where
  it could be looked at (…)", or the system's own error (a folder it may
  not search, a symlink loop) — never "no such file" for a file that is
  there; a runtime's prefixed "runtime `pi`: ". Hint `cannot-start`,
  matching "yobuild cannot start " only: rutis says its own spawn failures
  as "cannot start X: …", whose cause may be anything (the terminal not
  there), and a plugin's own words may say a program "cannot be started";
  rutis's one failure whose cause is plain, "cannot start node: No such
  file or directory", gets hint `no-node` (install Node). A runtime's
  program is looked for only when it starts through `env`; a stdio
  server's always (its command is always a path), so a missing one is
  said alike with a key set or not.
- A name `set` here is set and not empty (`run::key_is_set`: one exported
  empty hands over nothing usable). Each name of a `keys` grant that is
  not is said on the entry (`run::keys_unset`, `unset_grant_note`):
  "its `keys` grant names `X`, unset or empty here: it gets no key from
  it" when none of the grant's names is set — by `check`, and by `run` as
  a Warning ("plugin P: …") — else "…: not handed over (the rest of the
  grant is)", by `check` and by `run` as Info (the others may be
  fallbacks). `keys_given` counts only set names: a grant's, a reference's
  (one to an empty variable is filled empty), and a server's `env` name
  only when a reference filling it is set.
- When the entries lose different credentials (a `keys` grant or a
  server's `env` keeps some for one), `check` notes on each plugin and
  stdio server what it has taken out (`run::withheld_from`; "set empty"
  off unix, `TAKEN_ONE`), beside the combined note — a pi or DSH plugin's
  adding that its probe alone gets them empty, and a plugin whose `config`
  refers to one that it still gets it there; alike, the combined note
  alone.
- `Plan::keep`'s rule (one runtime, one `keys`) is an error naming the
  plugins, in the manifest's words, not only a debug assertion, should the
  manifest's checks ever loosen.

**Tests.**
- `cli_test`, the binary with `DEEPSEEK_API_KEY`, `OPENAI_API_KEY`, a
  custom `api_key_env` and an unrelated variable set, scripted: a rutis
  plugin, a pi extension, a DSH fixture file and a stdio MCP fixture, each
  recording which of those names it sees and how (unset, empty, a value).
  In `run` and in `check` together: the listed ones unset, the unrelated
  one kept, the server's `env` key handed over; in `check` alone: pi and
  DSH empty, rutis unset. Then a different `keys` grant on each plugin:
  every runtime keeps only its own. `check --json`: the notes on the
  entries and the agent note; `run`'s Info notice.
- `src/run.rs`: the note names only what some plugin or stdio server
  loses, and nothing when none does (no plugin, an HTTP server only,
  every name granted).
- `src/manifest/tests.rs`: `keys` refused with a name not on the list, a
  repeat, a list that differs between two pi plugins or two DSH plugins
  (each list said); `keys` on a DSH plugin accepted with or without
  `workspace`. Unit: `KEY_VARIABLES`
  equals yoagent's list; `Manifest::key_variables` adds `api_key_env`; the
  per-process `-u` list leaves out what that process is handed.
- Host test: `withhold` removes a variable from every runtime, `keep`
  leaves it in one, `keep` for a runtime not started is an error.
- `tests/run_terminal_pty.py` and `tests/terminal_pty.py` with a key set
  (CI sets one), so the terminal UI is started through `env`.
- `src/run/plugins.rs`: a `keys`-only DSH grant plans no DSH host; two
  rutis plugins of one package refused with different `keys` (two
  packages may differ).
- `src/run.rs`: a server's command in a folder named with `=` runs, each
  name taken out once; a program missing, not a file, not executable, or
  not on the `PATH` its `env` sets refused before `env` runs (and with
  nothing withheld too); the `DYLD_*` note only on macOS and only when
  something starts through `env` (not when every entry keeps what is
  withheld; with the terminal UI and no plugin, but not under `--once`; a
  server's own `DYLD_*` `env` entry named only when that server goes
  through `env`); each entry's losses said only when they differ; a
  `keys` grant's unset names said — "it gets no key from it" when all
  are, else "not handed over"; `keys_given` leaving out unset or empty
  names (a reference, an `env` name filled from an empty one).
  `src/frontend/host.rs`: relative and empty `PATH` entries from the
  child's folder, a later entry that runs winning, a symlink loop and a
  folder this user may not search said in the system's words. Host test:
  `node` not on a runtime's `PATH` refused at `start`, naming the runtime;
  a runtime's `DYLD_*` entry gone through `env` on macOS with SIP on
  (`csrutil status`), arriving where nothing goes through it. `cli_test`
  `a_server_that_cannot_start_is_said_with_what_to_do`: `check`'s problem
  and fix, with a key and without, `run`'s refusal with what to do, the
  `DYLD_*` note on macOS; the credentials test: `run`'s Warning for an
  unset grant, for that plugin only and for none when every grant is set,
  and a key exported empty said as unset. `src/hints.rs`: `cannot-start`
  not given for rutis's own spawn failure, a server's words or a
  plugin's; `no-node` for rutis's missing `node`.
- `src/run/plugins.rs`: `Plan::keep` refuses a runtime whose plugins keep
  different `keys`, naming them.
- CI job `macos`: the unit tests, host_test's
  `a_withheld_variable_is_gone_from_every_runtime_unless_kept_or_set` and
  cli_test's `a_server_that_cannot_start_is_said_with_what_to_do` on
  macOS (BSD `env`). SIP is off on the runners, so the `DYLD_*` drop
  itself is not exercised there — host_test checks it on a machine with
  SIP on (`csrutil status`), and CI that `DYLD_*` arrives otherwise.

**History.** Proposed 2026-10-21 as *blanked* (set empty everywhere, true
removal waiting for rutis's `Launcher::env_remove`), AWS's and Cloudflare's
credentials left out, no key grant for pi; the review decided that the same
day (blank now, `bash` left out, no pi grant until one is wanted). Amended
before the code, asked by the yoagent session's owner and checked with the
prototype: removed through `env -u` (no rutis change), every credential
yoagent lists, the `keys` grant. yoagent-rutis 0.8.1 added
`ProbeOptions::env` for the alone probes, and yoagent 0.25.12
`api_key_variables()`. After the merge (2026-10-21), yoagent's owner chose
to keep `ProbeOptions` setting names empty rather than removing them: the
alone probes get the credentials empty, `check` says so ("its probe alone
gets them empty"), and a plugin that `check` misjudges because of it is
to be sent to yoagent. The stronger step is an allowlist — a runtime
started with a small kept set (step 5 lists it) and what is granted —
which the same `env` gives without a rutis
change (`-u` every name not kept, not `env -i`, which would drop what the
launch sets): its own design, "Step 5" below (proposed 2026-10-22), the
kept set measured against the gallery and the matrix's plugins.

**Built** (2026-10-21): `manifest::KEY_VARIABLES` (26, yoagent 0.25.12's
list), `Manifest::key_variables`, `Grants::keys` / `hosts_nothing`,
`PluginHostBuilder::withhold` / `keep` and `run::without`, both through
`frontend::host::env_without` (`env -u`; `/bin/sh` for a path with `=`;
every launch made before a runtime is registered), `Plan::keys` / `kept` (the rutis same-per-package refusal is
`plan`'s, which knows the packages), `run::withheld`, the MCP launch's
`withheld`, `run::withheld_note` (`run`'s notice, `check`'s note),
`run::keys_given`; after the merge `frontend::host::startable`,
`run::key_is_set`, `run::keys_unset` / `UnsetGrant`,
`run::unset_grant_note`, `run::withheld_from`, `run::dyld_note` /
`Platform`, `run::TAKEN_ONE`, the hints `cannot-start`, `no-node` and
`no-env`, the `macos` CI job; the tests listed above,
`cli_test` `plugins_and_servers_have_the_credentials_taken_out_unless_handed_one`
and host_test
`a_withheld_variable_is_gone_from_every_runtime_unless_kept_or_set`
among them.

### Step 5: plugins with a kept environment (proposed 2026-10-22)

**Why.** Step 4 takes out the credentials yoagent reads. Everything else a
plugin inherits as before: `GITHUB_TOKEN`, `NPM_TOKEN`, a search API's key,
an observability service's secret, a cloud's credentials file path — and
the owner's settings for other programs. The same honest-code argument
holds: an SDK picks up `TAVILY_API_KEY` or `LANGFUSE_SECRET_KEY` when it is
there, and the plugin acts on the owner's accounts without the manifest
saying so. Step 4's History named the stronger step: an allowlist — a
runtime started with a small kept set and what the manifest grants.

**What it is not: a sandbox**, as step 4: a plugin runs as the owner and
can read what the owner's files hold. It stops a plugin from picking up
what the owner exported for something else, nothing more. Files are not
covered: any plugin can read the owner's dotfiles and `.env` files (DSH's
own command-line boot, `dsh-app-boot`, loads a `.env` into `process.env`;
yobuild's DSH runtime does not run it).

**Measured** (2026-10-22, on macOS only, `yobuild check --json` at
96e20a2, a scratch `YOBUILD_HOME`; the method is step 4's question asked
of every variable). Every plugin the compatibility matrix found loading —
pi's 15 (all but the 5 it refused; three of them with nothing to use) and
DSH's three that work partly (`dsh-univer-office`; `dsh-free-search` at
the version the DSH base pins, 0.8.1, through the gallery's `web-research`
and the handoff example — the matrix's 0.9.1 is refused alike in A and B;
`dsh-visualize`, which does not install, as `parts.md` says) — both MCP
servers the gallery uses, the four gallery agents and the three examples,
each checked twice: **A**, the environment as it is (with a dummy model
key and `GITHUB_TOKEN` set), and **B**, `env -i` with only `PATH`, `HOME`,
`TMPDIR`, `TERM`, the locale, `USER`, `LOGNAME`, `SHELL` and the model
key. In B, as in A, each one loads with the same tools, entries, problems
and notes (the agent notes differ only by the credentials step 4 names —
one more was exported in A — and the lock A wrote first): **nothing a
plugin does to load needs more**, on macOS. The matrix's other DSH
plugins (twelve needing a DSH host, four not applicable, one failed) were
not measured, nor were scripted runs (`--once`) in B. Linux was not
measured: what a Linux `node` or native addon needs from the environment
(`LD_LIBRARY_PATH`, below) is the kept set's guess, checked by CI's Linux
job once built.

Loading is not calling: `check` calls no tool. So each installed package's
own files (not its dependencies) were also read for the variables they
name (`process.env.X`, `process.env["X"]`). What the kept set below would
take from them, by kind:
- *Credentials of their own services* — pi-web-access names 36
  (`TAVILY_API_KEY`, `BRAVE_API_KEY`, `EXA_API_KEY`, `FIRECRAWL_API_KEY`,
  `SERPER_API_KEY`, `PERPLEXITY_API_KEY`, `JINA_API_KEY`, …; five of them
  — `OPENAI_API_KEY`, `GEMINI_API_KEY`, `MISTRAL_API_KEY`, `XAI_API_KEY`,
  `ZAI_API_KEY` — step 4 already takes out) and
  `GOOGLE_APPLICATION_CREDENTIALS`; the langfuse observer
  `LANGFUSE_PUBLIC_KEY` / `LANGFUSE_SECRET_KEY` / `LANGFUSE_HOST`; openwiki
  `LANGSMITH_API_KEY` and Azure's `AZURE_CLIENT_ID` / `AZURE_TENANT_ID` /
  `AZURE_FEDERATED_TOKEN_FILE`; plannotator `JINA_API_KEY`; univer
  `UNIVER_LICENSE`. This is what the step is for: each such plugin needs a
  grant to use the owner's account with that service.
- *Their own settings* — `PI_LENS_*` (about 120 tunables) and pi-lens's
  `PILENS_*` (`PILENS_DATA_DIR`, …), `PLANNOTATOR_*`, `PI_SUBAGENT_*`,
  `CONTEXT_MODE_*`, `PONYTAIL_*`, `BIGPOWERS_*`, `OPENWIKI_*`, `PI_MCP_*`,
  `PI_WEB_ACCESS_*`, `POWERLINE_NERD_FONTS`; `dsh-free-search`'s
  `RESEARCH_CACHE_DIR`, `DECISION_MODEL_URL` and `DSH_HOME`. Left out,
  each plugin does what it does when the owner never set it; an owner who
  did set one grants it.
- *Where programs live* — pi-lens finds language servers and scanners
  through `GOPATH`, `GOBIN`, `CARGO_HOME`, `JAVA_HOME`, `JDTLS_PATH`,
  `VIRTUAL_ENV`, `CONDA_PREFIX`, `UV_PROJECT_ENVIRONMENT`, `BUN_INSTALL`
  (context-mode reads `BUN_INSTALL` too). Not secrets; kept below.
- *Other agents' homes, to detect their host* — `CLAUDE_CONFIG_DIR`,
  `CODEX_HOME`, `CURSOR_*`, `VSCODE_*` (context-mode, ponytail,
  plannotator) — read to tell which agent app it runs in; left out, they
  read as they would in a plain pi.
- *The terminal, the network and the session* — `TERM_PROGRAM`
  (powerline-footer), `NO_COLOR` and `CI` (context-mode), the proxies
  (pi-web-access, context-mode), `BROWSER` (pi-mcp-adapter, plannotator:
  to open a sign-in page), `PWD` (plannotator, context-mode): kept below.
- *Node's own* — univer reads `NODE_PATH`, left out on purpose below;
  what it loses without it was not measured.

The scan sees only those two spellings in the package's own files: not a
name read by a dependency (an SDK the plugin imports reads its own key),
not `const { X } = process.env`, not a computed name, and not a name
looked up through another object (DSH's
`launchEnvironmentOf(ctx).get(name)`, which `dsh-credentials-local`
uses). It does see names in dead code.

And the code yobuild itself runs in the runtimes: pi-tui (the terminal
UI) reads the terminal's variables (`TERM`, `COLORTERM`, `TERM_PROGRAM`,
`COLUMNS`, `LINES`, `TMUX`, `STY`, `ZELLIJ`, `WT_SESSION`,
`KITTY_WINDOW_ID`, `ITERM_SESSION_ID`, `WEZTERM_PANE`,
`GHOSTTY_RESOURCES_DIR`, `WARP_*`, `TERMINAL_EMULATOR`, `TERMUX_VERSION`,
`SSH_*`, `DISPLAY`) and its own `PI_HYPERLINKS`, `PI_TRUE_COLOR`,
`PI_IMAGE_PROTOCOL`, `PI_PROGRAM_STATUS`, `PI_TUI_*`; the rutis Node
runtime its one setting `RUTIS_SHUTDOWN_TIMEOUT`, a remote link's
credentials (`RUTIS_TOKEN`, and the TLS files `RUTIS_CA`, `RUTIS_CERT`,
`RUTIS_KEY`, read whenever a channel opens — a stray one pointing at a
missing file fails even a local runtime) and test knobs (`RUTIS_RETRY`,
`RUTIS_HEARTBEAT`); in loopback handover — off unix, or with
`RUTIS_LOCAL_HANDOVER=loopback`, which yobuild's own process reads, as
`RUTIS_TRACE` — rutis sets `RUTIS_CHANNEL_TOKEN` on the command it spawns
(on unix the channel is otherwise file descriptor 3, which the
environment does not touch); tsx `NODE_OPTIONS`, `TSX_*`; the DSH base
`DSH_*` (yobuild sets `DSH_HOME` / `DSH_AGENTS_HOME` through the launcher
when it gives DSH a home; the base also reads `DSH_WEB_SEARCH_PROVIDER`,
`DSH_WEB_FETCH_PROVIDER`, `DSH_BUNDLED_SKILL_DIR`) and Cordis
`CORDIS_SHARED` (state a Cordis loader hands its own restarted child); pi
(the pi adapter's) its `PI_*` settings, `EDITOR` / `VISUAL`,
`WAYLAND_DISPLAY`, the proxies, and its model providers' variables, which
the adapter does not use (yoagent is the model).

**How: `-u` every inherited name not kept, not `env -i`.** `env -i`
would drop what the launch adds as it starts the process — the launcher's
`env` entries (`DSH_HOME`), a stdio server's `env` table, and in loopback
handover rutis's `RUTIS_CHANNEL_TOKEN` (the runtime could not connect
back). So the list `env_without` is given becomes every name in yobuild's
own environment that is neither kept nor granted (nor set by that launch,
as now): `/usr/bin/env -u A -u B … <program>`. `RUTIS_CHANNEL_TOKEN` is
not in it while rutis hands over by loopback (off unix, or
`RUTIS_LOCAL_HANDOVER=loopback`, as rutis decides it) for a runtime rutis
spawns: rutis sets it on the `/usr/bin/env` command itself, so a `-u`
would remove rutis's fresh token. Otherwise — and always for a stdio
server, which rutis does not start — one in yobuild's environment is its
own (from a rutis parent, say) and is taken out like any other (found in
review). A name is what `std::env::vars_os` reads: what precedes the
first `=` after an entry's first byte, so it may hold anything else
(`BASH_FUNC_x%%`), which `env -u` takes whole. Two kinds pass untouched,
warned once a process: a name holding `=` (an entry `=x=1`, made by a parent
that wrote `environ` by hand, reads as the name `=x`), which GNU and BSD
`env -u` both refuse, failing the start — skipped; and an entry with no
`=` after its first byte, which `vars_os` does not return, so it cannot
be named. The command line grows with the environment — about 100 names,
a few kilobytes, far below `ARG_MAX`.

What changes beside the list:
- **Where the kept set lives.** `frontend::host` stays generic: it gets
  no `run::KEPT`. `PluginHostBuilder` gains `inherit(Inherit)` — the names
  and prefixes every runtime keeps (`Inherit::only(names, prefixes)`;
  absent, today's behaviour) — and `keep(runtime, name_or_prefix)` takes
  a prefix too, for a runtime's extras (the `ui`-only
  `environment::UI_KEPT`: `PI_HYPERLINKS`, …, `PI_TUI_*`; a grant).
  The list is built by the builder at `start` from yobuild's environment.
  `withhold` stays for off unix (step 4's set-empty). `KEPT` lives in a
  new `src/environment.rs` (`yobuild::environment`, crate-private), which
  `run`, `check`'s `rutis_alone` and the MCP launch read.
- **Every plugin runtime and stdio server on unix starts through
  `/usr/bin/env`**, not only while a credential is set, and also when the
  `-u` list is empty (`env_without` no longer returns the program as it
  is), so a granted `DYLD_*` is decided by SIP in every case and the note
  below is exact. A machine without `/usr/bin/env` starts none.
  `env_without`'s error names no variables (it would list about 100):
  "cannot start X with the kept environment: /usr/bin/env is not there,
  and yobuild needs it to start a plugin runtime or stdio MCP server"
  (not "yobuild cannot start X: …", which the `cannot-start` hint,
  earlier in the table, would take); the `no-env` hint's fix follows, and its
  test.
- `startable` looks for every program first (it already does for a
  server, and for a runtime whenever something is withheld — from now on,
  always), so rutis's own "cannot start node: No such file or directory"
  is reached only off unix: the `no-node` hint stays for that, its comment
  saying so.
- **The `DYLD_*` note narrows.** `DYLD_*` are not kept, so the rule itself
  drops them whatever System Integrity Protection does. What SIP still
  decides is a `DYLD_*` name an entry grants or a server's `env` sets: it
  reaches the program only with SIP off. `dyld_note` says only those;
  `started_through_env` and `Platform::terminal` go (everything starts
  through `env`).
- The helper scripts (`node_json`) clear their environment
  (`Command::env_clear`) and set the kept set's values, on unix; off unix
  they keep step 4's removal (Node on Windows needs `SystemRoot`,
  `PATHEXT`, `USERPROFILE`, `APPDATA`, `COMSPEC`, … — a Windows kept list
  is roadmap question 7's).
- `keys` → `env` through the code: `Grants::keys`, `Plan::keys` / `kept`
  / `keep` and its refusal ("(`a` has X, `b` none)"), `keys_unset` /
  `UnsetGrant` / `unset_grant_note` ("its `env` grant names …"),
  `keys_given` (a grant's names, set here); the grants table's row (§12
  "What a grant hands a plugin"); host_test's
  `a_withheld_variable_is_gone_from_every_runtime_unless_kept_or_set`
  stays, and the kept environment gets a test of its own
  (`kept_env_test`).
- `withheld_from` stays for the credentials yoagent reads only — each
  entry's note names those it loses when the entries differ, as now; the
  other names left out are `check --json`'s `leftOut` (the agent's), not a
  note per entry. `withheld_note` and `TAKEN` give way to the counted note
  under "Said" (5b).
- `examples/coding_agent.rs` keeps step 4's withholding of
  `KEY_VARIABLES`: a library example, not `yobuild run`; its doc comment
  says so.

**The kept set** (`environment::KEPT`, fixed names and a few prefixes;
none is a credential yoagent reads nor a name `manifest::secret` flags,
and a test says so):
- *the process*: `PATH`, `HOME`, `USER`, `LOGNAME`, `SHELL`, `PWD`,
  `TMPDIR`, `TMP`, `TEMP`, `TZ`, `LD_LIBRARY_PATH` (a `node` installed
  through Nix, Linuxbrew, conda or an HPC module, and a native addon linked
  against such libraries, may not start without it; on macOS `DYLD_*`
  stay out, above);
- *the locale*: `LANG`, `LANGUAGE`, `LC_*`;
- *the terminal*: `TERM`, `COLORTERM`, `TERM_PROGRAM`,
  `TERM_PROGRAM_VERSION`, `NO_COLOR`, `FORCE_COLOR`, `COLUMNS`, `LINES`,
  and pi-tui's detection list above (`TMUX`, `STY`, `ZELLIJ`, `WT_SESSION`,
  `KITTY_WINDOW_ID`, `ITERM_SESSION_ID`, `WEZTERM_PANE`,
  `GHOSTTY_RESOURCES_DIR`, `WARP_SESSION_ID`, `WARP_TERMINAL_SESSION_UUID`,
  `TERMINAL_EMULATOR`, `TERMUX_VERSION`, `SSH_TTY`, `SSH_CONNECTION`,
  `SSH_CLIENT`, `DISPLAY`), and pi's `WAYLAND_DISPLAY` — none a secret,
  and a plugin that draws output reads them;
- *the session*: `BROWSER` (where a sign-in page opens), `CI` (a plugin
  under CI behaves as it would there);
- *the network*: `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY` and
  their lowercase forms, `NODE_USE_ENV_PROXY` (Node's `fetch` and
  `http`/`https` use the proxy variables only with it, on Node 22.21+ and
  24; on 22.19, yobuild's minimum, it does nothing), `NODE_USE_SYSTEM_CA`
  (the system's certificates), `NODE_EXTRA_CA_CERTS`, `SSL_CERT_FILE`,
  `SSL_CERT_DIR` — a plugin behind the owner's proxy or corporate CA must
  reach the network as the owner does (a proxy URL may hold a password: it
  is the owner's network setting, kept as such, and said, below);
- *standard folders*: `XDG_CONFIG_HOME`, `XDG_DATA_HOME`,
  `XDG_CACHE_HOME`, `XDG_STATE_HOME`, `XDG_RUNTIME_DIR`;
- *where programs live*: those pi-lens reads (`GOPATH`, `GOBIN`,
  `CARGO_HOME`, `JAVA_HOME`, `JDTLS_PATH`, `VIRTUAL_ENV`, `CONDA_PREFIX`,
  `UV_PROJECT_ENVIRONMENT`, `BUN_INSTALL`) and their kin, by the same
  reasoning, not measured (`GOROOT`, `RUSTUP_HOME`, `PNPM_HOME`, `NVM_DIR`,
  `VOLTA_HOME`, `DENO_INSTALL`);
- *yobuild's own runtimes*: `RUTIS_SHUTDOWN_TIMEOUT` (every runtime); and
  in the terminal UI's runtime only (`ui`): `PI_HYPERLINKS`,
  `PI_TRUE_COLOR`, `PI_IMAGE_PROTOCOL`, `PI_PROGRAM_STATUS`, `PI_TUI_*`.

Not kept, on purpose:
- `NODE_OPTIONS` (it can `--require` code into every runtime; an owner's
  debugging flag is not a plugin's — decision 4), `NODE_PATH`,
  `LD_PRELOAD` and `DYLD_*` (code into every process), `TSX_*` (the
  owner's tsx settings; yobuild's runtimes need none);
- the rest of `RUTIS_*`: `RUTIS_TOKEN` and the TLS files are a remote
  link's credentials (yobuild's runtimes are local), the others test
  knobs — so an owner's stray `RUTIS_CA` no longer fails a runtime;
- `npm_config_*` (`npm_config_proxy` among them: an owner whose proxy is
  set only there, for npm, sets `HTTPS_PROXY` for plugins);
- `DSH_*` and `CORDIS_SHARED`: the DSH runtime gets what yobuild's
  launcher sets. Where yobuild gives DSH no home (no host services, no
  DSH host), an owner's `DSH_HOME` no longer reaches it, so DSH and
  `dsh-free-search` use `~/.dsh`; an owner who wants theirs, or their
  `DSH_WEB_SEARCH_PROVIDER`, grants it on the DSH plugins;
- `EDITOR` / `VISUAL`: no plugin opens an editor — every runtime but the
  terminal UI's runs in a process group of its own, where touching the
  terminal stops it;
- `AWS_PROFILE`, which step 4 keeps (not a key): it selects the owner's
  cloud account, so a plugin that should use it is granted it;
- every other program's settings, and every secret.

What the launch sets (rutis's channel, `DSH_HOME`, a server's `env`) is
the launch's, as now.

**Granting more.** One grant, replacing step 4's `keys` (unreleased;
nothing to migrate): `grants = { env = ["TAVILY_API_KEY", "PI_LENS_*"] }`
on a `[[plugin]]` — names, or a prefix: at least two characters, then
`_*` (`PI_LENS_*`; `A_*` and `*` refused). The rules are `keys`'s: on any
plugin, the same on every pi plugin, every DSH plugin and every rutis
plugin of one package (one runtime, one environment), each name once,
sorted. Some names pass only when named in full, never by a prefix — the
credentials yoagent reads (`Manifest::key_variables()`, the agent's
`api_key_env` among them) and what loads code into the process
(`NODE_OPTIONS`, `NODE_PATH`, `LD_PRELOAD`, `DYLD_*`): a prefix that would
match one is refused ("`AWS_*` would hand over `AWS_ACCESS_KEY_ID`, a
credential yoagent reads: name it"; "`NODE_*` would hand over
`NODE_OPTIONS`, which loads code into the runtime: name it"), so `check`'s
notes stay exact. Named in full they pass, the owner's choice, noted:
a credential as now ("it is given `X`, … not metered"); a code-loading
name as "it is given `NODE_OPTIONS`, which can load code into its
runtime" (a `DYLD_*` one adding the SIP note below). An `env`-only grant,
as `keys` now, starts no DSH host (`hosts_nothing`). Any other granted
name that is set here and that `manifest::secret` flags (the function
made crate-visible: key, token, secret, password, auth, credential, …) is
noted "it is given `X`, set here" — the owner's account, used as the
plugin sees fit. A granted name not set here: said as `keys` is now
("unset or empty here: not handed over"). An MCP server needs no grant:
its `env` table already hands over a value (`env = { TAVILY_API_KEY =
"${TAVILY_API_KEY}" }`), under any name.

**Said.**
- `check`, per entry: `--json` gains `environment` — the names it gets
  beyond the kept set (its grants, a server's `env`) — and a note for each
  variable **its own code names, set here, that it does not get**. For a
  plugin: "its code reads `TAVILY_API_KEY`, which is set here but left out
  of its environment: grant it (`grants = { env = [\"TAVILY_API_KEY\"]
  }`) if it should have it"; for a stdio server: "…: hand it over in its
  `env` table (`TAVILY_API_KEY = \"${TAVILY_API_KEY}\"`) if it should have
  it"; for a credential yoagent reads, never a suggestion to grant: "its
  code reads `OPENAI_API_KEY`, a credential yoagent reads, taken out of
  its environment: one handed over is spent unmetered" (as built, after
  #59's review; "set empty in its environment" off unix); for a name that
  loads code, left out on purpose, none either (as built: "…, which is set
  here but left out of its environment on purpose: it can load code into
  its runtime"). The names come from
  the scan above, run on the entry's own files: a pi or DSH package's
  folder in its set (not its dependencies), a local file, a stdio
  server's package. Only names set here, and not kept or granted, are
  said, so a plugin's hundred tunables say nothing unless the owner set
  one. It is a hint, not a proof, with the scan's limits above.
- `check`, for the agent, when a kept proxy variable holds a password
  (`written_password`, made crate-visible and taught a proxy written
  without a scheme, `user:pw@proxy:3128`): "`HTTPS_PROXY` hold(s) a
  password, and every plugin and stdio MCP server gets it as the owner's
  network setting" (as built).
- `run` and `check` replace step 4's combined note with one that names the
  rule and counts: "plugins and stdio MCP servers start with the kept
  environment (the path, home, locale, terminal, session, proxies,
  standard folders, toolchains) and what their entries grant: N
  variable(s) set here are left out of one or more of them (`check
  --json` lists them: the agent's `leftOut`), among them the credentials
  yoagent reads (`X`, …); an entry that needs one gets it only from its
  entry (…), and what it spends with it is not metered" — the credentials
  named as now, the rest counted; with no credential among them, "…; an
  `env` grant on a plugin, or a stdio server's `env` table, passes one it
  needs" (as built).
- §6 ("Not covered") and the honour-system bullet: plugins start with the
  kept environment and what is granted, not the owner's whole
  environment; still not a sandbox (files, dotfiles, `.env`s).
- The grants table (§12 "What a grant hands a plugin"): the `keys` row
  becomes `env`.
- The skill's `manifest.md` (the `env` grant, what is kept), `parts.md`
  (pi-web-access's paid engines and the langfuse observer need `env`
  grants; `dsh-free-search`'s `RESEARCH_CACHE_DIR` moves its cache only
  when granted; pi-lens's toolchains are kept) and `failures.md` (a
  plugin's authentication error when its key is left out: the note above;
  the reworded `no-env`), and `CLAUDE.md`.

**Where it applies.** As step 4: the plugin runtimes (`ui`, `pi`, `dsh`,
`rutis-<n>`) in `run` and in `check` (alone and together), the stdio MCP
servers, the helper scripts (`node_json`: the kept set, no grants; they
import a package's code). `check`'s pi and DSH probes alone:
yoagent-rutis builds their launch and can only add or set empty, and
yoagent's owner chose not to add removal (2026-10-21, step 4's History) —
proposed here as keeping step 4's behaviour; as built (5a's review), they
get every name `run` leaves out set empty instead, said by the note's
suffix. Not npm (its own `npm_config_*` filtering, step M2b), not the
agent's `bash`, not `examples/coding_agent.rs` (above). Off unix: no `env`
to start through, and the helpers keep step 4's removal — step 4's
credentials set empty or removed, nothing else; said.

**Decisions for review.**
1. *`env` replaces `keys`* (recommended: one list of what passes, a
   credential among them noted as now) — or `env` beside `keys`. Step 4
   named the grant `keys`, not `env`, because a server's `env` sets
   values while the grant lets names through. With step 5 the grant is
   the plugin's environment: like a server's `env` table it decides what
   that process's environment holds, by name rather than by value, so the
   one word serves both. (The alternative names: `pass`, `inherit`.)
2. *Where programs live is kept by default* (recommended: not secrets,
   and without them pi-lens looks in fewer places — expected to find less
   without saying why; not measured) — or each plugin grants its own.
3. *No opt-out* (recommended: a plugin that needs a variable is granted
   it, named in the manifest and said by `check`) — or a manifest-wide
   `[builtins] environment = "inherit"` for debugging.
4. *`NODE_OPTIONS` left out* (recommended) — or kept, as the owner's Node
   setting. Two of its common uses are kept in their own form:
   `--use-env-proxy` as `NODE_USE_ENV_PROXY`, `--use-system-ca` as
   `NODE_USE_SYSTEM_CA`. Left out is what else an owner sets there for
   every Node program, such as a memory limit; an owner who needs it for a
   plugin grants `NODE_OPTIONS` by name, noted.

**Tests.**
- Host test (the kept-environment test, replacing step 4's): a runtime
  sees the kept names, a launcher `env` entry and, with
  `RUTIS_LOCAL_HANDOVER=loopback` set in the test, rutis's
  `RUTIS_CHANNEL_TOKEN` (it connects, a stale `RUTIS_CHANNEL_TOKEN` in
  the test's own environment notwithstanding), and not an unrelated
  variable nor `RUTIS_CA`; a granted name and a granted prefix reach only
  that runtime; `UI_KEPT` (pi-tui's `PI_*` settings) reach `ui` alone; a
  program not there said by `startable` with nothing withheld.
- `cli_test`: step 4's test with `UNRELATED` now unset everywhere,
  `HTTPS_PROXY` and `LANG` kept, an `env` grant (a name and a prefix) on
  the pi, DSH and rutis plugins each reaching only its runtime, a server's
  `env` handing one over; `check`'s `environment`, `leftOut`, the "set
  here" note on a secret-named grant, the code-loading note on a granted
  `NODE_OPTIONS`, and the "its code reads" notes (a plugin's, a server's, a
  credential yoagent reads) on fixtures naming `YOBUILD_TEST_TOKEN` and
  `OPENAI_API_KEY`; on macOS the `DYLD_*` note for a granted `DYLD_*` name
  and a server's `env` one; the terminal UI on a pty
  (`tests/terminal_pty.py`, `tests/run_terminal_pty.py`) with the
  allowlist on.
- `src/manifest/tests.rs`: `env` refused with a bad name, a short or
  misplaced `*`, a prefix that would match a credential yoagent reads (the
  agent's `api_key_env` too) or a code-loading name, a repeat, a list that
  differs within a runtime; an `env`-only DSH grant starts no DSH host.
- `hints.rs`: the reworded `no-env` against `env_without`'s message;
  `failures.md` follows.
- Unit: `KEPT` holds no credential yoagent reads and no name `secret()`
  flags; the `-u` list is every inherited name not kept, granted or set by
  the launch, never `RUTIS_CHANNEL_TOKEN`, a name holding `=` skipped;
  `env_without` wraps an empty list; the code scan (`process.env.X`,
  `process.env["X"]`, nothing in `node_modules`); the proxy-password note,
  with and without a scheme; `node_json`'s cleared environment.
- The gallery and the examples keep passing in CI, on Linux and in the
  `macos` job (their `check` did in B above, on macOS; their scripted runs
  were not measured).

**Measured again after** the code: the same harness (A against the
allowlist) over the same plugins, on macOS and Linux, with each gallery
agent's and example's scripted runs too, the table above re-checked, and
a live run of the handoff example.

**Built in two parts.** *5a* (2026-10-22): `src/environment.rs`
(`KEPT`, `KEPT_PREFIXES`, `UI_KEPT`, `CODE_LOADING`), `host::Inherit`,
`PluginHostBuilder::inherit`, prefixes in `keep` (`host::matches`),
`host::not_inherited`, `env_without` always through `env`,
`run::kept_environment`, the stdio servers' `without`, `Grants::env` /
`passes` (with `env_entry`'s refusals), `Plan::env`, `run::env_unset`,
the narrowed `dyld_note`, `plugins::kept_only` for the helpers, the
reworded `no-env`; tests: `kept_env_test` (a file of its own, for
`RUTIS_LOCAL_HANDOVER=loopback`; checked to fail when the channel token
is taken out), the unit tests above, `cli_test`'s
`plugins_and_servers_have_the_credentials_taken_out_unless_handed_one`
(an unrelated variable gone, `LANG` kept, a prefix grant) and
`a_server_that_cannot_start_is_said_with_what_to_do` (a server's own
`DYLD_*`). After the review of #58: a stdio server whose `-u` list
would hold a name that is not UTF-8 is refused (yoagent's transport takes
the command as text, and a lossy name would let the variable through);
`RUTIS_CHANNEL_TOKEN` kept only in loopback handover; `check`'s pi and
DSH probes alone get every name the kept environment leaves out set
empty too (`ProbeOptions::env`; the probe's own settings win), so they
see what `run` gives; `run` says, as an Info notice, how many variables
set here the kept environment leaves out (off unix: that only the
credentials are) — which ones, and who reads them, is 5b's; asdf's and
mise's homes kept (`ASDF_DIR`, `ASDF_DATA_DIR`, `MISE_DATA_DIR`,
`MISE_CONFIG_DIR`: a `node` shim finds its versions through them); a
name `env -u` refuses warned once a process; `kept_env_test` checks the
runtime's channel came by loopback. After the second review: a stdio
server's `-u` list holds this process's own `RUTIS_CHANNEL_TOKEN` in
loopback handover too (rutis starts no server), as do the probes' empty
names (rutis sets its token after them); `run`'s notice counts the
variables that reach no entry (not kept, not passed by a grant or a
server's `env`, a name `env -u` can take). In the probes a name set empty
is not quite unset for a few programs (`UV_THREADPOOL_SIZE` empty is 0
threads, then 1; `NODE_DISABLE_COLORS` and `OPENSSL_CONF` are read when
present): nothing that changes what loads. `withheld_note`, `TAKEN` and
`withheld_from` stay as step 4 made them until 5b. *5b* (2026-10-23):
`environment::names_read` (the scan, capped at 5000 code files and
4 MiB a file; after #59's review a `Scan`: 64 MiB a file (`dsh-univer-office`'s
gateway bundle is 58 MiB), 512 MiB in all, 50 000 files and folders, no link below its start followed (a loop, or a monorepo's
link out, walked nothing), files read as lossy UTF-8, and what it missed
— a limit, a file too big, links, what could not be read — or no code
found said as a note on the entry, so no note never means "not looked"), `environment::proxy_password` (a proxy written without a scheme
too) and `PROXIES`; `run::Starts` (a plugin or stdio server: `gets`,
`environment`), `run::left_out`, `run::environment_note` (the one note,
replacing `withheld_note` and 5a's `kept_note`: a count of what one or
more entries do not get, the credentials named; said by `run` and
`check`), `run::reads_notes`, `run::given_notes`, `run::proxy_note`;
`check`'s report version 3 — `Entry::environment`, `Agent::left_out`
(`leftOut`), the entry's code scanned where `run` finds it
(`check::entry_code`: a package's folder in its set, a local file, a
server's package or program); the skill's `manifest.md`, `parts.md`
(pi-web-access's engines, the langfuse observer, `dsh-free-search`'s
`RESEARCH_CACHE_DIR`, pi-lens's settings) and `failures.md`; tests: unit
tests for each, `cli_test`
`check_says_what_a_plugins_code_reads_and_what_it_is_given`, and the
gallery and examples accepting the environment note. `withheld_from`
stays (each entry's lost credentials when they differ; after the review
it says "; its code reads `X`" for one the code reads, in place of a
second note). Also after #59's review: the DSH runtime's own home
(`DSH_HOME`, and `DSH_AGENTS_HOME` for a private one), set by its launch
when it has host services or the DSH host, is not left out of a DSH
plugin (`run::dsh_launch`); a code-loading name its code reads is said
without "grant it"; `run_test` `check_scans_each_entrys_own_code` (a
pinned server's package, a rutis plugin's file, a shell server with no
code); the gallery and `cli_test` accept the notes that depend on the
variables set where they run.

### What it unlocks, and the migration

Expected (measured afterwards, as the matrix is): univer's 13 file tools;
the DSH tool plugins whose only DSH-specific reads are the agent and session
(dsh-ops, dsh-remote, dsh-tiddlywiki, dsh-mnemosyne, and the cwd reads of
dsh-palimpsest, dsh-mcp-connector, browser-skill and the image generators —
each still needing what else it injects); pi-lens (measured 2026-10-21: with
`trust = true` its TypeScript server finds a type error, without it no
server starts and its diagnostics say "unavailable"); pi's observers (langfuse,
langsmith); likely pi-background-tasks (model info); with L3, visualize's and
univer's views.

Migration, in the PRs that make each change: the DSH base gains `dsh-agent`,
`dsh-session`, `dsh-scope`, `dsh-user-approval` and the jsonl backend, which
changes the `dsh` set's `base` hash — the examples' locks (`handoff.lock`,
`coding.lock`) and the gallery's are regenerated in the same PR, or
`--locked` fails CI; `DSH_PROBE_BASE` follows `host-services.json`;
`pi.exec` closed by default is said as a Warning naming the extension.
modsearch's two composition fixes are not part of this layer: `{}` for a
missing config is a bug fix of its own, and letting a bundle patch a base
row's config is a security change (a plugin reconfiguring DSH's files or
credentials services) for its own PR, limited to named keys.

**Order** (M3b; L3 and the metered `llm` are M3c): (1) the L1 services
(`workspace`, `session` with its id and generation, `approval` with
`Gate::check_argv`) and `grants` read and said, with rutis's grants
enforced per row; (2) the DSH host — the stand-in, turns, the jsonl log,
dialogs through DSH's own services, `workspace` and `wake`, fixed answers
said; (3) pi grants — `trust`, `model = "info"`, `wake`, `exec` (the
factory wrapper shipped in yoagent-rutis 0.7.0; `observe` dropped,
above); (4) removing provider keys from plugin
runtimes. Each a PR pair (yoagent-rutis,
then yobuild), each design-checked; step 2's yoagent-rutis half shipped in
0.7.1.

## 13. Share: yobuild without its repository (M4; proposed 2026-10-23)

**Why.** Today yobuild runs only from a set-up checkout.
- `Layout::default()` names the checkout's `frontend/` and `adapters/` by
  the path the binary was compiled at (`CARGO_MANIFEST_DIR`).
- `adapters/` is a gitignored copy of the yoagent-rutis crate's `plugins/`
  folder (`scripts/copy-adapters.sh`).
- The runtimes need `npm ci` in three folders.
- `run/plugins.rs::rutis_runtime` reads the rutis runtime's version pin
  from `frontend/plugins/package.json` for every rutis plugin.
- The examples name the frontend's plugins by path
  (`rutis = "../frontend/plugins/todo-plan.ts"`).
- The skill says "until yobuild is installable on its own, it runs from a
  checkout".

M4's "Done when": someone who has not seen the repository installs yobuild,
runs `yobuild new`, and has a gallery agent running from the docs alone; 0.1
tagged.

### What the binary needs at run time, and where it comes from

| What | Today | Proposed |
|---|---|---|
| The web page (`frontend/web/index.html`, `app.js`, `lib.js`) | built in (`include_str!`) | unchanged |
| The terminal UI (`frontend/plugins/terminal-ui.ts`, which imports `../web/lib.js`, ESM through `frontend/web/package.json`) and the UI plugins `search-links.ts` and `todo-plan.ts`, with `frontend/plugins/package.json` and its lock | the checkout, `npm ci` by hand | built in; the runtime set `ui` (below), as `plugins/` beside `web/lib.js` and `web/package.json` |
| The pi example policy `frontend/plugins/pi-extensions/confirm-dangerous.ts` (not its `.test.mjs`) | the checkout | built in, in the `ui` set, nameable (below) |
| The adapters (yoagent-rutis's `plugins/`: `pi/`, `dsh/`, `yoagent.d.ts`, which the adapters import as `../yoagent.d.ts`, each folder's `package.json` and lock; not the `fixture-*.ts`) | copied to `adapters/`, `npm ci` by hand | built in; the runtime set `adapters` (below) |
| The rutis runtime's version (`@arcships/rutis-runtime`) | read from `frontend/plugins/package.json` | a constant, checked in a unit test against that file |
| Node 22.19+ and npm 11.19+ | the person's | still the person's, checked first (below) |

### The runtime sets

The cache gets two sets of yobuild's own, installed the way `install.rs`
installs a manifest's pins (design §4: written in a `.partial-` folder,
`npm ci` with `npm_flags`, the stamp last, renamed into place; a folder
without its stamp installed again).

- **`adapters`.** `<home>/runtime/adapters/<hash>/` holds `pi/`, `dsh/`
  and `yoagent.d.ts`, one root with both, as yoagent-rutis's probes need:
  `probe::pi_with(root)` and `dsh_with(root)` use `root/pi` and `root/dsh`.
  It is `Layout::adapters`.
- **`ui`.** `<home>/runtime/ui/<hash>/` holds `plugins/` and `web/` (the
  two files above). It is `Layout::frontend`.
- **The hash.** The first 16 hex of a sha256 over the set's name and,
  sorted by path, each file's path and bytes, as `install.rs`'s `hash`
  does over its own inputs. A changed file gives a new folder; an old
  folder stays usable by the old binary. Old folders are not removed; the
  manifest sets' are not either (§4). Cleaning the cache stays the
  person's, said in the guide.
- **Installed in each folder.** `adapters/pi` and `adapters/dsh` are each
  `npm ci`ed, and so is `ui/plugins`. Their install scripts run, as
  `scripts/try-coding-agent.sh` runs them today: these are yobuild's own
  packages, pinned by locks it ships, unlike a manifest's pins, which get
  `--ignore-scripts`. Among them, `esbuild` and `fsevents` place
  platform binaries. Running them is what has been tested so far.
- **When.** On first need:
  - `adapters` whenever a pi or DSH plugin loads, or `check` probes one;
  - `ui` for the terminal UI or a built-in plugin.

  The manifest's `dsh` set keeps holding its own copy of the DSH files and
  base (§4). With DSH packages pinned, the DSH runtime runs from that set,
  as today, and only the `adapters` set's `pi` side is used. The helper
  scripts (`resolve-package.mjs`, `inspect-package.mjs`; Node built-ins
  only) run from the `adapters` set either way.
- **Said.** Like a manifest's sets: "runtime `adapters` installed (pi, dsh)".
  `--locked` does not apply: these sets carry their own locks, are never
  stale and never in `agent.lock`. npm is needed with or without it.

**`Layout`.**
- **`Layout::installed(home)`** (the binary; and `Layout::default()` for
  library callers, which so far are the tests).
- **`Layout::checkout()`** (the repository's folders): for
  `examples/coding_agent.rs` and the tests that load the adapters'
  fixtures, which stay on `scripts/copy-adapters.sh`. The dialog tests
  name their own paths and are unchanged.
- **The tests.** `cli_test` and `gallery_test` run the binary, so they
  use the installed sets, from the tests' shared `YOBUILD_HOME`: each set
  installed once a job. Their skips on `adapters/*/node_modules` change to
  the Node check's.

**Where the adapters' files come from at build time.** They are
yoagent-rutis's `plugins/` folder, which its `adapters()` names by path
(`CARGO_MANIFEST_DIR`). Its doc says a binary on another machine must ship
a copy. Three ways:
- **(a)** yoagent-rutis exposes them as data,
  `adapters::files() -> &'static [(&'static str, &'static [u8])]`: paths
  relative to `plugins/` (`pi/pi-extensions-adapter.ts`, …,
  `yoagent.d.ts`), the fixtures left out by the crate. One source of
  truth, checked by the crate that owns it. It needs a yoagent-rutis
  release, the yoagent session's.
- **(b)** yoagent-rutis declares `links = "yoagent-rutis"` and exports its
  folder from a build script (`DEP_YOAGENT_RUTIS_PLUGINS`). yobuild's
  build script then embeds what it lists. No API, but also a release.
- **(c)** yobuild vendors a copy under `assets/adapters/`, refreshed by
  `scripts/copy-adapters.sh` and checked in CI against the crate's files.
  No release needed; a second copy to keep in step.

Recommended: (a).

### Node and npm, checked first

Before `prepare` (whose install runs npm) and before any runtime, whenever
the manifest needs Node, `node --version` and `npm --version` are read. The
manifest needs Node when it has:
- any plugin;
- a pinned server (installed with npm);
- the terminal UI.

A missing one, or one older than Node 22.19 or npm 11.19 (the skill's
floor for pinned DSH plugins), is an error naming what was found:

> yobuild needs Node 22.19+ and npm 11.19+: found node 20.11.0

It gets a new hint, `no-node-version`, with `failures.md`'s row. The `no-node`
hint stays for rutis's own message. A run that needs no Node, an HTTP-only
MCP agent say, checks nothing.

### Built-in plugins, by name

A manifest names a file by path, so nothing in the cache could be named.
A plugin's source may instead be `yobuild:<name>`:
- **The rutis plugins:** `rutis = "yobuild:todo-plan"` and
  `rutis = "yobuild:search-links"`.
- **The pi policy:** `pi = "yobuild:confirm-dangerous"`.

Each resolves to its file under `Layout::frontend`. That is the checkout's
`frontend/` until the `ui` set exists (PR 3), and the `ui` set after. An
unknown name is refused, listing the known ones of its kind; on DSH
(which has none) and on an MCP server the refusal says so. `Source::Builtin(name)`
joins `Package` and `Path` (`#[non_exhaustive]` already). The examples
switch to these names, so they run from any folder.

### New commands

- **`yobuild new <name> [--from <agent>]`.** It makes the folder `<name>/`,
  refused if it exists.
  - **Alone**, it writes:
    - `agent.toml`:
      - `deepseek/deepseek-flash`;
      - `tools = "read-only"`;
      - `ask_owner = true`;
      - `report = "handoff"`;
      - `budget_usd = 0.25`, `session_budget_usd = 1.0`;
      - the web page;
      - every other top-level key shown commented, one line on what it
        does, pointing at the skill's `manifest.md`.
    - `prompts/agent.md`: a role to fill in.
    - `scripts/demo.json`: one `list_files` call, then a reply naming the
      report's sections.
    - `scripts/demo.task`.

    A unit test parses the template as written (the commented keys too,
    uncommented one by one) and a `cli_test` runs `check` and the demo on
    it.
  - **With `--from`**, it copies an agent built into the binary:
    - the gallery's five (`web-research`, `notes-keeper`, `code-reviewer`,
      `spreadsheet-clerk`, `code-health`);
    - the examples (`handoff`, `coding`, `minimal`), as `agent.toml` with
      its prompt, lock and reply files.

    It copies them unchanged but for two things, said when it ends:
    - `[agent] name` becomes `<name>`, so its scratch and log are its own;
    - the README's `path/to/gallery/<x>/` becomes the new folder's path.

    The agent is the coding agent's work; `new` hand-edits nothing else.
    `yobuild new --list` names them, one line each.
  - It ends by printing the next commands: `check`, the demo scripted,
    a live run.
- **`yobuild skill install [--project | --dir <folder>] [--force]`.** It
  writes the skill, built into the binary, to `~/.claude/skills/yobuild/`
  (Claude Code's, for every project), `./.claude/skills/yobuild/` with
  `--project`, or any folder with `--dir` (the skill is plain Markdown:
  another coding agent can be pointed at it; decided 2026-10-24), with a
  stamp
  (`.yobuild-skill`: the yobuild version and a hash of the files written).
  - **When the folder is there:**
    - stamped, and its files still as written: replaced (an upgrade), and
      said;
    - files changed since (edited by hand): refused without `--force`,
      naming them; no stamp but a `SKILL.md` (a copy made by hand):
      refused without `--force`;
    - a folder holding no skill (no stamp, no `SKILL.md`: a project, a
      home, a mistyped `--dir`): refused, `--force` or not.
  - **With `--force`** the skill's files are written over; files an
    earlier yobuild wrote and this one does not ship are removed, and
    nothing else (as built: never the whole folder, which `--dir` lets
    name anything; files of one's own beside it stay).
  - **The skill names no repository path.**
    - `SKILL.md`'s "runs from a checkout" becomes "installed: `cargo
      install yobuild` or a release binary".
    - `scripts/try-coding-agent.sh` and "never add … to yobuild's
      `adapters/`" go.
    - `manifest.md`'s `examples/…` become `yobuild new --from handoff`
      (and so on).
    - `parts.md`'s `frontend/plugins/` become `yobuild:` names.
    - Messages the skill quotes change with it: the `dsh-host` hint's
      "adapters/dsh: `npm ci` there" and `install.rs`'s "the files in
      adapters/dsh" name the runtime set instead.
    - `skill_test` checks there is no repository path.
- **`yobuild --version`.**

### Upgrades and locks

A manifest's `dsh` set records `base`, a hash of the DSH adapter's
`package.json` and lock (§4). A yobuild whose DSH adapter changed finds
every lock with DSH pins stale. Without `--locked` that set is resolved
again and said; with it, refused. That includes agents copied by
`new --from` and run by a newer binary.

The refusal is kept, since the base is part of what was locked, and is
made plain:

> agent.lock: its `dsh` set was locked with another yobuild's DSH base
> (this one's changed): run once without `--locked` to lock it again

`CHANGELOG.md` says, for each release, whether the DSH base changed.

### Distribution

- **crates.io (open question 2).** Decided, and done in 0.1: publish `yobuild` 0.1 as
  a binary crate (`cargo install yobuild`).
  - Its library (`yobuild::builtins`, `frontend`, `install`, `manifest`,
    `run`) stays, since the tests use it, documented as having no API
    promise before 1.0.
  - Cargo.toml changes:
    - `publish = false` goes;
    - `version` becomes 0.1.0;
    - an `include` list: `src/`, `frontend/web/` and the `frontend/plugins/`
      sources and lock (no `node_modules`), `gallery/`, `examples/`,
      `skill/`, plus `assets/adapters/` under (c).

    The package stays well under crates.io's 10 MB. Everything it depends
    on is on crates.io, given (a) or (b) released.
- **Prebuilt binaries.** A release workflow on a `v*` tag builds for macOS
  (arm64, x86_64) and Linux (x86_64, aarch64; glibc). It attaches tarballs
  and checksums to the GitHub release, plus `cargo binstall` metadata. No
  install script piped to a shell.
- **Publishing the crate** (decided 2026-10-25, as yoagent does it): a
  person publishes the draft release, and that publishes the crate —
  `publish.yml`, on `release: published`, runs `cargo publish --locked`
  at the release's tag. Run by hand with a tag, it publishes a tag made
  before it existed (v0.1.0: a release runs the workflow of its tag's
  commit). 0.1.0 went with a token (`CARGO_REGISTRY_TOKEN`: a new crate's
  first version needs one); from 0.2 by crates.io's trusted publishing
  (the crate's settings name this repository, `publish.yml` and the
  `crates-io` environment; a token for 30 minutes from the job's GitHub
  identity, none stored). A version already there is skipped; a failure
  is said on the release ("Not on crates.io yet", with the run); a
  pre-release (a tag with `-`) stays off crates.io.
- **Windows (open question 7).** Decided, and shipped so in 0.1: not
  supported.
  - Off unix there is no kept environment (only the credentials, set
    empty), the terminal UI reads `/dev/tty`, and nothing is tested there.
  - So no Windows binary, and the README says unsupported.
  - On Windows `yobuild run` and `check` refuse with one line, "Windows is
    not supported yet (yobuild 0.1: macOS and Linux)". The CI job
    `windows` (`cargo check --locked` on windows-latest) keeps it
    compiling for later.
- **The version.** 0.1.0, with a `CHANGELOG.md` from the milestones.

### Docs

- **`README.md`, as the quickstart:**
  - install (cargo or a release binary; Node 22.19+, npm 11.19+);
  - `yobuild skill install`;
  - `yobuild new my-agent --from web-research`;
  - `check`;
  - a scripted run, then a live run with a key;
  - where the skill takes over.
- **`docs/guide.md`**, the person's manual:
  - the manifest (pointing at the skill's `manifest.md`), grants, the
    kept environment, the gallery, what to do when a run fails;
  - a rutis plugin of one's own (its package and `npm install`, the
    runtime version yobuild names);
  - the cache and how to clean it.
- **The roadmap's "docs site, the video re-cut, posts"** move out of M4's
  "Done when", to a later item.

**Done-when check, in CI.** A `fresh-machine` job builds the release
binary, then moves the whole checkout out of reach (renamed) before
running anything. So no file read through a compiled-in path can pass,
and any leftover `CARGO_MANIFEST_DIR` read fails. Then, with an empty
`HOME` and `YOBUILD_HOME`:
1. `yobuild skill install`;
2. `yobuild new a --from web-research`, which installs the DSH `adapters`
   set and the manifest's `dsh` set: `check`, then `demo` under `--once`,
   `Completed`;
3. `yobuild new b --from coding`, which installs the pi side and the `ui`
   set for its `yobuild:` plugins: `check`, then its demo;
4. the terminal UI on a pty with `yobuild new c` (`tests/run_terminal_pty.py`).

No key is needed.

### Decisions for review

1. *crates.io* (open question 2). Decided: publish the binary crate,
   the library unpromised.
2. *Windows* (open question 7). Decided: not in 0.1, refused with a
   line, kept compiling in CI.
3. *The adapters' files.* Recommended: (a) yoagent-rutis exposing them;
   (b) a build-script link; (c) vendoring.
4. *Built-in plugins by `yobuild:` name* (recommended: the examples run
   anywhere), or the examples kept repository-only and the UI plugins not
   shipped.
5. *The gallery and examples built into the binary* (recommended: about
   450 KB of text, mostly locks), or fetched from the repository. Fetching
   needs the network and a URL to trust.
6. *Docs.* Recommended: README and `docs/guide.md` for 0.1; a rendered
   site, the video and posts later.

**Built so far:** PR 2 (yoagent-rutis 0.8.2, `embedded-adapters`, the
yoagent session's); PR 4, `yobuild:` built-ins (#66); PR 3, the runtime
sets (#67: `src/runtime.rs`, `Layout::installed`, the version constant,
the Node and npm check); PR 5, `yobuild new` (`src/agents.rs`: the
starter and the eight built-in agents, the examples gaining a `.task`
each) and `yobuild skill install` (`src/skill.rs`, with `--dir` for a
coding agent other than Claude Code, decided 2026-10-24), the skill free
of repository paths (`skill_test`), the README's Start. PR 6, the release
groundwork: Cargo.toml 0.1.0 with its `include` list and `cargo binstall`
metadata, `CHANGELOG.md`, `run`/`check` refused on Windows with a CI job
keeping it compiling, the release workflow (tarballs and checksums for
four targets, `cargo publish --dry-run`, a draft release; publishing by
hand) and the `fresh-machine` CI job (`web-research` and `coding` copied,
checked and run with the checkout moved away, and the terminal UI on a
pty). PR 3's findings:
- a DSH plugin file resolves its imports from where it is, so the tests'
  DSH fixture in the checkout's `adapters/dsh`, which imports Cordis,
  cannot run beside the cache's runtime; the binary takes
  `YOBUILD_RUNTIMES=checkout` for developing yobuild, which that test
  sets (every other binary test, and CI's runs, use the cache);
- on a cold cache the first runs say "installing yobuild's runtime …" on
  stderr, which an assertion on stderr's first line must allow.

### Order of PRs

1. this design;
2. yoagent-rutis's `adapters::files()` (the yoagent session);
3. the runtime sets, `Layout`, the rutis runtime's version constant, the
   Node check, the messages that named the checkout;
4. `yobuild:` built-in plugins and the examples moved to them;
5. `yobuild new` (with the built-in gallery and examples) and `yobuild
   skill install`, the skill made repository-free;
6. the release workflow, Cargo.toml's `include` and `publish`,
   `CHANGELOG.md`, the Windows refusal and `cargo check` job, and the
   `fresh-machine` job;
7. README and `docs/guide.md`;
8. tag 0.1 (and publish, per decision 1).

## 14. Agents that extend themselves (M5; decided 2026-10-25)

**Why.** An agent sometimes finds it lacks a capability: it has no linter,
or no way to read a spreadsheet. Today it stops and says so, and the person
edits `agent.toml`. M5 lets the agent find a plugin, ask the owner, load it
and go on, without anything running that the owner did not allow or
anything reaching the agent that `check` would refuse. This is what rutis
was built for: software that adds plugins to itself while it runs. It is
also the riskiest part of the plan: a running agent proposes code to run
on the owner's computer, and what it proposes may come from a page it read.

M5's "Done when" (roadmap): the demo runs live end to end, a refused
plugin is shown never loading, and a denied approval leaves the agent
without the plugin.

### What is decided (2026-10-25)

- **No catalogue built into yobuild.** Plugins are found by searching live
  sources: the npm registry and the MCP registry. yobuild does not keep a
  list of good plugins to go stale between releases; it vets what it finds,
  every time.
- **None of a package's code runs before the owner's first yes.** Before
  the owner is asked, a candidate is only read: its registry data, its
  install without scripts, a scan of what its code reads. Starting it is
  what lists its tools, so that comes after the owner allows it; then
  `check`'s MCP step judges the tools, and the owner is asked a second
  time, with the tools in front of them, before the agent gets any. (The
  first draft checked the server alone before asking; that runs its code
  first, and the review of #76 caught it.)
- **For the session only.** A loaded server is not written to
  `agent.toml` or `agent.lock`. When the session ends, yobuild says what
  was loaded and the `[[mcp]]` block that would keep it. The manifest stays
  the owner's to change.
- **MCP stdio servers first.** Each runs in its own process, so loading one
  touches nothing already running. pi, DSH and rutis plugins come in a
  later step (below).

### The two tools

Both are yobuild built-ins, offered only when the manifest turns them on
(below). The gate allows both, as it allows `ask_owner`: asking the owner
is part of what `load_plugin` does, and `find_plugin` only reads.

**`find_plugin(need, kind?)`** searches and returns candidates. It starts,
loads and installs nothing.
- `need`: a few words, naming the tool when one is known, such as
  "eslint" or "read xlsx files" (a description alone may miss: tried
  live, "lint TypeScript" did not find ESLint's server, below).
- `kind`: `"mcp"` only in stage 1; later `"pi"`, `"dsh"`, `"rutis"`
  (stage 2, below). The stages are what is found and loaded; the PRs that
  build stage 1 are numbered apart ("Order of PRs").
- Sources, for MCP servers, each with a 15-second limit (npm's search
  30, below):
  - the MCP registry, its `v0` API (`/v0/servers?search=`; base
    `https://registry.modelcontextprotocol.io`, or `YOBUILD_MCP_REGISTRY`
    for the tests: https, or http on a loopback address; its path kept;
    checked before anything starts, in `run` and `check`): only entries
    with an `npm` package and a `stdio` transport. Remote servers and PyPI
    or OCI packages are left out, and the result says how many were.
    - Its search matches the search text against any part of a server's
      name, so a need searched whole ("lint TypeScript") would rarely
      match a name, and one short word matches too much (`lint` finds
      `maxwellintl`). So each of the need's words is a search of its own,
      and a server is kept only when its name, package or description has
      one of them as a word.
    - The words: the need split at what is not a letter, digit or `-`
      (`lint-staged` stays one); up to four of three letters or more that
      are not plain (`mcp`, `server`, `tool`, `the`, `and`, `for`, `with`,
      `from`); a need of only such words (`mcp server`) is searched for its
      first four as they are. A need with no letter or digit is refused.
    - A server names a word when the word, or the word with `s`, `er`,
      `ers`, `ing` or `ed` after it, is one of its words (`linting` names
      `lint`); a word of parts (`lint-staged`) must be those parts in a
      row (`lint_staged` too).
    - Servers found by more of the words come first. Each server is counted
      once however many searches return it; those left out are counted by
      why (remote only, not an npm package over stdio, naming none of the
      words). A search that matched more than the page it returns (30) is
      said. Searches that fail are said with their count and reasons; all
      of them failing is the source failing;
  - npm's search, through the `npm` CLI (`npm search --json`, then
    `npm view <package> --json` for each candidate) with the npm settings
    `install` keeps (the registry and its login keys), so a mirror or a
    private registry holds. Its search gets 30 seconds, not 15: its first
    call in a cold cache was seen to take longer. Its `keywords:mcp`
    qualifier only ranks (tried live on 2026-10-26, "lint TypeScript"
    brought React hooks and the MCP SDKs), so a package is kept only when
    it is marked as MCP: an MCP keyword (`mcp`, `mcp-server`,
    `model-context-protocol`, `modelcontextprotocol`); a word of its name
    or of a keyword that begins or ends with "mcp" (`fastmcp`,
    `mcpserver`) or is `modelcontextprotocol` (the scope); or "MCP" or
    "Model Context Protocol" as words in its description. The rest are
    counted as left out ("not marked as MCP").
- The result opens with one line per source: how many it found, or that
  it failed and why (a time-out, an HTTP status, npm's message). One
  source failing is said there and the other's candidates are given; both
  failing makes the result an error (`is_error`) and a Warning notice to
  the owner, never an empty list that reads as "nothing found". One
  failing is a Warning notice too: the owner sees the search was half
  done. npm's search answering in another shape (a package without a
  `name`) is a failure, never "0 found"; packages under names yobuild does
  not take (npm's old forms) are counted as left out.
- Each candidate (those both sources found first, then each source's own
  in turn; one per package; at most 10), npm asked about at most four at
  once. npm is asked in rounds, each as many as could still be kept, up
  to 20 in all: one that names no program to start (no `bin`: a library,
  an SDK, which `load_plugin` could only refuse) — at the version it
  would take, else (none to take) its latest — is left out, counted on
  the sources' line ("6 left out once npm was asked: no program to start
  (a library, not a server)"), and makes room for the next. A version to
  take without one, where the latest has one, is listed not loadable;
  one with one, where the latest has none, is taken. Those never asked
  about are counted there too ("12 more found and not looked at (…): a
  narrower need puts others first"), so an empty list never hides them.
  For each:
  - the package and the version `load_plugin` would take: the newest
    release (`a.b.c`) not past the `latest` tag (before a pre-release
    `latest`, `2.0.0-rc.1`, only what comes before it: not `2.0.0`)
    published at least `min_age_days` ago (from `npm view`'s `time`).
    With none old enough, no `latest` tag, no list of versions, or a
    publish time missing or unreadable, the candidate is listed as not
    loadable, saying which. npm answering a `versions` or `time` of
    another shape is npm failing for that candidate, not "no version";
  - its description, licence (`license`, or the older `licenses` list),
    and repository, marked "claimed by the package" (nothing checks it);
    whether a provenance attestation is published with the version
    ("unknown" when npm gives no `dist`). Published is not verified:
    neither yobuild nor `npm install` checks it (`npm audit signatures`
    does, when someone runs it);
  - weekly downloads, from npm's downloads API only when npm's registry
    is `registry.npmjs.org` (with or without its last `/`), so a private
    registry's package names never reach npmjs; else "unknown", with why
    (another registry, the API's error in its words, a package it does
    not know, or one its answer left out). The unscoped packages are
    asked for in one request (the API's bulk form, up to 128), each scoped
    one alone (the bulk form takes none): asking each package in a
    request of its own ran into the API's rate limit (HTTP 429) for half
    of a search's candidates. The requests go one after another, 5 s
    each and 10 s for a round's together (those past it "not asked");
    a failed request makes "unknown" only its own packages;
  - when it is in the MCP registry, the arguments and environment
    variables the entry lists as required; from npm only, "not listed".
- All of it is text the packages' authors wrote. Each field has its line
  breaks and control characters made spaces (`flatten`, `host.rs`) and is
  cut to a length with an ellipsis (`clip`, a new helper beside it:
  `one_line` refuses text, it does not cut it). The result says the text
  is unverified.
- npm failing for every candidate (a registry down, every `npm view`
  timing out) makes the result an error and a Warning notice, as both
  sources failing does: not a list of candidates that read as "not
  loadable".
- The session remembers each candidate it returned (package, version, the
  registry entry); a later search where npm failed for a package does not
  replace one with a version to take. `load_plugin` takes only those, so a name the agent
  read on a page, or a look-alike of a real package, is refused unless a
  search found it.

**`load_plugin(package, version?, name?, args?)`** proposes one server.
Without `version` it takes the one `find_plugin` offered for the package;
one given must be that one (a server publishes often: a version written
into an agent's prompt or a reply file would soon be refused). In
order, stopping at the first refusal:
1. **The limits.** Not under `--once` (nobody is there to answer; under
   `--once` the result says so and nothing is counted). No other load in
   progress (two at once: the second is refused, "another load is in
   progress"). The package (at that version, when given) among this session's
   candidates, and loadable. Not loaded already (one that stopped is said
   so: it is not loaded again this session), and not blocked by the owner
   this session (any version; other refusals, nobody answering among
   them, are not remembered). `max_attempts` not reached; this call then
   counts as one, whatever refuses it after. `max_loads` not reached (a
   server that stopped still counts). A call whose arguments are not of
   these shapes is refused before any of this, and not counted.
2. **The name.** `name` if given; else the package's name without its
   scope, without a leading `server-` or `mcp-server-` and a trailing
   `-mcp` or `-mcp-server`; when nothing is left, or only `mcp`, `server`
   or `mcp-server` (`@eslint/mcp`), the scope without `@` (`eslint`);
   unscoped with nothing left, the package's name as it is. It takes
   letters, digits, `-` and `_` (not the `.` a manifest's name may hold:
   a tool's name takes none), no `__` and no `_` at its end (`a_` with a
   tool `_x` would read as `a` with `__x`), short enough that
   `<name>__<tool>` can be a tool's name, and not the name of any of the
   manifest's plugins or servers or of a server already loaded: otherwise
   refused, asking the agent to pass another `name`. Its tools are
   `<name>__<tool>`.
3. **What it is handed.** `args` pass the manifest's rule for a server's
   `args` (`guard`), with no `${…}` at all (nothing fills one) and no
   control character (the block that keeps it is TOML). They are the
   server's own and shown whole in the question, an argument naming a path
   outside the agent's folder marked so. A server whose MCP registry entry
   lists a required environment variable is refused: in stage 1 nothing
   hands a loaded server a credential, and it would only start broken. A
   candidate from npm alone lists none; the question says its needs are
   not listed.
4. **Install, running nothing.** A one-entry `mcp` set, resolved with
   `npm install --package-lock-only --before=<now − min_age_days>`, so
   every package in its tree, not only the top one, was published that
   long ago (with `min_age_days = 0`, no `--before`); installed as
   `install` installs a set (`--ignore-scripts`, the kept npm settings),
   in the same cache, with a 5-minute limit per npm call
   (`install::install_one`). Its lock is resolved for this load only and
   written nowhere (no `lock_beside`). The program is found as `bin_of`
   finds a manifest server's: one `bin`, or the one named like the
   package; several and none so named is refused (`load_plugin` takes no
   `bin`). Then what is read without running it, off the async threads:
   the install scripts it would have run (the packages it brings with
   npm's `hasInstallScript`) and what its code reads, set here, that it
   would not get (`environment::names_read`: outside the kept set, or a
   credential), with what the scan missed (a limit, links, no code found)
   said in the question, not taken for "reads nothing".
5. **Ask the owner: start it?** An Allow/Block question through
   `Session::ask` with `UI_TIMEOUT` (300 s), as the gate asks:
   - title: `Start the MCP server <package>@<version> as <name>? (asked by
     the agent)`;
   - details: what the agent said it needs (`find_plugin`'s `need`), the
     candidate's fields, the args, the install scripts it would have run,
     what its code reads, and two lines: "Its tools are not known until it
     starts: you will be asked again once they are." and "It runs with your
     rights: this is not a sandbox.";
   - three outcomes, as `host_approval`'s: **Allow** goes on; **Block**
     refuses and remembers the package (stage 1); **unanswered** (nobody
     there, timed out, dismissed) refuses without remembering it, with a
     Warning notice, and the result tells the agent nobody answered.
6. **Start it and list its tools.** Started as a manifest's server is
   (`run::without`: the kept environment, `env -u`), owned by `Loaded`
   (below); `initialize` and `tools/list` within 30 seconds each. The list
   is judged as `check`'s MCP step judges a manifest server's: no tools, a
   prefixed name a model cannot take, or a tool listed twice stops the
   process and refuses it. A start that fails is refused with yoagent's
   words for it (its exit status and last stderr, the wrapper the kept
   environment starts it through not named), an Error notice. The list is
   fixed from here: a `list_changed` from the server is ignored.
7. **Ask the owner: keep it?** A second Allow/Block question, `Keep the
   MCP server <name>? It offers: <tools>`, the details each tool's
   description (its own claim) and parameters. Allow adds the tools to
   `Loaded`, and the result names them. Block stops it and remembers the
   package; unanswered stops it without remembering (both a Warning, as
   at the first question). The process the owner allows is the one that
   runs: there is no second start.

Every refusal (a malformed call's too) is the tool's result, marked
`is_error`, and a Warning notice to the owner ("The agent's plugin `x`
was refused: …"), so a
refused plugin is shown never loading. A refusal with a fix the owner can
make (`min_age_days`, `max_loads`, `max_attempts`, a name in use) has a
row in `hints.rs`, and so in `failures.md`. The server failing after the
first Allow (a start that fails, a bad tool list) is an Error notice; the
owner's Block or silence at the second question, or a run stopped
meanwhile, a Warning.

### How the tools reach the agent

yoagent asks for tools once per run (`ToolSource`, "per run, not per
turn"): a tool that appears mid-run is first offered on the next run.
Loading is mid-run by nature.

**Decided (2026-10-25): yoagent asks again at a turn boundary**, in a
yoagent PR of its own:
- `ToolSource` gains `generation() -> u64` (default 0: a source that never
  changes need not say so).
- Before each request after the first, once every tool call of the last
  turn has returned (so no call the model made is stranded), the loop
  compares each source's generation with the one it read its tools at. A
  source whose generation moved is asked again, and only that one; its
  tools replace its previous ones, a shorter list too (a server that
  died), and the collision rule is applied again over the whole list.
- This happens before `before_model`, so extensions see the new list (the
  gate, `AllowedCheck`). Extensions' per-run tools (pi's offer, for one)
  are not asked again mid-run.
- The tool list is part of the cached prompt, so a change rewrites the
  cache once per load: that request's usage counts against the run's
  budget, as any request's does.
- yobuild then depends on the yoagent release that brings it.

The other way, (b), needed no yoagent change: the run that loads ends and
yobuild starts the next one. It splits a task into two runs (budgets and
`report` twice, and `--once` could not use it), so it was not taken.

The loaded tools come from a `ToolSource` yobuild installs, `Loaded`: the
servers loaded this session, their tools and a generation, bumped on each
load and each server that stops. On a name collision yoagent keeps the
agent's own tools, then earlier sources, then extensions' tools, so a
loaded tool loses to the built-ins and the manifest's servers and wins
over a pi, DSH or rutis plugin's tool of the same name. Step 2's naming
makes the first two impossible; the third is rare (a plugin's tool named
`x__y`), and there the loaded tool is the one offered (yoagent logs the
plugin's it dropped). So every tool `Loaded` holds is offered: there is
nothing to check at `before_model`. (The first draft had `Loaded` check
that its tools were offered; built, it was seen that no case leaves one
out.)

### The gate and the loaded tools

A loaded server's tools all **ask**, call by call: the owner's Allow on
the load is not an `allow` list, and the descriptions a server gives its
tools are its own claims (MCP's `readOnlyHint` included). The gate asks
every tool in `Loaded` by membership, whatever its name, not by a name it
does not know, before any rule that could free it (`Gate::with_asked`),
with a question of its own: "Run `x__y`, a tool of a server loaded this
session?". A stopped server's tools stay asked: freed by name, a
plugin's tool of that name could then run unasked. (A call to one after
its server stopped is asked, then not found: the gate judges before
yoagent looks the tool up.) The gate is what makes them ask, so `run`
refuses `[extend]` without it, as the manifest does with `gate = "off"`.

### Lifetime

- `Loaded` owns the processes; `run` holds it for the session and stops
  them at its end (`Loaded::close`), however it ended: the driver
  returning, Ctrl+C, a failed start, the terminal UI stopping (each child
  is also `kill_on_drop`). A server the owner does not keep, or that fails
  to start, is stopped at once.
- A reset keeps them: they are the session's, as its plugins are.
- A server that stops mid-session, during a call or while idle: yoagent
  0.25.15's exit watch (`McpTransport::exit_watch`, taken from the stdio
  transport before the client is built, so reading it never waits behind
  a call in flight, which holds the `tokio::Mutex` yobuild keeps the
  client in) says it has gone and why; a transport without one is
  refused at the start (yoagent's stdio always gives one). `Loaded` looks
  at every running server's watch whenever yoagent asks it for its
  generation (each turn) or its tools, and whenever the watcher's tick
  asks what stopped (every 250 ms): one newly gone leaves the list and
  moves the generation at once, so its tools are gone from the next
  turn. Once it has been seen gone for 0.6 s — yoagent's text settles
  within 0.5 s, the last of its stderr read — the tick says so, an Error
  notice ("The loaded MCP server `x` stopped: it exited with …; last
  stderr: …", the tail's end kept when cut; also "it closed its output
  (its process is still running)", "it has gone (exit status unknown:
  …)"), with the longest reason read (yoagent answers without the tail
  while a call holds the child), and marks it stopped: about a second
  after a death while idle. It still counts toward `max_loads`, and is
  not loaded again this session.
  - At the session's end, every server seen gone is marked stopped in
    the block (`stopped_at_end`), those seen within 0.6 s waited for —
    but after a Ctrl+C one first seen after it, or ended by SIGINT, is
    not: a Ctrl+C typed in the terminal reaches the servers too (they
    share yobuild's process group; a group of their own is asked of
    yoagent).
  - One that dies while the owner is asked to keep it is refused then
    (an Error notice), never announced as loaded.
  - Not seen: a server whose process lives on but no longer reads its
    input (every call "Write error: Broken pipe"); asked of yoagent, to
    count a failed write as closed.
  - Before 0.25.15 yobuild wrapped the transport and saw a death only at
    the next call.
- A run aborted mid-load stops the load where it is: npm killed (and no
  later npm call started), the set's `.partial-` folder removed as
  `install` removes one, an open question withdrawn, a started process
  stopped. Nothing is remembered, but the call counted toward
  `max_attempts`; no load is left "in progress".
- Each step has its own limit (15 s a source, 30 s npm's search, 5 min an npm call, 30 s to
  start and list, `UI_TIMEOUT` a question), and all of them sit inside the
  run's `max_minutes`.

### The manifest

Off unless the manifest says so:

```toml
[extend]
kinds        = ["mcp"]   # what may be found and loaded; stage 1: "mcp" only
max_loads    = 2         # servers kept per session: 1 to 5
max_attempts = 4         # load_plugin calls per session: 1 to 10
min_age_days = 7         # whole days, 0 to 365: newer versions are not taken
```

- A table of its own, not `[builtins]`: it has settings of its own and
  later more kinds. `deny_unknown_fields`; integers only; each bound
  refused with its place (`file:line:col`), as every manifest error is.
- `[extend]` alone turns both tools on with these defaults; `kinds`, when
  given, is not empty. A kind other than `"mcp"` is refused when the
  manifest is read, naming the step that brings it.
- `min_age_days`: a package taken over and republished is most often
  caught within days; a week is a cheap guard. 0 turns it off.
- With `gate = "off"` the loaded tools would run unasked, so `[extend]` is
  refused with it. With `tools = "read-only"` it is allowed: the loaded
  tools ask anyway.
- `find_plugin` and `load_plugin` are reserved names: `Tools::check`
  refuses another tool of either name, and a plugin's `allow` naming one
  is refused, as `ask_owner` is.
- `--locked` is unaffected: nothing loaded goes into the lock.
- `check` lists both tools as allowed and notes that the agent may load
  MCP servers, up to `max_loads`, with the owner's approval each time.
  (Until `load_plugin` was built, in PR 4, it listed `find_plugin` alone.)
  `check --json` gains the agent's `extend` (`kinds`, `maxLoads`,
  `maxAttempts`, `minAgeDays`), and its report version becomes 4.

### What stays the same

- **The kept environment.** A loaded server starts as a manifest's does,
  with the kept environment and no `env` table: no credential, no other
  variable beyond the kept set.
- **Not a sandbox.** A loaded server runs with the owner's rights, like any
  other. The owner's approval and `check` decide whether it runs, not what
  it can reach once it does. The first question says so.
- **Budgets and limits.** A loaded tool's call counts as any call does,
  and `facts` counts it. `measure` does not time it: `measure` times
  plugin handlers, and an MCP call is none.

### When the session ends

`main.rs` writes, to stderr, after the session ends however it ended
(Ctrl+C too, when no notice could still reach a frontend), once the
plugin runtimes have stopped — the terminal UI may hold the terminal
until then (`Ended::loaded`; when `run` itself fails, `run` writes it):

```text
Loaded this session: @eslint/mcp@0.3.15 as eslint (tools: lint-files). To keep it, add to agent.toml:

[[mcp]]
name = "eslint"
npm  = "@eslint/mcp@0.3.15"
# allow = ["lint-files"]   # tools that would run without asking

The next run resolves its dependencies again (the lock's `mcp` set, without the age limit) and writes agent.lock, which --locked needs.
```

with its `args` when it had any, the manifest's and its lock's own names
(`handoff.toml`, `handoff.lock`), and a server that stopped marked so
(`tools: lint-files, stopped this session: …`). The same text is an Info
notice the moment the owner keeps the server ("The agent loaded … for
this session."): once the session ends, no frontend is left to hear
one. Nothing is written by yobuild.

### Later steps

- **Stage 2: rutis, pi and DSH plugins.** npm search finds them by their
  keywords (`pi-package`, `dsh-plugin`). Loading them is harder:
  - a rutis plugin runs in a Node runtime of its own package, and
    `PluginHost`'s runtimes are fixed at `start`; it needs a host that adds
    a runtime while running;
  - every pi extension runs in one runtime, as every DSH plugin does:
    loading one changes a runtime other plugins share, and its grants must
    match theirs (§12).

  Proposed below: "Stage 2: pi and DSH plugins".
- **Remote MCP servers**, and a loaded server handed a credential, once
  there is a way for the owner to grant one at the question.

### Tests

As built (PR 4), each named where it lives:

- The sources are stubbed: an axum server in the tests answers the MCP
  registry (`YOBUILD_MCP_REGISTRY`) and npm's registry
  (`npm_config_registry`, a kept setting), so no test reaches the
  network. For `load_plugin` the stub also serves packages packed in the
  test with `npm pack` (a small Node MCP server), so the install is the
  real `npm install --before` and `npm ci`.
- `tests/load_plugin_test.rs`, the binary through its page:
  - a candidate allowed twice under a given `name`, its tool offered at the
    next turn and asking with the loaded tool's own question; a call that
    kills it (an Error notice, its tools not offered at the next turn); the
    block on stderr after Ctrl+C, the server marked stopped; no
    `agent.lock`; its `postinstall` never run, and said in the question;
    its dependency taken at the version a week old, not its day-old newest;
    every server it started gone;
  - unanswered at either question not remembered, Block at either one
    remembered, a server with no tools refused after the first Allow
    (an Error notice), each server it started stopped;
  - a run aborted while npm installs (npm stopped, no `.partial-` folder
    left) and one aborted while the owner is asked (the question
    withdrawn), neither leaving a load in progress.
- Unit tests (`run/load/tests.rs`): every refusal before the install
  (shapes, not a candidate, not loadable, another version, loaded already
  and stopped, blocked, `max_attempts`, `max_loads`, two loads at once,
  `--once`, a name in use or bad, `args` guarded, a required environment
  variable), the default name, the tool list's judgement, a path outside
  the folder, the block read back by the manifest, `Loaded` as a tool
  source and a server found gone (a stand-in watch; a real stdio server
  dying while idle, with no call made; one yobuild closes never said;
  the end of the session and a Ctrl+C; the longest reason), the
  words of a start that fails, several programs, what the scan reads and
  missed. The gate's `with_asked`, and the hints, in their own tests.
- `find_plugin_test` (PR 3): one source down and both down, what `check`
  lists (both tools allowed).
- Not tested end to end: a publish time that cannot be read (unit tests
  of `find_plugin`), `min_age_days = 0` (no `--before`), a reset with a
  server loaded.
- A scripted demo in the gallery (PR 5, below), run in CI with both
  questions answered over the page's WebSocket.

### The demo

A code reviewer that starts without ESLint (`tools = "read-only"`,
`[extend]`). Asked to review a project whose ESLint config it reads, it calls `find_plugin`
(in its own words: tried live on 2026-10-26, the need "eslint" finds
ESLint's server and "lint TypeScript" does not — the server's name and
description hold neither word as written), proposes `@eslint/mcp`, the owner allows starting it
and then keeping it, and it lints with `eslint__lint-files`, each call
asked and allowed. Run once live (cost in its README), and scripted in CI
against the live sources (decided 2026-10-26): its reply files name no
version (`load_plugin` takes the one offered) and expect only what holds
as the sources change; npm's registry down fails the job (`@eslint/mcp`
is found by npm's search, and installed from npm; the MCP registry down
alone is a Warning), said so in its README. A second reply file has the agent propose a package
`find_plugin` did not return (the task names it, as a friend's
recommendation would, or a page the agent read):
refused before anything installs, never offered (decided 2026-10-27: a
real server that `check` refuses, as first planned here, cannot be
counted on among live sources; that refusal is `load_plugin_test`'s,
against the stub). A third has the owner block the load, and the agent
reports what it could not do. Under `--once` `load_plugin` is refused, so
`gallery_test` runs these reply files on the page, answering over its
WebSocket.

### Decisions

1. *How the tools reach the agent.* Decided (2026-10-25): (a), a yoagent
   change. (b) would work without one, at the cost of a second run.
2. *Sources.* Decided (2026-10-25): the MCP registry and npm search, both.
   The MCP registry lists servers others have named as such; npm search
   finds more of them, and the same search finds pi and DSH packages later.
3. *`min_age_days`.* Decided (2026-10-25): it defaults to 7, and the owner
   may set 0.
4. *Loaded tools.* Decided (2026-10-25): they all ask, call by call. There
   is no option at the question to let them run unasked.
5. *When the owner is asked.* Decided (2026-10-25, after the review of
   #76): before any of the package's code runs, and again once its tools
   are known.

### Order of PRs

1. this design;
2. yoagent: tool sources asked again at a turn boundary after a change
   (decision 1);
3. `[extend]` in the manifest, `find_plugin` with its sources, and the
   stub registry in the tests;
4. `load_plugin`: the session set, the two questions, `Loaded` and its
   lifetime, the end-of-session block, the hints;
   - between 4 and 5, `find_plugin`'s quality (#79: only servers
     offered, `version` optional) and yoagent 0.25.15's exit watch in
     place of yobuild's own transport wrapper;
5. the demo agent, the skill (`manifest.md`, `parts.md`, `failures.md`),
   the guide, `CLAUDE.md` and the roadmap.

### Stage 2: pi and DSH plugins (proposed 2026-10-27)

**What it adds.** With `kinds = ["mcp", "pi", "dsh"]`, the agent can find
and load a pi package or a DSH package as it loads an MCP server. The two
questions, the limits, the age rule and the refusals stay as stage 1 has
them. What differs: how the plugin is installed and run, what it may
reach once it runs, and the end-of-session block.

**Why it is harder than an MCP server.** An MCP server is a process that
answers calls to its own tools and reaches nothing else. A plugin is more:

- **It runs inside a runtime.** Every pi extension of a manifest runs in
  the `pi` runtime, under one pi adapter row, and every DSH plugin runs in
  the `dsh` runtime over one Cordis. Loading one there would restart or
  reconfigure what other plugins share, and its environment and grants
  would have to match theirs (§12, "One rule for both ecosystems").
- **Its handler gets every hook.** `before_tool` can deny or rewrite any
  call, `after_tool` can rewrite any result, `before_model` can add a note
  to the turn or stop the run, `on_input` can reject the owner's input,
  `on_event` sees the run's events. A pi extension reaches these through
  pi's events (`tool_call`, `tool_result`, `before_agent_start`, …).
- **Its runtime gets the shared services.** Every runtime gets `SHARED`
  (`yoagent`, `frontend`, `ui`, `ask_owner`, the `host_…` services), and
  none of `frontend`, `ui` or `ask_owner` checks who calls. A plugin that
  injects `frontend` reads every message and can send a prompt, a steer or
  an answer — its own Allow at the second question among them, since it
  runs by then — and add a UI module to the page.
- **Its tools come with each run.** A run takes a snapshot of the
  registered handlers when it starts (yoagent-rutis's `start_run`).
- **`PluginHost`'s runtimes are fixed at `start`,** and it routes rows by
  name: a second runtime loading `pi-extensions-adapter.ts`, or a DSH
  package the manifest's DSH plugins load too, is refused as "loaded by
  runtime `pi` already".

**Proposed answers.**

1. **A runtime of its own for each loaded plugin.** A loaded pi package
   gets a new pi runtime, `loaded-<name>`, on the pi adapter's folder, with
   one row: the pi adapter loading only its extensions. A loaded DSH
   package gets a new DSH runtime on a `dsh` set of its own (the DSH base
   plus that package), with the DSH tools adapter row and the package's
   rows. Nothing the manifest loaded is restarted or reconfigured, and a
   shared runtime's environment and grant rules never come up.
   `PluginHost` gains `add_runtime` and `remove_runtime`, both while
   running:
   - `add_runtime` gives it what every runtime loses (`withhold`,
     `inherit`: the kept set alone, no `keep`, no `env` grant) and its own
     service catalog (point 3), and waits until it runs: one that does not
     start is said, with why, and removed;
   - a loaded runtime's rows are routed to it only: the loader resolves
     a row by its name alone, so the host gives it `<runtime>::<name>`,
     and its rows never meet the manifest's; a row naming no runtime
     never goes to it;
   - `remove_runtime` unloads its rows, then stops its process.

   Built (PR 3, `host_test`): the same file loaded in `ui` and in an
   added runtime, two processes; the added runtime keeps what every
   runtime keeps and nothing `keep` gave another; one given no service
   never reaches `yoagent`; its rows reload and unload; a row naming no
   runtime stays out of it; removed, its handler and its process gone, the
   others' kept; one that cannot start said at once. Found there:
   - a row whose injected service is left to its runtime's own Cordis
     reads as running to the host (`load` is `Ok`) while it never starts,
     so `load_plugin` learns that a plugin started from its handler, never
     from `load`;
   - the loader keeps a row's resolution by its name, the runtime's
     among it: an added runtime's name is used once a session (unless it
     never started), and a name the loader saw in one of the host's
     runtimes — unloaded since, in a batch that failed, or with no runtime
     named — is not loaded in another;
   - `add_runtime` is cancellable: dropped while it waits (the run
     cancelled), it stops the runtime in a task of its own;
   - what a row provides is published to every runtime by name, whatever
     its catalog shares. A row of an added runtime that declares a service
     is refused; one an SDK plugin provides without declaring
     (`ctx.provide`) is not seen, so a loaded plugin could take a name
     nobody provides yet (a manifest plugin restarting, say). Asked of
     yoagent-rutis and rutis with point 2: a runtime whose rows publish
     nothing.

   The adapters' runtime set is installed when `[extend]` names `pi` or
   `dsh`, as when the manifest has such a plugin. The cost is memory: a
   pi runtime was measured at 335 MB and a DSH one at 157 MB with fixtures
   (§9); with real plugins, about 800 MB (pi-lens) and up to about 1 GB
   (univer) (`parts.md`). `max_loads` (at most 5) bounds how many.
2. **Tools only, decided by the runtime.** yoagent-rutis is told, before
   any row of a loaded runtime loads, that every handler registered over
   that runtime's connection is restricted — from a row or from none (a
   Cordis child, code without a row scope: owned by "language plugin
   (session …)"), whatever its owner says. An allow-list in effect: only
   handlers from runtimes not marked get hooks, so a handler cannot slip
   through by registering from no row, or before yobuild knows its row.
   A restricted handler's run calls only:
   - `tools`, offering only the names the owner saw at the second
     question (below);
   - `before_tool` for calls to its own tools — the pi adapter runs a call
     only after its own `before_tool` has prepared, validated and judged
     the arguments (`judged`; without it every call is "not run: the
     arguments … changed after pi's policies judged them") — but the gate
     goes first, and a deny or rewrite reaches only its own calls;
   - `call_tool`, and `finish`, which ends the adapters' per-run state.

   None of `after_tool`, `before_model`, `on_input`, `on_stop`,
   `on_event`. So:
   - a pi extension's own `tool_call` policies see only its own tools'
     calls, and its `tool_result`, `before_agent_start` and observers are
     not called;
   - a DSH plugin's system-prompt guidance (the DSH adapter's
     `before_model` note) is left out;
   - a plugin that needs those hooks works in part, or not at all. The
     second question says "only its tools are used, not its hooks (put it
     in the manifest for those)".
3. **Its runtime gets `yoagent` only.** Its resolver is given a catalog of
   its own, which shares `yoagent` and nothing else: no `frontend`, `ui`,
   `ask_owner`, `host_…`, and none of the pi or DSH host rows
   (`pi-host-ui`, `pi-host-approval`, `pi-host-session`, `dsh-host.json`'s
   rows). The pi adapter there runs in print mode (`lookupUi`: no `ui`).
   A second line, in case a name reaches it anyway: `frontend`, `ui` and
   `ask_owner` refuse a caller row `loaded:…`.
4. **No grants, no config, no host services.** A loaded plugin's rows are
   `loaded:<name>[:<row>]`, never `plugin:<name>` nor `dsh-host`, so
   `Host::caller` finds no grant for them; the pi host's rows are matched
   by exact id (`pi_row`: `pi-host-session`, `pi-host-approval`), which a
   loaded runtime neither has nor can take. A pi extension's fixed answers
   apply (`ctx.isProjectTrusted()` false, no model), so a package that
   needs `trust` — pi-lens — finds nothing loaded and belongs in the
   manifest. A plugin that needs `config` to start fails to start: an
   Error refusal in its own words. No DSH host services, whatever `[dsh]
   host_services` says: the owner turned those on (DSH's files,
   credentials, storage) for the plugins they named, and §12 keeps them
   from any grant. Before the second question `probe_services` checks the
   loaded rows: one waiting for a service nobody provides is refused, the
   services named, its runtime removed. No `DSH_HOME` is passed (a private
   one only if a DSH runtime turns out to need one to start, never the
   manifest's).
5. **Prefixed names, the list frozen.** Its tools reach the agent as
   `<name>__<tool>` (the plugin called with its own names), as a loaded
   server's do, so they never meet another tool's name. Only the tools
   listed at the second question are offered: one the plugin registers
   later is not (stage 1's rule, `Loaded::tools`), and `Loaded::asks`
   matches the names the owner saw. A description that names its tool
   unprefixed leaves the model to match it, as for a server.
6. **How its tools reach the agent mid-run.** A run's snapshot is taken
   at its start, and yoagent's `ToolSource` (`tools()`, `generation()`)
   knows no run. Proposed: the bridge offers the restricted handlers'
   tools through a source of its own that knows the run in progress (set
   by `start_run`, cleared by `finish`): when a restricted handler
   registers mid-run, the source calls its `tools` for that run, moves its
   generation, and the run's hooks take the handler in for `before_tool`,
   `call_tool` and `finish`. Outside a run it offers nothing. This and
   point 2 are one yoagent-rutis change, asked of its owner with this
   design. If it cannot be done, the fallback is the next run: the
   handler joins the next snapshot, and the result of `load_plugin` says
   its tools come with the next task.
7. **Installed as its set says.** `install_one` resolves a one-entry set
   of the plugin's kind: `pi` with `peers_of` and `pis_own` and
   `legacy-peer-deps`, as `install` resolves the `pi` set; `dsh` from the
   base's lock, a pin the base holds at another version refused before npm
   runs ("pinned by the DSH base at X": once `load_plugin` takes `dsh`, PR
   6, the refusal says so and the agent sees it), the DSH files in the
   hash. `--before` applies to what npm resolves anew; the base's
   packages stay as locked, checked after resolving. For `dsh` npm's own
   peer resolution is off and yobuild adds the pin's peers that the base
   does not hold and that are not optional, as for `pi`: under `--before`
   npm sees none of the base's versions published since, so a plugin
   peering on one (its Cordis, say, the base updated within the age rule)
   would fail ("Could not resolve dependency") though the locked base
   satisfies it. The install scripts said are the pin's, the base's own
   apart (said once for the agent, as `check` does).

   Built (PR 4, `install_one_test` against the stub npm registry): a pi
   package's peer installed at the version the age rule allows, pi's own
   never asked about, its dependency's install script not run and said; a
   DSH package beside the base's own, which was published a day ago and
   stays as locked, peered on by the pin (its install script not the
   pin's), the pin's other peer added and its optional one never asked
   about, the adapter's files copied in; the base's own package at its
   version taken; a pin of the base's at another version, and a base entry
   that is not a version, refused before npm is asked. Seen by breaking it
   (2026-10-28): without the base's lock as the start npm refuses the
   base's package under `--before` ("No matching version found … with a
   date before …"), and with npm's own peer resolution the pin's peer on
   it fails as above.
8. **Found by npm.** `find_plugin` with `kind = "pi"` searches npm for
   `keywords:pi-package <need>`, and with `"dsh"` for `keywords:dsh-plugin
   <need>`; the MCP registry is not asked. A pi package is kept when `npm
   view` shows a `pi` field or the keyword `pi-package` (pi also resolves
   conventional folders, which `npm view` cannot see: the install and
   `resolve-package.mjs` decide then, refused before the first question
   when it holds no extension); a DSH package when it has a `dsh` field or
   the keyword `dsh-plugin` (`inspect-package.mjs` reads a package with no
   bundle as one row). Packages with neither mark — DSH's own
   `dsh-tool-ask-user` has no keyword — are not found: an owner who wants
   one names it in the manifest.

   Built (PR 5): `find_plugin`'s `kind`, the manifest taking `"pi"` and
   `"dsh"` (`rutis` still refused); `load_plugin` refuses such a
   candidate until PR 6 ("loads MCP servers only so far"; a package
   found as an MCP server too is loaded as that), and `check`'s agent
   note says so; without `"mcp"` among the kinds there is no
   `load_plugin`, and `YOBUILD_MCP_REGISTRY` is not read. Tested against the stub registries
   (`find_plugin_test`: each kind's keyword searched, no MCP registry, a
   package marked by its field or by the keyword alone kept, one marked
   by neither left out and counted, npm failing said as the one source).
   npm's answer on 2026-10-28: 250 or more packages for each keyword (a
   search's most), many of them themes, chat bridges and model
   providers rather than tools.

   **The survey** (2026-10-28, run by the owner: `yobuild check` on a
   one-plugin manifest per package, the newest version 7 or more days
   old, no grants, no config, no DSH host services — what a loaded
   plugin gets, though `check` runs it in the shared runtime with every
   hook):

   | Package | Verdict | Tools |
   |---|---|---|
   | pi `pi-web-access@0.44.0` | loads (asks `ctx.model`: undefined) | `web_search`, `fetch_content`, `get_search_content`, `source_check` |
   | pi `@juicesharp/rpiv-todo@2.13.9` | loads | `todo` |
   | pi `pi-memory@0.4.2` | loads | 7 `memory_*` tools, `scratchpad` |
   | pi `@hypabolic/pi-hypa@1.2.0` | loads | `hypa_find`, `hypa_grep`, `hypa_ls`, `hypa_read`, `hypa_shell` |
   | pi `@juicesharp/rpiv-ask-user-question@2.13.9` | loads; asks through pi's dialogs, which a loaded runtime lacks (print mode) | `ask_user_question` |
   | pi `pi-simplify@0.2.3` | loads, offers no tool (commands only) | — |
   | pi `pi-background-tasks@2.6.10` | fails in `session_start`: `ctx.modelRegistry.getProvider()` is not available | — |
   | DSH `dsh-univer-office@0.3.7` | loads; its useful tools need `workspace`, which a loaded plugin is not granted | 14 `univer_*` |
   | DSH `dsh-image-gen@0.9.2` | waits for `webServer`, a service the host lacks | — |
   | DSH `dsh-ops@0.4.1` | refused: a row `disabled`, which yobuild does not apply | — |
   | DSH `dsh-free-search@0.9.11` | refused: the base pins 0.8.1 | — |
   | DSH `dsh-builtin-browser@0.4.14`, `dsh-mcp-connector@0.2.76`, `dsh-remote@0.8.49` | not resolved: they peer on DSH's web client packages (`dsh-client-ui-*`), whose own peers want another `dsh-agent` than the base's | — |

   So pi packages mostly load, and the demo can be one (PR 7): a tool
   the agent lacks and finds, such as `pi-web-access`'s web search or
   `pi-memory`'s notes — not one that asks the owner through pi's
   dialogs, nor `hypa_shell`, a shell. DSH packages mostly need DSH's web
   GUI, its host services or a grant: none surveyed is a demo as a
   loaded plugin. The web-client ones would resolve through
   `install_one` (npm's peer resolution off), only to need a GUI yobuild
   does not have.
9. **rutis plugins later.** rutis's SDK carries the keyword `rutis-plugin`,
   but no published plugin does yet. They come when some do.

**The questions.** Question 1 names the kind: `Start the pi package
<package>@<version> as <name>? (asked by the agent)`, with what stage 1
shows, plus "it runs in a runtime of its own, with only its tools used".
Its default name is `default_name`'s (`pi-lens` stays `pi-lens`): the
agent may pass a shorter one. Between the two questions: the runtime
added, its rows loaded, its handler waited for (by its own name, a fresh
`loaded-<name>-<uuid>` per load, so no manifest handler's name is taken;
a wait scoped to its runtime, so a manifest row stopped earlier refuses
nothing), the DSH rows probed, its tools read and judged by
`tools_problem`. All of it under the call's cancellation, as the install
is (`StopOnDrop`): a run aborted or a call dropped there removes the
runtime. Question 2: `Keep the pi package <name>? It offers:
<name>__<tool>, …`. Block or silence removes the runtime.

**When it stops.** A loaded runtime that dies, or a loaded row that fails,
is said as a loaded server that dies is ("The loaded pi package `x`
stopped: …", an Error), its tools gone at the next turn. `PluginHost::stopped`
reports rows and runtimes, so the tick's "A plugin stopped: …" leaves out
`loaded-…` runtimes and `loaded:` rows, which `Loaded` says instead, once.
A runtime removed after a Block is not said as stopped.

**The end-of-session block.** `[[plugin]]` with `pi = "pi-lens@4.4.3"` (or
`dsh = …`) and `name`, its tools' own names in the commented `allow`, and
the lock line naming its set ("the lock's `pi` set"). In the manifest it
runs as any plugin does: in the shared runtime, with its hooks, its tools
unprefixed. The block says so, and when the manifest's pi or DSH plugins
have grants it cannot be pasted as it is: it says which (`model`, `env`,
the DSH grants must match theirs, §12).

**`check`.** `[extend]` with `pi`/`dsh` lifts the manifest's refusal
(`"pi"`/`"dsh"` "come in a later step") and is said in `check`'s agent
note and `agent.extend`.

**The demo.** Chosen from the packages surveyed in PR 5 (which ones load
with no grants, no config and no host services). Not pi-lens (it needs
`trust`) nor `dsh-free-search` (the DSH base pins it at 0.8.1, and the
7-day rule takes a 0.9 version, which the base refuses). Three reply
files, as stage 1: allowed, not offered, blocked.

**Decisions for review.**

1. A runtime of its own per loaded plugin, or the shared `pi`/`dsh`
   runtime when no manifest plugin uses it?
2. Tools only (with its own `before_tool` for pi), or every hook, shown at
   the second question?
3. `yoagent` only for its runtime?
4. Mid-run through the bridge's run-aware source, or at the next run?
5. Prefixed names, or the plugin's own, refused when they clash?
6. pi and DSH now, rutis when plugins exist?

**Order of PRs.**

1. this design;
2. yoagent-rutis: runtimes marked restricted (point 2), the run-aware
   source (point 6) — asked of its owner, released, then taken here;
3. `PluginHost::add_runtime` / `remove_runtime`: per-runtime routes, the
   kept environment and a catalog of its own; host tests (a second pi
   adapter loads; a loaded row's handler gets no hook; `frontend` not
   reachable);
4. `install_one` for the `pi` and `dsh` sets;
5. `find_plugin`'s `pi` and `dsh` kinds, with a survey of what loads;
6. `load_plugin` for both: the manifest's kinds, the adapters installed,
   `Loaded` holding runtimes beside servers, cancellation, the tick, the
   block, `check`, the hints;
7. the demo, the skill, the guide, `CLAUDE.md` and the roadmap.
