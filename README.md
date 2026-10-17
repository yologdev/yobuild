# yobuild

**Build an agent by choosing its parts.** yobuild assembles working agents
from plugins other ecosystems already publish — pi extensions, DeepSeek
Harness plugins, MCP servers, rutis plugins in Rust, TypeScript, Python or
Go — plus a few small built-ins of its own, on top of the
[yoagent](https://github.com/yologdev/yoagent) loop.

> Status: **M2a (manifest and runner).** The built-ins and the frontend
> (`yobuild::builtins`, `yobuild::frontend`, [`frontend/`](frontend/README.md),
> with its [examples](examples/README.md)), the manifest
> (`yobuild::manifest`) and `yobuild run` (plugins already installed;
> installing them is M2b). Numbers quoted are measured in the yoagent
> repository.
>
> ```sh
> scripts/try-coding-agent.sh                         # in the checkout: installs the plugin runtimes, runs the coding agent example
> cd ~ && mkdir research && cd research && git init   # anywhere outside the checkout
> path/to/yobuild/scripts/try-handoff.sh --demo       # the handoff agent, scripted; works in the current directory
> ```

## The idea in one example

The `handoff_agent` example ([`examples/`](examples/README.md)) is a delegate you hand a task to
and walk away from. It plans, researches, asks you only what is yours to
decide — without waiting for the answer — and ends every run with a report
and facts the model cannot make up. Of what it does:

- the **plan** is a pi extension (`@juicesharp/rpiv-todo`), unchanged from npm;
- the **web search** is a DeepSeek Harness plugin (`dsh-free-search`), unchanged;
- the **loop** is yoagent;
- what makes it *a handoff agent* is three small built-ins — ask the owner
  without blocking, check the report, add the facts — and a frontend
  (browser, phone).

The agent is mostly a *selection*. That example is about 600 lines of Rust,
mostly wiring (a third is its scripted demo). yobuild turns the wiring into a manifest
(this one comes as `handoff.toml` in a later step of M2a: until then its prompt file
is not there, and its plugins must be installed by hand at these versions — the pi
package with `--legacy-peer-deps`, as `frontend/plugins/.npmrc` does):

```toml
[agent]
name   = "handoff"
model  = "deepseek/deepseek-flash"
prompt = "prompts/handoff.md"

[[plugin]]
pi    = "@juicesharp/rpiv-todo@2.12.0"
allow = ["todo"]                       # runs without asking the owner

[[plugin]]
dsh    = "dsh-free-search@0.8.1"
config = { provider = "bing", bingMarket = "en-US" }
allow  = ["multi_search", "web_search"]

[builtins]
ask_owner = true          # questions that do not block the run
report    = "handoff"     # Done / Waiting on you / Decisions / Next, then facts
gate      = "allow-list"  # ask before anything not known to be harmless

[frontend]
web = true
phone = true
```

```sh
yobuild run handoff.toml
```

To run an agent today, from a checkout (`yobuild run` loads its plugins'
adapters from the checkout it was built from). This one has an MCP server
(bash: Unix) and needs no Node; plugins and the terminal UI need the setup
line (Node 22.19+, npm) first:

```sh
scripts/try-coding-agent.sh --setup-only   # for plugins and the terminal UI
cargo run -- run tests/fixtures/echo/agent.toml \
  --scripted tests/fixtures/echo/script.json --once "echo hi"
```

A manifest's pi packages, stdio MCP servers and the rutis runtime are
installed in `node_modules/` beside it, DSH plugins with
`npm install --no-save --prefix adapters/dsh …`, at exactly their pinned
versions; installing them is M2b.

## Documents

- [Vision](docs/vision.md) — why, for whom, what it is not.
- [Design](docs/design.md) — architecture, the manifest, the built-ins, what
  moves from yoagent, the safety model.
- [Roadmap](docs/roadmap.md) — milestones, and the open questions for review.

## Relationship to yoagent

yoagent stays a loop library: the loop, providers, tools, the `Extension`
contract, and the plugin bridge (yoagent-rutis) with its pi and DSH
adapters. yobuild is an *application* of it, depending on yoagent and
yoagent-rutis from crates.io. The frontend that was yoagent-frontend
(sessions, browser/phone/terminal UIs, the plugin host) lives here as
`yobuild::frontend`.

## License

MIT (proposed, matching yoagent).
