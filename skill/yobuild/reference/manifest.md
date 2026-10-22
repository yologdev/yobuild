# The manifest (`agent.toml`)

One TOML file per agent, read strictly: an unknown key or a wrong value is
an error naming its line and column. Paths are relative to the manifest's
folder. The agent itself works in the folder `yobuild run` is started in.

## Every key, at once

A reference, not a template: it shows every kind of entry; an agent uses
a few. Start from the patterns below, or from yobuild's `examples/`.

```toml
[agent]
name   = "researcher"                 # one plain name (it names the scratch folder)
model  = "deepseek/deepseek-flash"    # provider/id; the key from the provider's variable
prompt = "prompts/agent.md"           # optional: the system prompt ({cwd}, {scratch} filled in)

[limits]                              # all optional; yoagent's defaults: 50 turns, 1M tokens, 10 min
max_turns   = 80
max_tokens  = 4_000_000               # per run
max_minutes = 30
budget_usd  = 1.0                     # dollars per run (refused for a model with no price)
session_budget_usd = 2.0              # dollars across the session's runs

[[plugin]]                            # a pi package, pinned
pi    = "@juicesharp/rpiv-todo@2.12.0"
allow = ["todo"]                      # its tools the gate lets run without asking

[[plugin]]                            # a DSH package, pinned
dsh    = "dsh-free-search@0.8.1"
config = { provider = "bing", disabledEngines = [], bingMarket = "en-US" }

[[plugin]]                            # a local rutis plugin (TypeScript, in its own npm package)
rutis   = "plugins/word_count.ts"
runtime = "node"
name    = "words"                     # optional: else the file's stem
grants  = { workspace = true }        # optional: what its host gives it

[[mcp]]                               # a stdio MCP server: the pinned package's bin, run
name  = "files"                       # its tools are files__<tool>
npm   = "@modelcontextprotocol/server-filesystem@1.0.0"
args  = ["."]                         # as written: "." is the folder yobuild run starts in
allow = ["read_text_file", "list_directory"]   # the server's own names

[[mcp]]                               # an MCP server over HTTP
name    = "example"                   # required here: a host's dots cannot prefix tool names
url     = "https://mcp.example.com/mcp"
headers = { Authorization = "Bearer ${EXAMPLE_MCP_TOKEN}" }

[builtins]
ask_owner = true                      # questions to the owner, without waiting
report    = "handoff"                 # or a list: ["Done", "Next"]
facts     = true                      # what changed (git), tokens, tool calls, after each run
gate      = "allow-list"              # the default; "off" only with a sandbox or a policy plugin
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
- `name`: one plain name (no `/`, not `.` or `..`).
- `model`: `provider/id`, the id as the provider's API names it. Ids with
  a price in yoagent's table (so a budget works): `deepseek/deepseek-flash`
  (the examples'), `deepseek/deepseek-v4-flash`, `deepseek/deepseek-v4-pro`,
  and every id yoagent has a preset for — the model's real context window
  and output limit, where another id of those providers often gets
  smaller ones (about 128K in, 4K out; DeepSeek's ids have their own):
  `anthropic/claude-fable-5`, `anthropic/claude-fable-5-1`,
  `anthropic/claude-opus-5-5`, `anthropic/claude-opus-5`,
  `anthropic/claude-opus-4-8`, `anthropic/claude-sonnet-5-5`,
  `anthropic/claude-sonnet-5`, `anthropic/claude-haiku-5-5`,
  `anthropic/claude-haiku-4-5`; `openai/gpt-5.5`, `openai/gpt-6-astra`,
  `openai/gpt-6-sol`, `openai/gpt-6-luna`, `openai/gpt-6.1-sol`;
  `google/gemini-4-argon`, `google/gemini-3.8-flash`; `xai/grok-4.7`;
  `mistral/mistral-large-4`; `zai/glm-5.3`, `zai/glm-5.3-flash`;
  `qwen/qwen3.8-max`, `qwen/qwen3.8-flash`. Any other id (but DeepSeek's
  and Meta's) gets the provider's default limits, and `check` notes it:
  prefer a listed one. Use the provider whose key
  the person has; for DeepSeek, `deepseek/deepseek-flash` unless the
  task needs a stronger model (`deepseek/deepseek-v4-pro`). Providers and their key
  variables: `anthropic` (`ANTHROPIC_API_KEY`), `openai`
  (`OPENAI_API_KEY`), `deepseek` (`DEEPSEEK_API_KEY`), `google`
  (`GEMINI_API_KEY` or `GOOGLE_API_KEY`), `groq`, `mistral`, `xai`, `qwen`
  (`DASHSCOPE_API_KEY`), `zai`, `minimax`, `meta`; `ollama` and `local`
  take no key (`local` needs `base_url`). Any other provider needs
  `base_url` (https, or http on this machine) and `api_key_env` (the
  variable's name). Neither `check` nor reading the manifest asks the
  provider whether an id exists: a wrong one fails the first live run
  (`API error: …`). A budget does tell: it is refused for an id with no
  price.
- `prompt`: a file (below). Without one, a plain prompt naming the agent
  and its folder.

**`[limits]`**: each more than zero. `budget_usd` and
`session_budget_usd` need a model id with a price in yoagent's table
(DeepSeek's, Anthropic's, OpenAI's … main models; a local one has none):
the manifest is refused otherwise ("`budget_usd set, but … has no
price`").

**`[[plugin]]`** names exactly one of:
- `pi = "<package>@<version>"` or a path (`./ext.ts`, a folder): a pi
  extension or package. No `config`. Its skills are not loaded.
- `dsh = "<package>@<version>"` or a path: a DeepSeek Harness plugin.
  `config` is the plugin's own. Its skills are not loaded either (`check
  --json`'s `guidanceChars` is the prompt guidance it does add): put the
  workflow they teach into the prompt. DSH files and DSH packages cannot be in
  one manifest yet. yobuild's DSH base (the packages its DSH runtime
  carries) pins some DSH packages itself — `dsh-free-search` 0.8.1,
  `@deepseek-ai/dsh-tool-ask-user` and `dsh-user-questions` 0.2.0-rc.2:
  if you pin one of these, pin it at that version.
- `rutis = "<path>"` with `runtime = "node"` (only Node runs, for now): a
  local plugin, loaded in its own npm package — the nearest folder above
  it with a `package.json`, which must be `"type": "module"` for a `.ts`
  file and have `@arcships/rutis-runtime` installed at the version
  yobuild's runtimes use (0.12.0; `check` names it when it is missing).

And optionally:
- `name`: unique among plugins and servers. The default: a package's
  name without its scope, or a file's stem.
- `allow = [...]`: the plugin's tools (as it names them) the gate lets run
  without asking. Not `bash`, `write_file`, `write`, `edit_file`, `edit`
  (judged call by call), not `ask_owner`, not a name with `__`. Freed only
  when that plugin offers it (`check` says when another does).
- `policy = true`: this pi or rutis plugin is the approval policy: while
  no plugin judging calls is up, every call is denied. Usually with
  `gate = "off"`. Not on a DSH plugin.
- `grants = { ... }`: what yobuild, as the plugin's host, gives it beyond
  what every plugin has — each off unless granted, and named by `check`.
  On a rutis plugin: `workspace = true` (the
  `host_workspace` service's `cwd()`: the agent's folder), `trust = true`
  (`trusted()` says yes: the plugin may run the project's code —
  honour-system, nothing enforces it), `model = "info"`
  (`host_session.model()`: provider, id, limits), `wake = true`
  (`host_session.wake(text)`, one line, no control character: it reaches the agent as `[plugin
  name] text` — steering a run going on, else a run of its own, never
  before the owner's first run, at most 3 a session for all plugins; under
  `--once`, only while the task's run goes on; one not delivered is a
  Warning notice). Every plugin may use `host_session.info()` (the
  session's id, its reset generation) and `host_approval.judge(argv,
  cwd?)` (the agent's gate's verdict on one command's words, `{ free:
  true, judged: false }` with `gate = "off"`); a rutis plugin also
  `host_approval.ask(question, details?)` (Allow/Block put to the owner,
  naming the plugin, answered `"allow"`, `"block"` or `"unanswered"`).
  On a DSH plugin, `workspace`, `wake` and `env` only, `wake` with
  `workspace`, and **the same on every DSH plugin** (else refused): one DSH host — a
  stand-in DSH agent in the agent's folder — serves them all. It is what
  DSH plugins that read the calling agent need (`dsh-univer-office`'s
  file tools fail `SESSION_SCOPE_UNAVAILABLE` without it); once loaded,
  every DSH call goes through it. A DSH plugin's `steer`/`followup` reach
  the agent as `[DSH plugin] text`, line breaks made spaces, under the
  same limits; one refused is a Warning notice. `[dsh] host_services`
  still decides DSH's own file, credential and storage services.
  On a pi extension, per extension: `trust = true`
  (`ctx.isProjectTrusted()` says yes — honour-system), `wake = true`
  (`pi.sendUserMessage`, or a `pi.sendMessage` that starts a turn, reaches
  the agent as `[pi extension name] text`, line breaks made spaces, under
  the same limits; one refused is a Warning notice), `exec = "off"` (the
  default: `pi.exec` refused), `"gate"` (each command's words judged by
  the gate; one it would ask about is put to the owner, naming the
  extension, and runs only on Allow) or `"free"` (anything, as in pi); and
  `model = "info"` (`ctx.model`, `ctx.modelRegistry`) on **every pi plugin
  or none** (the adapter has one model for all). `workspace` is refused (a
  pi extension has `ctx.cwd`). What a pi extension was answered for want
  of a grant (not trusted, no model) and a `pi.exec` refused are Warning
  notices naming the grant. Every pi extension sees the conversation
  (`message_end`, `context`, observer events): there is no grant for it.
  On any plugin, `env = ["TAVILY_API_KEY", "PI_LENS_*"]`: those variables
  are left in its runtime's environment (below, **The environment**) —
  names, or a prefix (at least two characters — letters, digits or `_` —
  then `_*`), each once, and the same on every pi plugin or none, every
  DSH plugin or none, every rutis plugin of one package or none (they
  share one runtime). A
  credential yoagent reads (and the agent's `api_key_env`) and what loads
  code (`NODE_OPTIONS`, `NODE_PATH`, `LD_PRELOAD`, `DYLD_*`) pass only by
  their full name: a prefix that would match one is refused. It starts no
  DSH host.
- `config = { ... }`: a table; every string in it may hold `${NAME}`
  references, and a key named like a secret (`token`, `key`, `password`,
  `auth`, …) must hold only references, plus at most a scheme word
  (`Bearer ${TOKEN}`, `Basic …`, `Token …`).

**`[[mcp]]`** names exactly one of:
- `npm = "<package>@<version>"` (or a path to a program): a stdio server,
  with `args`, `env` (secrets as `${NAME}`), and `bin` when the package
  has several.
- `url = "https://…"`: an HTTP server, with `headers` (secrets as
  `${NAME}`, a scheme word allowed).

And `name`: its tools are named `<name>__<tool>`. The default is the
package's name without its scope, or the URL's host — whose dots no tool
name may hold, so give an HTTP server a `name` (letters, digits, `-`,
`_`; no `__`). And optionally `allow` (the server's own tool names).
A server may want absolute paths (`check --json` shows its parameters):
the prompt builds them from `{cwd}`, and the reply files write
`"{cwd}/…"`.

**The environment.** Plugins and stdio MCP servers start with a kept
environment, in `run` and in `check` alike (on unix): the path, home,
user, shell, working folder, temp folders, time zone, `LD_LIBRARY_PATH`,
the locale (`LANG`, `LC_*`), the terminal's variables, `BROWSER`, `CI`,
the proxies (`HTTPS_PROXY`, …, `NODE_USE_ENV_PROXY`) and CA settings,
the `XDG_*` folders and where toolchains live (`GOPATH`, `CARGO_HOME`,
`JAVA_HOME`, `VIRTUAL_ENV`, …, asdf's and mise's homes). Everything else
the person has exported is left out: every credential yoagent reads
(`DEEPSEEK_API_KEY`, `OPENAI_API_KEY`, …, AWS's and Cloudflare's,
`API_KEY`, the agent's `api_key_env`), so a plugin cannot call a model
outside the run's budget, and every other secret or setting
(`GITHUB_TOKEN`, `TAVILY_API_KEY`, `AWS_PROFILE`, a plugin's own
`PI_LENS_*` settings, `NODE_OPTIONS`). `check`'s pi and DSH plugins
probed alone get what is left out set empty instead. `check` and `run`
say how many variables set here are left out, naming the credentials
among them (`check`'s agent note, `run`'s Info notice); `check --json`
lists them all as the agent's `leftOut`, and gives each entry's
`environment` (what it gets beyond the kept set). On each entry `check`
notes a variable its own code reads that is set here but left out ("its
code reads `X` …: grant it"; a credential yoagent reads is noted without
that), and a granted one that looks like a secret or loads code
(`NODE_OPTIONS`); on the agent, a proxy setting that holds a password. A
server or plugin that truly needs a variable is handed it on purpose: a
stdio server through its `env` (`TAVILY_API_KEY = "${TAVILY_API_KEY}"`)
or its `args`, an HTTP server through its `url` or `headers`, a DSH or
rutis plugin through a `${NAME}` in its `config`, any plugin through
`grants = { env = [...] }` (its whole runtime gets it). `check` notes
each credential yoagent reads handed over ("it is given `X`, a
credential yoagent reads: what it spends with it is not metered"). An
`env` grant naming a variable that is unset or empty hands nothing over
under that name, which `check` and `run` say ("its `env` grant names
`X`, unset or empty here: …"). When entries lose different credentials,
`check` notes on each what it has taken out. On macOS (with System
Integrity Protection on, the default), a `DYLD_*` variable an entry is
handed does not reach it (everything starts through the protected
`/usr/bin/env`): `check` and `run` name it. Tell the person before you
hand anything over. This is not a sandbox: a plugin can still read a key
from a file the person keeps it in (`~/.aws/credentials`, a shell
profile, a `.env`). A plugin that fails with an authentication error, or
says a feature is off, may be one that wanted a variable.

**Pins.** Every package is `name@1.2.3` (exact; `1.2.3-beta.1` too) or a
local path (`./`, `../`, `/`). Tags, ranges, `git`, URLs and `file:` are
refused. Two entries of one kind (pi, DSH, stdio servers) may not pin one
package at two versions.

**`[builtins]`**: `ask_owner`, `facts`, `scratch` are `true`/`false`
(default `false`). `report` is `"handoff"` (Done / Waiting on you /
Decisions / Next) or a list of section names. `gate` is `"allow-list"`
(default: reading is free; `bash` commands it can read, builds and tests
in the project, and writes of new or the agent's own files are free;
everything else asks) or `"off"`. `tools` is `"coding"` (default: bash,
read, write, edit, list, search) or `"read-only"` (read, list, search,
inside the agent's folder).

**`[frontend]`**: `web`, `phone` (needs `web`), `terminal` (not with
`web`). Nothing set: the web page. `yobuild run --web ADDR` sets where it
listens.

## Writing the prompt

`prompts/agent.md`, plain text. Say, in a few lines:

- who the agent is and what it is for, in the owner's words;
- what it works in: `{cwd}` (the folder) and, with `scratch`, `{scratch}`
  (for what is not a deliverable);
- how to use its tools, by the names `check` lists (`agent.tools`);
- when to ask: with `ask_owner`, only what is the owner's to decide; never
  to wait (the question returns at once; the answer arrives later as a
  message `[Owner's answer to #N] …`, steering a run going on or starting
  the next) — keep working on what does not depend on it, or end the run
  with the question listed as waiting;
- what it must never do without asking;
- how to end: with `report`, yobuild appends its instruction (the
  sections) itself; say what goes in each. The check is lenient: each
  section name must appear anywhere in the last answer, in any case — so
  choose names that appear only as headings (`Sources cited`, not
  `Brief`).

`examples/prompts/handoff.md` in yobuild is a whole one.

## Patterns

- **An agent that reports back** (`examples/handoff.toml`): `ask_owner`,
  `report = "handoff"`, `facts`, a scratch, a budget, the web page.
- **Ask, and go on** (a question that must be answered): `ask_owner`; the
  prompt says to carry on with what does not depend on it, or end with
  the question waiting — the answer starts the next run. A scripted run
  shows only that the question was asked.
- **Ask before working** (clarify, then do): `ask_owner`, and either no
  `report`, or a `report` with a section for open questions —
  `"handoff"` (its "Waiting on you"), or your own list holding one
  (`["Short answer", "Sources used", "Waiting on you"]`). A run that only
  asks still ends with every section: the question under the open one,
  the others "none yet". A `report` without such a section fails that run
  (sent back 3 times, then `[Extension failed: report]`). Asking and
  answering are two runs: script them as two reply files (scripted.md).
- **A coding agent with a policy** (`examples/coding.toml`): a pi
  extension with `policy = true`, `gate = "off"`, the terminal.
- **A reader** (`examples/minimal.toml`): `tools = "read-only"`, no
  plugins. It has no tool that writes: its work is its last answer.
  `scratch` adds nothing then (the read-only tools reach only the agent's
  folder).
- **Not in a git repository**: leave `facts` off — it only says "not a
  git repository".
- **A budget**: start small (`budget_usd = 0.5` for a short task) and
  raise it once a live run shows the cost (yobuild says each run's tokens
  with `facts`). `budget_usd` is per run, and an answered question starts
  another run, so one job that asks first can cost up to twice it; nothing
  caps one job across its runs. `session_budget_usd` caps everything
  until yobuild stops: say both to the person. A cap the person gives
  without saying per what ("cap it at a dollar") is the session's
  (`session_budget_usd`), with `budget_usd` a fraction of it: say the split.
  A limit "per question" for an agent that may ask first: `budget_usd` at half of it, and a
  `session_budget_usd` a few questions' worth (say the figure you chose).
  A comment on
  `budget_usd` says "per run", never "per question" or "per task".
- **Turns**: `max_turns` counts model requests in a run (50 by default);
  a short lookup needs 10–20, a coding task more. A cap stops a run that
  loops; the budget is what caps the cost.
- **Run checks, write nothing** (a reviewer): no setting runs a
  project's type checker or tests without also letting the agent write —
  `tools = "coding"` brings `write_file`, and a new file in the project
  is free. Use a server or plugin that runs the check and only reports
  (ESLint's `@eslint/mcp`), keep `tools = "read-only"`, and say what was
  not run (`npx tsc --noEmit`) in the report.
- **`search`'s pattern**: ripgrep's regex when `rg` is installed, else
  grep's basic one — keep patterns plain (`eval(`, `: any`), one per call,
  so they mean the same on both.
- **What to tell the person of a read-only agent**: the gate may still
  ask Allow/Block for a writer or `bash` the agent does not have (it
  judges a call before the tool is looked up); Block is the answer, and
  either answer changes nothing.
- **No local files at all**: not possible — yoagent's tools are always
  there; `tools = "read-only"` is the least (reading, inside the folder
  yobuild runs in). Tell the person, and suggest running it in an empty
  folder.
