# yobuild

**Build an agent by choosing its parts.** yobuild assembles working agents
from plugins other ecosystems already publish — pi extensions, DeepSeek
Harness plugins, MCP servers, rutis plugins in Rust, TypeScript, Python or
Go — plus a few small built-ins of its own, on top of the
[yoagent](https://github.com/yologdev/yoagent) loop.

> Status: **M4 (share) under way**: yobuild runs without its repository;
> a release (0.1, crates.io and binaries) comes next. Built: the manifest,
> `yobuild run` and `yobuild check`, installing pinned plugins, the
> built-ins, the frontends (web page, phone, terminal), the host layer
> (grants, the kept environment), a gallery of agents built through the
> skill, and `yobuild new` / `yobuild skill install`. Numbers quoted are
> measured in the yoagent repository.

## Start

yobuild runs on macOS and Linux (not Windows yet). It needs Rust to
build, and Node 22.19+ with npm 11.19+ for plugins (it installs its own
runtime files into `~/.yobuild` the first time a plugin needs them). What
each release brings: [CHANGELOG.md](CHANGELOG.md).

```sh
cargo install --git https://github.com/yologdev/yobuild   # until 0.1 is on crates.io
yobuild new --list                                        # the agents built in: the gallery and the examples
yobuild new research --from web-research                  # a copy, ready to run
cd research                                               # an agent works in the folder it starts in
yobuild check agent.toml                                  # vet every plugin, no model or key needed
yobuild run agent.toml --scripted scripts/demo.json --once "$(cat scripts/demo.task)"   # scripted: no key (its searches still search)
DEEPSEEK_API_KEY=… yobuild run agent.toml                 # live: open the URL it prints
```

Each gallery agent's copy has a README saying what it needs and does (the
examples: `handoff`, `coding`, `minimal`, explained in
[`examples/`](examples/README.md)). `yobuild new` prints the commands with
the copy's full paths. An agent that works on a project (a code reviewer)
runs from the project's folder: `yobuild run path/to/agent.toml`.

`yobuild new <folder>` alone writes a starter whose every other key is
shown commented. To have a coding agent build one for you instead:
`yobuild skill install`, then ask Claude Code for an agent (below).

## The idea in one example

The handoff agent ([`examples/handoff.toml`](examples/handoff.toml)) is a delegate you hand a task to
and walk away from. It plans, researches, asks you only what is yours to
decide — without waiting for the answer — and ends every run with a report
and facts the model cannot make up. Of what it does:

- the **plan** is a pi extension (`@juicesharp/rpiv-todo`), unchanged from npm;
- the **web search** is a DeepSeek Harness plugin (`dsh-free-search`), unchanged;
- the **loop** is yoagent;
- what makes it *a handoff agent* is three small built-ins — ask the owner
  without blocking, check the report, add the facts — and a frontend
  (browser, phone).

The agent is mostly a *selection*. As a Rust example it was about 600 lines,
mostly wiring (a third its scripted demo). yobuild turns the wiring into a
manifest — this is the whole agent, beside its prompt file:

```toml
[agent]
name   = "handoff"
model  = "deepseek/deepseek-flash"
prompt = "./prompts/handoff.md"

[limits]
max_turns          = 80
max_tokens         = 4_000_000
max_minutes        = 30
session_budget_usd = 2.0

[[plugin]]
pi    = "@juicesharp/rpiv-todo@2.12.0"
allow = ["todo"]                       # runs without asking the owner

[[plugin]]
rutis = "yobuild:todo-plan"            # yobuild's own UI plugin: draws the plan in the page

[[plugin]]
dsh    = "dsh-free-search@0.8.1"
config = { provider = "bing", disabledEngines = [], bingMarket = "en-US" }
allow  = ["multi_search", "research", "advanced_search", "domain_search",
          "platform_search", "video_search", "cache_search"]

[builtins]
ask_owner = true          # questions that do not block the run
report    = "handoff"     # Done / Waiting on you / Decisions / Next, then facts
facts     = true
gate      = "allow-list"  # ask before anything not known to be harmless
scratch   = true

[frontend]
web = true
```

```sh
yobuild new ~/agents/handoff --from handoff   # agent.toml, agent.lock, its prompt and demo
cd my-project
DEEPSEEK_API_KEY=… yobuild run ~/agents/handoff/agent.toml "the task"
```

`yobuild run` installs its own runtime files (the pi and DSH adapters, the
terminal UI) into `~/.yobuild/runtime/` the first time a manifest needs
them. It installs a manifest's pinned pi packages, DSH packages and stdio
MCP servers itself (never running a package script), into
`~/.yobuild/plugins/` (`YOBUILD_HOME`), as the lock beside the manifest
says (`agent.toml` → `agent.lock`, written on the first run; check it
in; `--locked` refuses a lock that is not current). A rutis plugin's
runtime is installed in its own package. `yobuild check agent.toml`
vets every plugin and MCP server without a model or a key — each alone
(the compatibility matrix's probe), then the agent together — and says
what each adds, how the gate treats each tool, and what `run` would
refuse (exit 1). The other agents built in: `yobuild new --list`.

## The skill: a coding agent builds the agent

[`skill/yobuild/`](skill/yobuild/SKILL.md) teaches a coding agent (Claude
Code first) to build an agent with yobuild: ask what it is for, pick the
parts, write the manifest, `yobuild check`, run it scripted, then live —
and what to do when a run fails. `yobuild skill install` writes it where
Claude Code reads it:

```sh
yobuild skill install                    # ~/.claude/skills/yobuild, for every project
yobuild skill install --project          # ./.claude/skills/yobuild, for this one
yobuild skill install --dir <folder>     # the skill's own folder, anywhere: plain Markdown, for another coding agent
```

Then ask Claude Code for an agent ("make me an agent that …"). The gallery's
agents ([`gallery/`](gallery/)) were each built this way.

Running it again after an upgrade updates the copy, if nobody edited it.
It never overwrites a file someone changed: it names them, and `--force`
writes the skill's files over them (files of your own beside them stay).
It never writes into a folder holding no yobuild skill, `--force` or not,
so a mistyped `--dir` (a project, a home) is refused.

## Upgrading

```sh
cargo install --git https://github.com/yologdev/yobuild --force   # the new yobuild
yobuild skill install                                             # the skill, updated if nobody edited it
```

- yobuild installs its own runtime files again by itself, in a new folder
  under `~/.yobuild/runtime/` (an older yobuild keeps its own).
- Copied agents (`yobuild new --from`) are yours and stay as they are.
- A lock with DSH plugins was made against yobuild's DSH base: when a new
  yobuild's base changed, `--locked` refuses it, and a run without
  `--locked` locks it again (commit the new lock).
- `~/.yobuild` can be deleted at any time: what it holds is installed
  again when needed.

## Documents

- [Vision](docs/vision.md) — why, for whom, what it is not.
- [Design](docs/design.md) — architecture, the manifest, the built-ins, what
  moves from yoagent, the safety model.
- [Roadmap](docs/roadmap.md) — milestones, and the open questions for review.
- [Devices](docs/devices.md) — what an agent may learn about the machine's
  hardware, how it is listed and reported, and how using a device is approved.

## Relationship to yoagent

yoagent stays a loop library: the loop, providers, tools, the `Extension`
contract, and the plugin bridge (yoagent-rutis) with its pi and DSH
adapters. yobuild is an *application* of it, depending on yoagent and
yoagent-rutis from crates.io. The frontend that was yoagent-frontend
(sessions, browser/phone/terminal UIs, the plugin host) lives here as
`yobuild::frontend`.

## License

MIT (proposed, matching yoagent).
