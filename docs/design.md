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

## 6. Safety model

Inherited from yoagent and the handoff work, and kept on by default:

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
  joins them (`run/joined.rs`, an `after_tool` extension) until it depends
  on a yoagent with the fix (yologdev/yoagent#347, all five providers);
  the second live run reported all three of ESLint's errors. Lessons:
  `check` loads plugins and lists their tools, it does not call them, and
  a `✓` says a tool ran, not that it worked — two of four candidates
  passed `check` and did not work. A gallery demo calls every tool its task
  needs, and a live run reads what came back.

**Order of PRs**: this design; then one PR per agent (web research, your
MCP server, the third), each with the skill fixes its build found.
