# yobuild

**Build an agent by choosing its parts.** yobuild assembles working agents
from plugins other ecosystems already publish — pi extensions, DeepSeek
Harness plugins, MCP servers, rutis plugins in Rust, TypeScript, Python or
Go — plus a few small built-ins of its own, on top of the
[yoagent](https://github.com/yologdev/yoagent) loop.

> Status: **M2b (install, lock, check): install and lock done; `yobuild check` next.** The built-ins and the frontend
> (`yobuild::builtins`, `yobuild::frontend`, [`frontend/`](frontend/README.md),
> with its [examples](examples/README.md)), the manifest
> (`yobuild::manifest`), `yobuild run`, and installing pinned plugins
> (`yobuild::install`, M2b; `yobuild check` is next). Numbers quoted are measured in the yoagent
> repository.
>
> ```sh
> scripts/try-coding-agent.sh                         # in the checkout: installs the plugin runtimes, runs the coding agent example
> cd ~ && mkdir research && cd research && git init   # anywhere outside the checkout
> path/to/yobuild/scripts/try-handoff.sh --demo       # the handoff agent (examples/handoff.toml), scripted; works in the current directory
> ```

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
rutis   = "../frontend/plugins/todo-plan.ts"   # draws the plan in the page
runtime = "node"

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
scripts/try-coding-agent.sh --setup-only   # once (Node 22.19+, npm): the adapters and the frontend's plugins
cd my-project
DEEPSEEK_API_KEY=… cargo run --manifest-path path/to/yobuild/Cargo.toml -- run path/to/yobuild/examples/handoff.toml "the task"
```

`yobuild run` loads its plugins' adapters from the checkout it was built
from. It installs a manifest's pinned pi packages, DSH packages and stdio
MCP servers itself (never running a package script), into
`~/.yobuild/plugins/` (`YOBUILD_HOME`), as the lock beside the manifest
says (`handoff.toml` → `handoff.lock`, written on the first run; check it
in; `--locked` refuses a lock that is not current). A rutis plugin's
runtime is installed in its own package. The other examples:
[`examples/`](examples/README.md).

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
