# The yobuild guide

How to make, run and trust an agent with yobuild. The README's
[Start](../README.md#start) gets one running in a minute; this guide is
the rest. What you can build with yobuild, and why, is in
[What you can build](use-cases.md). Every manifest key, as yobuild
reads it, is in the skill's
[manifest reference](../skill/yobuild/reference/manifest.md): this guide
links there rather than repeating it.

- [What an agent is](#what-an-agent-is)
- [Installing](#installing)
- [Making an agent](#making-an-agent)
- [The manifest, in brief](#the-manifest-in-brief)
- [Plugins and servers](#plugins-and-servers)
- [Checking an agent](#checking-an-agent)
- [Running an agent](#running-an-agent)
- [Resuming a session](#resuming-a-session)
- [Running an agent on a schedule](#running-an-agent-on-a-schedule)
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

Download yobuild for your machine from the
[releases page](https://github.com/yologdev/yobuild/releases): unpack the
`.tar.gz` and put `yobuild` on your PATH. Or build it:

```sh
cargo install yobuild
yobuild --version
```

- **Rust** (a recent stable toolchain, 1.88+), only to build it.
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
terminal UI, its own plugins) into `~/.yobuild/runtime/` the first time an agent needs them.
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
for one project; for another coding agent, `--dir` and the skill's own
folder, such as `--dir ~/.codex/skills/yobuild`, not the skills folder
holding it). Then ask
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
  part, `[builtins]`, `[frontend]` (and `[dsh]`, `[session]`). Every key is in the
  [manifest reference](../skill/yobuild/reference/manifest.md), with
  patterns and how to write the prompt.
- **Read strictly:** an unknown key or a range instead of a version is
  refused, with its line and column.
- **Secrets** are never written: `${NAME}` takes them from the
  environment when the agent starts. A value whose name looks like a
  secret (a key, token, password, auth, …) holding anything but `${NAME}`
  references is refused, and so is a URL with its password written in. A
  secret under another name is not caught: keep it in a variable.
- **What the parts give:** [parts](../skill/yobuild/reference/parts.md)
  lists the built-ins, the pi and DSH plugins known to load, and MCP
  servers, with the versions to pin.

## Plugins and servers

- **pi extensions** (`pi = "pkg@1.2.3"`, or a path) and **DSH plugins**
  (`dsh = "pkg@1.2.3"`, or a path) run unchanged, in Node, through
  yoagent-rutis's adapters. Their skills are not loaded: put what they
  teach into your prompt.
- **rutis plugins** (`rutis = "./plugin.ts"`, `runtime = "node"`) are
  plugins of your own, in TypeScript. One lives in its own npm package:
  - the nearest folder above it with a `package.json`, which must be
    `"type": "module"` for a `.ts` plugin;
  - with `@arcships/rutis-runtime` installed there at the version
    yobuild's runtimes use, 0.20.0, and the SDK it imports,
    `@arcships/rutis`, at the same version (`npm install
    @arcships/rutis@0.20.0 @arcships/rutis-runtime@0.20.0`, with
    `--ignore-scripts` if you like; `npm ci --ignore-scripts` where the
    package has its lock, as an agent copied by `yobuild new --from`
    does). Without the runtime, `run` and `check` refuse before anything
    starts, naming the command.
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
  is missing, stale, disagrees with itself or holds a set nothing pins any
  more, rather than resolving it again.
- **Without `--locked`**, a changed pin is resolved again, and yobuild
  says what changed.

**Servers the agent finds itself.** With an `[extend]` table, the agent
gets two tools, `find_plugin` and `load_plugin`. With them it can find an
MCP server it lacks and load it for the session. It works like this:
- **The search:** `find_plugin` searches the MCP registry and npm live
  (there is no list built in), and runs nothing.
- **Two questions:** you are asked before any of the server's code runs,
  and again once its tools are listed. After that, each call of its
  tools asks too.
- **The guards:** only versions at least `min_age_days` old (7 by
  default) are taken, and a session loads at most `max_loads` servers
  (2 by default).
- **Nothing is written:** at the session's end yobuild prints the
  `[[mcp]]` block that would keep each one, for you to add if you want.
- **Someone must answer:** under `--once` it is refused.
- **pi and DSH packages too:** with `kinds = ["mcp", "pi", "dsh"]` it
  can also find a pi or DSH package on npm and load it the same way. A
  loaded package runs in a runtime of its own with its tools only: its
  hooks are not called, and it gets no grants, `config` or host
  services. One that needs those belongs in the manifest as a
  `[[plugin]]`, which the session's end prints. The gallery's
  `project-notebook` loads the `pi-memory` package this way.
- **Remote MCP servers too:** with `"remote"` among the `kinds` it can
  find a server in the MCP registry that runs elsewhere and connect to it
  over https. The search asks the MCP registry only, and
  `min_age_days` does not apply: nothing installs or runs on your
  computer, but each call sends its arguments to the server's host.
  When it takes a credential, you are asked between the two questions
  which environment variable holds it, never the value: the value goes
  to that host only and is shown nowhere. A server that needs you to
  sign in through a browser (OAuth) is signed in: you are asked whether
  to sign in, then shown a link (on the page, or printed where yobuild
  runs when the page is on your phone) that you open and finish in your
  browser. The token is kept for this session only and the agent never
  sees it; its sign-in must be on the server's own site. In `agent.toml`
  such a server is an `[[mcp]]` with `url` and `auth = "oauth"` (the
  block printed at the end says it; its URL may hold no `${…}`, as the
  sign-in sends it on): it is signed in each session once the page or
  the terminal UI is open, its sign-in on any site, and the task waits
  for it, so the agent has its tools from the start. The gallery's `docs-helper`
  connects to Context7, which needs no sign-in, this way.

[§14](design.md#14-agents-that-extend-themselves-m5-decided-2026-10-25)
is the design.

## Checking an agent

```sh
yobuild check agent.toml          # a report; exit 1 on a problem, or when the agent did not start
yobuild check agent.toml --json   # the same, for a program
```

`check` needs no model and no key. It starts each plugin and server
alone (a few DSH plugins that the DSH runtime loads anyway are checked
only together), then, when no entry has a problem, the agent together.
For each entry it says:
- what it offers;
- how the gate treats each of its tools (free, judged, allowed, asks, or
  runs, with `gate = "off"`);
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
DEEPSEEK_API_KEY=… yobuild run path/to/agent.toml --web 0.0.0.0:8787   # where the page listens
DEEPSEEK_API_KEY=… yobuild run path/to/agent.toml --once "a task"   # one task, no frontend
```

- **The web page** prints its URL. Open it to talk to the agent, answer
  its questions, and see each tool call. A task given on the command line
  waits until the page is open. `--web` sets where it listens (not with
  `--once`, nor for a terminal agent). With `phone = true` (beside
  `web = true`) it listens on every interface and prints a link for a
  phone on the same Wi-Fi, or says it found none.
- **The terminal UI** (`terminal = true`) draws in the terminal. Ctrl+C
  quits.
- **`--once`** runs one task with nobody there:
  - each tool call and notice goes to stderr, then the last answer and how
    the run ended to stdout;
  - a question gets no answer, and a call the gate would ask about is
    refused;
  - the exit code is 0 only if the run completed.
- **`--resume`** continues the conversation this agent last had in this
  folder ([below](#resuming-a-session)).
- **`--measure`** says each plugin's time and each runtime's memory after
  each run, when the agent has plugins.
- **Each run's cost** is shown as it ends: on the page's last line for
  the run (` · $0.0034`), and under `--once` as a line on stderr
  (`Cost: $0.0034 (…)`). With `facts`, a notice says what changed in the
  folder's git repository.
- **Every run is logged** as it ends: when, its task, how it ended, its
  cost, the tools it called, the facts, the questions left waiting on
  you, and its report (the last answer). `yobuild log path/to/agent.toml`
  prints the last ten (`-n 30` for more, `--json` as logged); the page
  lists the last twenty beside the conversation. The log is yours alone
  (under `~/.yobuild/logs/`, readable only by you); it never holds a
  tool's arguments or results, and a credential set in the environment
  is written as its `${NAME}`, never its value.

## Resuming a session

Every session is saved as it goes, so stopping yobuild (Ctrl+C, a closed
terminal, a crash) loses at most the run in progress. To go on where it
was, run it again from the same folder with `--resume`:

```sh
DEEPSEEK_API_KEY=… yobuild run path/to/agent.toml --resume
DEEPSEEK_API_KEY=… yobuild run path/to/agent.toml --resume --once "and now the tests"
```

- **What comes back:** the agent's conversation, its open `ask_owner`
  questions (asked again, with their numbers), and what the session has
  spent, so `session_budget_usd` still holds.
- **What does not:** what the page showed before (it starts empty; the
  agent remembers), the plugins the agent loaded through `[extend]` (it
  may load them again), sign-ins (you sign in again), facts' snapshot and
  the plugins' own state. A notice says which apply.
- **One saved session per agent and folder**, under yobuild's home
  (`~/.yobuild/sessions/`, or `$YOBUILD_HOME/sessions/`), readable by you
  only. Nothing is written in the project. A run without `--resume`
  starts a new conversation, which replaces the saved one once its first
  run ends (it says so as it starts).
- **Another model, or an edited `agent.toml`:** the session resumes, with
  a warning; the prompt is the manifest's as it is now.
- **To keep conversations off the disk**, put `[session] save = false` in
  the manifest.

The saved file holds the conversation as the model saw it: what a tool
showed is in it.

## Running an agent on a schedule

`--once` runs one task with nobody there, so cron or launchd can start an
agent: every night, say, to draft the release notes for what was merged
that day. Two things make such a run useful when nobody watches it:
- **`--report FILE`** writes the agent's last answer to FILE, whole or not
  at all, readable only by you. Its folder must be there (yobuild says
  so before anything runs). It is always written: a run that gave no
  answer writes "No answer: the run ended …" over yesterday's report,
  and one yobuild could not start (no node on the scheduler's `PATH`,
  say) "No answer: yobuild stopped: …".
- **The exit code** is 0 only when the run completed. A run stopped by a
  limit or a budget, one that failed, one stopped by a signal, or a
  report that could not be written, is 1: have the scheduler mail you
  the output when it is not 0.

Its questions cannot be answered while it runs: `ask_owner`'s stay open,
and the log keeps them. `yobuild log` shows each as `waiting on you:
#1 …`, and so does the page. `yobuild run agent.toml --resume`, from
the folder the agent worked in, asks them again on the page (the
conversation is saved: [Resuming a session](#resuming-a-session));
or give the next run the answer in its task.
A call the gate would ask about is refused at once (nobody is there to
allow it): `allow` what a scheduled agent needs, and keep `budget_usd`
and `session_budget_usd` set.

A worked example: the gallery's release-notes agent, run at 2:00 each
night on a project.

```sh
yobuild new ~/agents/release-notes --from release-notes   # then the npm ci line it prints
cd ~/projects/shop && yobuild check ~/agents/release-notes/agent.toml   # once, by hand
```

A script the scheduler runs, `~/bin/release-notes.sh` (`chmod 700`):

```sh
#!/bin/sh
set -eu
# cron and launchd start with a bare PATH: yobuild, node and npm must be on it.
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
# The key from a file only you can read (chmod 600), not in the crontab.
DEEPSEEK_API_KEY=$(cat "$HOME/.config/yobuild/deepseek.key")
export DEEPSEEK_API_KEY
cd "$HOME/projects/shop"   # the folder the agent works in
exec yobuild run "$HOME/agents/release-notes/agent.toml" --locked --once \
  --report "$HOME/reports/release-notes.md" \
  "Draft the release notes for everything since the last version tag."
```

With cron (`crontab -e`):

```text
0 2 * * * $HOME/bin/release-notes.sh >> $HOME/reports/release-notes.out 2>&1
```

With launchd on macOS: `~/Library/LaunchAgents/dev.yobuild.release-notes.plist`
(its paths written out in full: launchd expands neither `~` nor `$HOME`),
then `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.yobuild.release-notes.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>dev.yobuild.release-notes</string>
  <key>ProgramArguments</key>
  <array><string>/bin/sh</string><string>/Users/you/bin/release-notes.sh</string></array>
  <key>StartCalendarInterval</key>
  <dict><key>Hour</key><integer>2</integer><key>Minute</key><integer>0</integer></dict>
  <key>StandardOutPath</key><string>/Users/you/reports/release-notes.out</string>
  <key>StandardErrorPath</key><string>/Users/you/reports/release-notes.out</string>
</dict>
</plist>
```

The next morning, read the report, and the run in the log:

```sh
cat ~/reports/release-notes.md
yobuild log ~/agents/release-notes/agent.toml -n 1
```

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

Not a sandbox, unless you ask for one: a plugin has Node's whole power,
and the agent's tools reach your files. What yobuild does:

- **The gate**, an allow-list. Free:
  - reading;
  - builds and tests in the project;
  - writing new files, or the agent's own;
  - editing files in the project (it guards your files against being
    deleted or replaced, not against edits; `.env*` and `.git` ask).

  Anything else asks you, and is refused with nobody there. That covers
  deleting or replacing what you have, files outside the project, an
  unknown program or option, and a plugin's tool not allowed by name.
  `allow = [...]` on an entry frees its harmless tools.
- **Grants**, per plugin, off unless given:
  - `workspace`: the agent's folder;
  - `trust`: the project is trusted;
  - `model = "info"`: which model runs;
  - `wake`: it may message the agent;
  - `exec = "gate"` (each command judged by the gate) or `"free"`: a pi
    extension running commands;
  - `env`: named variables.

  Each ecosystem takes some of them, and every DSH plugin must have the
  same: the [manifest reference](../skill/yobuild/reference/manifest.md)
  says which. What a plugin was answered for want of a grant is said, by
  `check` and by `run`.
- **The kept environment.** Plugins and servers start without your
  credentials and other exported variables: the model's key stays with
  yobuild, so a plugin cannot spend it outside the run's budget. What one
  truly needs is handed over on purpose, and `check` notes it. The agent's
  own `bash` (with `tools = "coding"`) keeps your whole environment, the
  model's key included. Your files (a `~/.aws/credentials`, a `.env`) are
  still readable, unless the sandbox hides them.
- **The sandbox**, on request: with a `[sandbox]` table, every plugin
  runtime and stdio MCP server starts confined (bubblewrap on Linux,
  `sandbox-exec` on macOS). They write only to the agent's folder (not
  its repository's `.git`), temp and the folders `write` names; they
  cannot read your SSH keys, GnuPG, cloud, GitHub and AI tools' logins,
  `~/.npmrc`, `~/.netrc`, the keychains, your shell profiles and
  histories, nor what `hide` names; they cannot reach your terminal; the
  network stays open.

  ```toml
  [sandbox]
  write = ["~/.cache/acme"]   # a plugin's own cache, if it needs one
  hide  = ["~/.config/acme"]  # your other secrets
  ```

  It does not confine the agent's own tools (the gate judges those), and
  it does not stop a plugin reading what is not hidden and sending it.
  What a plugin writes in the agent's folder — sources, build files,
  `node_modules` — runs unconfined when your tools or the builds and tests
  the gate allows use it: look at what changed before you build.
  On Linux install bubblewrap first (`apt install bubblewrap`); on
  Ubuntu 23.10 and later it also needs unprivileged user namespaces
  allowed for it: an AppArmor profile for `bwrap`, or
  `sysctl kernel.apparmor_restrict_unprivileged_userns=0`. `check` says
  which entries are confined.
- **Budgets and limits**, per run and per session; a run that would spend
  more stops, and says which limit.
- **Questions that do not block:** the agent asks with `ask_owner` and
  carries on. Your answer reaches it mid-run, or starts the next run.

The design's [§6](design.md#6-safety-model),
[§12](design.md#12-the-host-layer-what-a-plugins-own-app-gives-it-proposed-2026-10-19)
and [§17](design.md#17-an-opt-in-sandbox-for-plugins-and-servers-m6-built-2026-11-03)
explain each, and what each does not cover.

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

Everything yobuild installs or keeps is under `~/.yobuild` (`YOBUILD_HOME` to move
it):
- `runtime/<set>/<hash>/`: its own runtime files, a folder per version;
- `plugins/<set>/<hash>/`: agents' pinned packages, shared by agents whose
  pins resolve alike;
- `logs/<agent>-<hash>/`: each agent's run log (`runs.jsonl`, and
  `runs.1.jsonl` once it passed 2 MiB), what `yobuild log` reads.

Old folders are not removed. Delete `~/.yobuild`, or any folder in it,
whenever no yobuild is running: what is needed is installed again (but
not the run logs: keep `logs/` to keep them).

## Upgrading

See the README's [Upgrading](../README.md#upgrading):
- the new binary;
- `yobuild skill install` again, which updates the skill if nobody edited
  it;
- a DSH lock that a new DSH base made stale, locked again by one run
  without `--locked`.

What each release changes is in the [CHANGELOG](../CHANGELOG.md).
