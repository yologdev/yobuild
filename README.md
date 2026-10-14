# yobuild

**Build an agent by choosing its parts.** yobuild assembles working agents
from plugins other ecosystems already publish — pi extensions, DeepSeek
Harness plugins, MCP servers, rutis plugins in Rust, TypeScript, Python or
Go — plus a few small parts of its own, on top of the
[yoagent](https://github.com/yologdev/yoagent) loop.

> Status: **design, not code yet.** These documents are for review. Nothing
> here is built; numbers quoted are measured in the yoagent repository.

## The idea in one example

The `handoff_agent` example in yoagent is a delegate you hand a task to
and walk away from. It plans, researches, asks you only what is yours to
decide — without waiting for the answer — and ends every run with a report
and facts the model cannot make up. Of what it does:

- the **plan** is a pi extension (`@juicesharp/rpiv-todo`), unchanged from npm;
- the **web search** is a DeepSeek Harness plugin (`dsh-free-search`), unchanged;
- the **loop** is yoagent;
- what makes it *a handoff agent* is three small parts — ask the owner
  without blocking, check the report, add the facts — and a frontend
  (browser, phone).

The agent is mostly a *selection*. About 870 lines of Rust in that example
are wiring. yobuild turns the wiring into a manifest:

```toml
[agent]
name   = "handoff"
model  = "deepseek/deepseek-flash"
prompt = "prompts/handoff.md"

[[plugin]]
pi = "@juicesharp/rpiv-todo@2.12.0"

[[plugin]]
dsh    = "dsh-free-search@0.8.1"
config = { provider = "bing", bingMarket = "en-US" }

[parts]
ask_owner = true          # questions that do not block the run
report    = "handoff"     # Done / Waiting on you / Decisions / Next, then facts
gate      = "destructive" # ask before deleting or overwriting the owner's files

[frontend]
web = true
phone = true
```

```sh
yobuild run handoff.toml
```

## Documents

- [Vision](docs/vision.md) — why, for whom, what it is not.
- [Design](docs/design.md) — architecture, the manifest, the parts, what
  moves from yoagent, the safety model.
- [Roadmap](docs/roadmap.md) — milestones, and the open questions for review.

## Relationship to yoagent

yoagent stays a loop library: the loop, providers, tools, the `Extension`
contract, and the plugin bridge (yoagent-rutis) with its pi and DSH
adapters. yobuild is an *application* of it, depending on yoagent and
yoagent-rutis from crates.io. The experimental yoagent-frontend crate
(sessions, browser/phone/terminal UIs, the plugin host) moves here.

## License

MIT (proposed, matching yoagent).
