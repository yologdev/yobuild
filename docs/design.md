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
terminal UI and `--measure` are (Windows: open question 7).

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

Not covered, and said so: plugins run with the process's rights; the
environment they inherit is visible to them — except the model key
variables, which plugin runtimes, MCP servers and the adapters' helpers get
empty (design §12, "Step 4"; the agent's own `bash` keeps them), against
honest code calling a model unmetered, not hostile code, which can read a
key where the owner keeps it; the gate judges command
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

Two tools for a running agent: `find_plugin` (search + vet) and
`load_plugin` (install + hot-load through `PluginHost`). Both behind the
owner's approval (`ask_owner`) and the fail-closed rules. This is what
rutis was built for — software that adds its own plugins while it runs —
and the riskiest part of the plan, so it comes last.

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
repository and is installed by copying the folder (decided 2026-10-19) into `~/.claude/skills/` or a project's
`.claude/skills/` (the README says how); `yobuild skill install` comes
with M4's install (roadmap M4).

**A working `yobuild`.** Until M4, `yobuild` runs from a checkout of this
repository, set up once (`scripts/try-coding-agent.sh --setup-only`: the
adapters, the frontend's plugins), and is called by path
(`path/to/yobuild/target/debug/yobuild`, or `cargo run --manifest-path
path/to/yobuild/Cargo.toml -- …`) from the agent's own folder. `SKILL.md`
says so first.

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
of them), each in `gallery/<name>/`: the manifest, its prompt, its lock,
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
  reports every file clean (its own log said so). It took ESLint's MCP
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
| `keys` | names of credentials yoagent reads (step 4, proposed) | yes (every pi plugin or none) | yes (every DSH plugin or none) | yes (every plugin of its package or none) | those variables left in its runtime |

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
`failures.md` entry point at the grant.

### What a grant hands a plugin

- **Honour-system** — `trust`, network, installs, and also what goes around
  `exec` and `llm`: a plugin already has Node's whole power; it can spawn
  through `child_process`, or read a model key where the owner keeps it
  (the runtimes get the key variables empty: step 4, below; §6). A grant changes how honest code behaves (pi-lens installs
  language servers into `~/.pi-lens` and runs the project's code); nothing
  enforces it. Said by `check` and in §6.
- **Mediated** — calls through the host: `pi.exec` judged by the gate,
  `llm` charged to the budget, wake bounded, approvals naming the plugin.
  Metering holds only once plugin runtimes lose the provider key variables:
  step 4, below ("Step 4: plugins without the provider keys").
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
host's warnings. Not yet: `check` (no session watches there).

### Step 4: plugins without the provider keys (proposed 2026-10-21, amended the same day)

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
key: kept), reach a cloud machine's metadata endpoint, and on Linux read
`/proc/<yobuild's pid>/environ`. And the list holds only what yoagent
knows: `GITHUB_TOKEN`, `NPM_TOKEN` and the like still pass. It is the
honour-system kind of rule ("What a grant hands a plugin"): taking the
variables away stops a library from picking a key up by default, nothing
more. §6 and the skill say so in these words.

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
  every platform.

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
no `env`. `check`'s rutis plugin alone (`rutis_alone`) goes through
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
  in its arguments, URL or headers, sets it for that server.
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
`manifest.md` says how to pass one (a server's `env`, `keys`), and
`parts.md` which listed plugins lose a feature without one. `check` says
in an agent note which ones are withheld when a plugin or stdio server
would run. The owner is not told on every run: it is the default, not an
event. A plugin that fails for want of a key says so in its own words; no
hint can match them.

**Tests.**
- `cli_test`, the binary with `DEEPSEEK_API_KEY`, `OPENAI_API_KEY`, a
  custom `api_key_env` and an unrelated variable set, scripted: a rutis
  plugin, a pi extension, a DSH fixture file and a stdio MCP fixture, each
  recording which of those names it sees and how (unset, empty, a value).
  In `run` and in `check` together: the listed ones unset, the unrelated
  one kept, the server's `env` key handed over; in `check` alone: pi and
  DSH empty, rutis unset. Each with a `keys` grant on one: that runtime
  sees the granted key, the others do not. `check --json`: the notes on
  the entries and the agent note.
- `src/manifest/tests.rs`: `keys` refused with a name not on the list, a
  repeat, a list that differs between two pi plugins, two DSH plugins, or
  two rutis plugins of one package (two packages may differ); `keys` on a
  DSH plugin accepted with or without `workspace`. Unit: `KEY_VARIABLES`
  equals yoagent's list; `Manifest::key_variables` adds `api_key_env`; the
  per-process `-u` list leaves out what that process is handed.
- Host test: `withhold` removes a variable from every runtime, `keep`
  leaves it in one, `keep` for a runtime not started is an error.
- `tests/run_terminal_pty.py` and `tests/terminal_pty.py` with a key set
  (CI sets one), so the terminal UI is started through `env`.
- `src/run/plugins.rs`: a `keys`-only DSH grant plans no DSH host.

**History.** Proposed 2026-10-21 as *blanked* (set empty everywhere, true
removal waiting for rutis's `Launcher::env_remove`), AWS's and Cloudflare's
credentials left out, no key grant for pi; the review decided that the same
day (blank now, `bash` left out, no pi grant until one is wanted). Amended
before the code, asked by the yoagent session's owner and checked with the
prototype: removed through `env -u` (no rutis change), every credential
yoagent lists, the `keys` grant. yoagent-rutis 0.8.1 added
`ProbeOptions::env` for the alone probes, and yoagent 0.25.12
`api_key_variables()`. The stronger step is an allowlist — a runtime
started with `PATH`, `HOME`, `TMPDIR`, the locale, `TERM`, the proxy
settings and what is granted — which the same `env` gives (`env -i`)
without a rutis change: its own design later, the keep-list measured
against the gallery.

**Built** (`manifest::KEY_VARIABLES`, `Manifest::key_variables`,
`PluginHostBuilder::blank`, `run::blanked`, `run::keys_given`): in `run`,
in `check`'s rutis probe and its run together, for stdio MCP servers and
the helpers. The pi and DSH probes `check` runs alone wait for
yoagent-rutis's `env` argument.

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
