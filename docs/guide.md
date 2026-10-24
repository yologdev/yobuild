# The yobuild guide

How to make, run and trust an agent with yobuild. The README's
[Start](../README.md#start) gets one running in a minute; this guide is
the rest. Every manifest key, as yobuild reads it, is in the skill's
[manifest reference](../skill/yobuild/reference/manifest.md): this guide
links there rather than repeating it.

- [What an agent is](#what-an-agent-is)
- [Installing](#installing)
- [Making an agent](#making-an-agent)
- [The manifest, in brief](#the-manifest-in-brief)
- [Plugins and servers](#plugins-and-servers)
- [Checking an agent](#checking-an-agent)
- [Running an agent](#running-an-agent)
- [Scripted runs](#scripted-runs)
- [What keeps an agent in bounds](#what-keeps-an-agent-in-bounds)
- [When something fails](#when-something-fails)
- [The cache](#the-cache)
- [Upgrading](#upgrading)

## What an agent is

An agent is a **manifest**, `agent.toml`, plus a prompt file. The manifest
names:
- **a model** (any provider yoagent knows), and limits on what a run may
  spend;
- **plugins**, taken unchanged from the ecosystems that publish them: pi
  extensions, DeepSeek Harness (DSH) plugins and rutis plugins;
- **MCP servers**, as found on npm or anywhere;
- **yobuild's built-ins**: questions to you that do not block the run, a
  required report, facts the host measures, an approval gate, a scratch
  folder;
- **a frontend**: the web page, a phone on the same Wi-Fi, or the
  terminal.

yobuild installs what the manifest pins, vets it, and runs the agent on
the [yoagent](https://github.com/yologdev/yoagent) loop. The agent works in
the folder `yobuild run` starts in.

## Installing

```sh
cargo install --git https://github.com/yologdev/yobuild   # until 0.1 is on crates.io
yobuild --version
```

- **Platforms:** macOS and Linux. On Windows, `yobuild new` and
  `yobuild skill install` work, and `run` and `check` refuse.
- **Node 22.19+ and npm 11.19+** for any agent with plugins (or a pinned
  MCP server, or the terminal UI). Node 22 brings npm 10: update it with
  `npm install -g npm@latest`. yobuild says what it found when either is
  missing or older.
- **A model key** for live runs only, in the variable the provider uses
  (`DEEPSEEK_API_KEY`, `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, …). `check`
  and scripted runs need none.

yobuild installs its own runtime files (the pi and DSH adapters, the
terminal UI) into `~/.yobuild/runtime/` the first time an agent needs them.
That takes a minute once, and it says so.

## Making an agent

Three ways, from least to most work.

**Copy one built into yobuild.** `yobuild new --list` names them:

| Agent | What it does |
|---|---|
| `web-research` | researches a question on the web and reports with its sources |
| `notes-keeper` | answers from a folder of notes and tidies them, asking before each write |
| `code-reviewer` | reviews a TypeScript project's code quality, read-only |
| `spreadsheet-clerk` | turns a bank's CSV export into a spreadsheet with totals |
| `code-health` | finds a TypeScript project's type errors and outlines it, read-only |
| `handoff` | takes a task and works on it alone, asking without waiting, ending with a report |
| `coding` | a coding agent in the terminal, a pi policy judging its commands |
| `minimal` | the smallest: reads its folder, no plugins, no Node |

```sh
yobuild new my-research --from web-research
```

The first five are the [gallery](../gallery/): each was built by a coding
agent through the skill, and is checked and run in yobuild's CI. The copy
is the agent as built, with two changes `new` prints:
- its `[agent] name` becomes the folder's name;
- its README's paths name the copy.

Each gallery agent's README says what it needs (keys, network, files it
reads or writes) and how to run it. Change the copy as you like: it is
yours, and an upgrade never touches it.

**Start from the starter.** `yobuild new my-agent` writes:
- `agent.toml`, with sensible defaults (a DeepSeek model, read-only tools,
  questions to you, a report, a small budget, the web page) and every
  other key shown commented, with a line on what it does;
- a prompt to fill in;
- a demo reply file, so `check` and a scripted run work at once.

**Have a coding agent build it.** `yobuild skill install` puts the yobuild
skill where Claude Code reads it (`~/.claude/skills/yobuild/`; `--project`
for one project, `--dir <folder>` for another coding agent). Then ask
Claude Code for an agent: "make me an agent that keeps my reading list".
The skill teaches it to ask you what matters, pick parts, write the
manifest, check it, run it scripted, and ask you before a live run.

## The manifest, in brief

```toml
[agent]
name   = "research"
model  = "deepseek/deepseek-flash"
prompt = "prompts/agent.md"         # {cwd} and {scratch} are filled in

[limits]
budget_usd         = 0.25           # a run stops when it would spend more
session_budget_usd = 2.0            # all runs together

[[plugin]]
dsh    = "dsh-free-search@0.8.1"    # every package pinned, exactly
config = { provider = "bing", disabledEngines = [], bingMarket = "en-US" }
allow  = ["multi_search", "research"]   # these run without asking you

[builtins]
ask_owner = true                    # it may ask you, and carries on meanwhile
report    = ["Short answer", "Sources used", "Waiting on you"]
tools     = "read-only"             # read, list and search, in its folder

[frontend]
web = true
```

- **The tables:** `[agent]`, `[limits]`, one `[[plugin]]` or `[[mcp]]` per
  part, `[builtins]`, `[frontend]` (and `[dsh]`). Every key is in the
  [manifest reference](../skill/yobuild/reference/manifest.md), with
  patterns and how to write the prompt.
- **Read strictly:** an unknown key, a range instead of a version, or a
  secret written into the file is refused, with its line and column.
- **Secrets** are never written: `${NAME}` takes them from the
  environment when the agent starts.
- **What the parts give:** [parts](../skill/yobuild/reference/parts.md)
  lists the built-ins, the pi and DSH plugins known to load, and MCP
  servers, with the versions to pin.

## Plugins and servers

- **pi extensions** (`pi = "pkg@1.2.3"`, or a path) and **DSH plugins**
  (`dsh = "pkg@1.2.3"`, or a path) run unchanged, in Node, through
  yoagent-rutis's adapters. Their skills are not loaded: what they teach
  goes into the prompt.
- **rutis plugins** (`rutis = "./plugin.ts"`, `runtime = "node"`) are
  plugins of your own, in TypeScript. One lives in its own npm package:
  - the nearest folder above it with a `package.json`, which must be
    `"type": "module"`;
  - with `@arcships/rutis-runtime` installed there at the version
    yobuild's runtimes use, 0.12.0 (`npm install @arcships/rutis-runtime@0.12.0`;
    `check` names it when it is missing).
- **yobuild's own plugins** are named, not given a path:
  - `rutis = "yobuild:todo-plan"` draws a plan on the page;
  - `rutis = "yobuild:search-links"` turns search results into links;
  - `pi = "yobuild:confirm-dangerous"` is a pi approval policy.
- **MCP servers:** `npm = "pkg@1.2.3"` (or a program's path) for a stdio
  server, `url = "https://…"` for an HTTP one. Their tools are named
  `<name>__<tool>`.

**Pins and the lock.** yobuild installs every pinned package itself into
`~/.yobuild/plugins/`, never running a package's install scripts.
- **The lock:** what it resolved is written beside the manifest
  (`agent.toml` → `agent.lock`). Commit it.
- **`--locked`** (for CI, or an agent someone checked) refuses a lock that
  is missing, stale or disagrees with itself, rather than resolving it
  again.
- **Without `--locked`**, a changed pin is resolved again, and yobuild
  says what changed.

## Checking an agent

```sh
yobuild check agent.toml          # a report; exit 1 when anything has a problem
yobuild check agent.toml --json   # the same, for a program
```

`check` needs no model and no key. It starts each plugin and server
alone, then the agent together, and for each entry says:
- what it offers;
- how the gate treats each of its tools (free, judged, allowed, asks);
- what yobuild gives it, and what it does not get;
- what `run` would refuse, with a fix for each problem.

It also notes what you may want to know:
- a pi extension told it is not trusted;
- a variable its code reads that it does not get;
- a model whose limits are guesses.

Run it after every change to the manifest.

## Running an agent

From the folder the agent should work in:

```sh
DEEPSEEK_API_KEY=… yobuild run path/to/agent.toml           # the frontend the manifest names
DEEPSEEK_API_KEY=… yobuild run path/to/agent.toml "a task"  # start with a task
yobuild run path/to/agent.toml --web 0.0.0.0:8787            # where the page listens
DEEPSEEK_API_KEY=… yobuild run path/to/agent.toml --once "a task"   # one task, no frontend
```

- **The web page** prints its URL. Open it to talk to the agent, answer
  its questions, and see each tool call. A task given on the command line
  waits until the page is open. With `phone = true` it listens on every
  interface and prints a link for a phone on the same Wi-Fi.
- **The terminal UI** (`terminal = true`) draws in the terminal. Ctrl+C
  quits.
- **`--once`** runs one task with nobody there:
  - each tool call and notice goes to stderr, then the last answer and how
    the run ended to stdout;
  - a question gets no answer, and a call the gate would ask about is
    refused;
  - the exit code is 0 only if the run completed.
- **`--measure`** says each plugin's time and each runtime's memory after
  each run.
- **Each run ends with a cost line** (`Cost: $0.0034 (…)`) and, with
  `facts`, what changed in the folder's git repository.

## Scripted runs

`--scripted scripts/demo.json` replaces the model with a reply file: no
key, no cost, the same every time. The plugins and servers still run for
real (a search searches). Use it before every live run:

```sh
yobuild run agent.toml --scripted scripts/demo.json --once "$(cat scripts/demo.task)"
```

A reply file is a list of the model's turns: tool calls, then a last
text. A call may say what its result must contain (`"expect"`), so the
run checks what a tool found, not only that it ran. The format, what
`--once` shows and the traps are in the
[scripted reference](../skill/yobuild/reference/scripted.md).

## What keeps an agent in bounds

Not a sandbox: a plugin has Node's whole power, and the agent's tools
reach your files. What yobuild does:

- **The gate**, an allow-list. Reading is free, as are builds and tests
  in the project and writing new files or the agent's own. Anything else
  asks you, and is refused with nobody there. That covers deleting or
  overwriting what you have, an unknown program or option, and a plugin's
  tool not allowed by name. `allow = [...]` on an entry frees its
  harmless tools.
- **Grants**, per plugin, off unless given:
  - `workspace`: the agent's folder;
  - `trust`: the project is trusted;
  - `model = "info"`: which model runs;
  - `wake`: it may message the agent;
  - `exec`: a pi extension running commands, judged by the gate with
    `"gate"`;
  - `env`: named variables.

  What a plugin was answered for want of a grant is said, by `check` and
  by `run`.
- **The kept environment.** Plugins and servers start without your
  credentials and other exported variables: the model's key stays with
  yobuild, so a plugin cannot spend outside the run's budget. What one
  truly needs is handed over on purpose, and `check` notes it. Your files
  (a `~/.aws/credentials`, a `.env`) are still readable.
- **Budgets and limits**, per run and per session; a run that would spend
  more stops, and says which limit.
- **Questions that do not block:** the agent asks with `ask_owner` and
  carries on. Your answer reaches it mid-run, or starts the next run.

The design's [§9 and §12](design.md) explain each, and what each does not
cover.

## When something fails

- **At start:** a refusal says why, and `what to do: …` follows when
  yobuild knows a fix.
- **After a run that did not complete:** the same. A notice says what to
  do, and `check` gives each problem its `fix`.
- **Every message:** the
  [failures reference](../skill/yobuild/reference/failures.md) lists each
  one yobuild gives, what it means and what to do.

Some common ones:
- **"needs its key in …":** export the model's key.
- **"yobuild needs Node 22.19+ and npm 11.19+: found …":** update Node or
  npm.
- **A plugin failing with an authentication error:** it may want a
  variable the kept environment left out. `check` notes what its code
  reads.
- **`--locked` refusing a lock:** run once without it, and commit the new
  lock.

The log goes to stderr (`RUST_LOG=info` for more), or to a file named at
the end under the terminal UI.

## The cache

Everything yobuild installs is under `~/.yobuild` (`YOBUILD_HOME` to move
it):
- `runtime/<set>/<hash>/`: its own runtime files, a folder per version;
- `plugins/<set>/<hash>/`: agents' pinned packages, shared by agents whose
  pins resolve alike.

Old folders are not removed. Delete `~/.yobuild`, or any folder in it, at
any time: what is needed is installed again.

## Upgrading

See the README's [Upgrading](../README.md#upgrading):
- the new binary;
- `yobuild skill install` again, which updates the skill if nobody edited
  it;
- a DSH lock that a new DSH base made stale, locked again by one run
  without `--locked`.

What each release changes is in the [CHANGELOG](../CHANGELOG.md).
