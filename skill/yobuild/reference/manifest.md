# The manifest (`agent.toml`)

One TOML file per agent, read strictly: an unknown key or a wrong value is
an error naming its line and column. Paths are relative to the manifest's
folder. The agent itself works in the folder `yobuild run` is started in.

## A whole manifest

```toml
[agent]
name   = "researcher"                 # one plain name (it names the scratch folder)
model  = "deepseek/deepseek-flash"    # provider/id; the key from the provider's variable
prompt = "prompts/researcher.md"      # optional: the system prompt ({cwd}, {scratch} filled in)

[limits]                              # all optional; yoagent's defaults: 50 turns, 1M tokens, 10 min
max_turns   = 80
max_tokens  = 4_000_000               # per run
max_minutes = 30
budget_usd  = 1.0                     # dollars per run (refused for a model with no price)
session_budget_usd = 2.0              # dollars across the session's runs

[[plugin]]                            # a pi package, pinned
pi    = "@juicesharp/rpiv-todo@2.12.0"
allow = ["todo"]                      # tools the gate lets run without asking

[[plugin]]                            # a DSH package, pinned
dsh    = "dsh-free-search@0.8.1"
config = { provider = "bing", disabledEngines = [], bingMarket = "en-US" }

[[plugin]]                            # a local rutis plugin (TypeScript, in its own npm package)
rutis   = "plugins/word_count.ts"
runtime = "node"
name    = "words"                     # optional: else the package's name or the file's stem

[[mcp]]                               # a stdio MCP server: the pinned package's bin, run
npm   = "@modelcontextprotocol/server-filesystem@<version>"
args  = ["."]
allow = ["read_text_file", "list_directory"]   # the server's own names; the agent sees files__read_text_file

[[mcp]]                               # an MCP server over HTTP
url     = "https://mcp.example.com/mcp"
headers = { Authorization = "Bearer ${EXAMPLE_MCP_TOKEN}" }

[builtins]
ask_owner = true                      # questions to the owner, without waiting
report    = "handoff"                 # or a list: ["Done", "Next"]
facts     = true                      # what changed (git), tokens, tool calls, after each run
gate      = "allow-list"              # the default; "off" only in a sandbox
scratch   = true                      # a folder the agent may use freely
tools     = "coding"                  # yoagent's six (the default), or "read-only"

[dsh]
host_services = false                 # DSH's own services, for DSH plugins that need them

[frontend]                            # nothing set: the web page
web      = true
phone    = false                      # the web page on the local network, with a link
terminal = false                      # the terminal UI instead of the web page
```

## The keys

**`[agent]`**
- `name`: one plain name (no `/`, not `.` or `..`): it names the scratch
  folder.
- `model`: `provider/id`. Providers and their key variables: `anthropic`
  (`ANTHROPIC_API_KEY`), `openai` (`OPENAI_API_KEY`), `deepseek`
  (`DEEPSEEK_API_KEY`), `google` (`GEMINI_API_KEY` or `GOOGLE_API_KEY`),
  `groq`, `mistral`, `xai`, `qwen` (`DASHSCOPE_API_KEY`), `zai`,
  `minimax`, `meta`; `ollama` and `local` take no key (`local` needs
  `base_url`). Any other provider needs `base_url` (https, or http on this
  machine) and `api_key_env` (the variable's name).
- `prompt`: a file. `{cwd}` and `{scratch}` are filled in at start
  (`{scratch}` needs `[builtins] scratch = true`). Without one, a plain
  prompt naming the agent and its folder. With `report`, its instruction
  is added after the prompt.

**`[limits]`**: each more than zero; `budget_usd` and
`session_budget_usd` need a model with a price (`deepseek`, `anthropic`,
`openai`, … are priced; a local one is not).

**`[[plugin]]`** names exactly one of:
- `pi = "<package>@<version>"` or a path (`./ext.ts`, a folder): a pi
  extension or package. No `config`.
- `dsh = "<package>@<version>"` or a path: a DeepSeek Harness plugin.
  `config` is the plugin's own. DSH files and DSH packages cannot be in
  one manifest yet; a DSH package the DSH base already pins must be at the
  base's version.
- `rutis = "<path>"` with `runtime = "node"`: a local plugin. Its folder's
  nearest `package.json` must have `@arcships/rutis-runtime` installed.

And optionally:
- `name`: unique among plugins and servers.
- `allow = [...]`: the plugin's tools (as it names them) the gate lets run
  without asking. Not `bash`, `write_file`, `write`, `edit_file`, `edit`
  (judged call by call), not `ask_owner`, not a name with `__`. Freed only
  when that plugin offers it (`yobuild check` says when another does).
- `policy = true`: this pi or rutis plugin is the approval policy: while
  no plugin judging calls is up, every call is denied. Usually with
  `gate = "off"`. Not on a DSH plugin.
- `config = { ... }`: a table; every string in it may hold `${NAME}`
  references, and a key named like a secret (`token`, `key`, `password`, …)
  must hold only references.

**`[[mcp]]`** names exactly one of:
- `npm = "<package>@<version>"` (or a path to a program): a stdio server,
  with `args`, `env` (secrets as `${NAME}`), and `bin` when the package
  has several.
- `url = "https://…"`: an HTTP server, with `headers` (secrets as
  `${NAME}`).

And optionally `name` (its tools are named `<name>__<tool>`; no `__` in
the name) and `allow` (the server's own tool names).

**Pins.** Every package is `name@1.2.3` (exact; `1.2.3-beta.1` too) or a
local path (`./`, `../`, `/`). Tags, ranges, `git`, URLs and `file:` are
refused. Two entries of one kind (pi, DSH, stdio servers) may not pin one
package at two versions.

**`[builtins]`**: `ask_owner`, `facts`, `scratch` are `true`/`false`
(default `false`). `report` is `"handoff"` (Done / Waiting on you /
Decisions / Next) or a list of section names. `gate` is `"allow-list"`
(default: reading is free, `bash` and writes are judged, everything else
asks) or `"off"`. `tools` is `"coding"` (default: bash, read, write,
edit, list, search) or `"read-only"` (read, list, search, inside the
agent's folder).

**`[frontend]`**: `web`, `phone` (needs `web`), `terminal` (not with
`web`). Nothing set: the web page. `yobuild run --web ADDR` sets where it
listens.

## Patterns

- **An agent that reports back** (the handoff example): `ask_owner`,
  `report = "handoff"`, `facts`, a budget, the web page.
- **A coding agent with a policy**: a pi extension with `policy = true`,
  `gate = "off"`, the terminal.
- **A reader**: `tools = "read-only"`, no plugins.

The examples in yobuild's `examples/` (`handoff.toml`, `coding.toml`,
`minimal.toml`) are each a whole, working manifest.
