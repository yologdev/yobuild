# Roadmap

Draft for review. Each milestone ends with something that runs.

## M0 — Bootstrap (done 2026-10-15)

- Repository, license, CI (fmt, clippy, tests; Node 22.19+, Bun 1.4, Go 1.24,
  Python 3.10+ runtimes as yoagent-rutis's CI has them).
- Move **yoagent-frontend** here with its history (`git subtree split`);
  depend on **yoagent ≥ 0.25.4** (`ToolGate`, for the gate part) and
  **yoagent-rutis ≥ 0.3.1** (the DSH host services behind
  `[dsh] host_services`) from crates.io; both released 2026-10-14.
- The three examples build and their CI checks pass here (the scripted
  handoff demo, the pty check, the web tests).
- In yoagent: remove `integrations/yoagent-frontend`, point its README to
  yobuild.

**Done when** yobuild's CI is green on the moved code and yoagent's on
the removal.

Done: yobuild CI green, yoagent#314 merged (with pointers at the old
paths).

## M1 — Parts

- One crate, `yobuild`: the frontend becomes `yobuild::frontend` (decided
  2026-10-15), the parts `yobuild::parts`.

- `yobuild::parts`: `ask_owner`, `report`, `facts`, `scratch`, `gate`,
  extracted from `handoff_agent` with their own tests.
- The gate is a Rust `before_tool` on yoagent's `ToolGate` (decided
  2026-10-15: no Node needed for safety, microseconds instead of ~3 ms,
  tests beside the Rust); the pi extension stays as an example.
- `handoff_agent` rebuilt on the parts: same behaviour, much less code.
- **In parallel, in yoagent** (separate PRs, so M2 is not blocked waiting
  on a release):
  - yoagent-rutis exposes the ecosystem probe as a public module, and a
    way to locate the pi and DSH adapter files (decided 2026-10-15);
    `yobuild check` needs the first, and the second lets yobuild drop
    `vendor/`. An API addition, so it ships in a yoagent-rutis release
    (0.3.2 or 0.4.0).
  - DeepSeek pricing in yoagent's price table (in progress), so
    `budget_usd` and cost facts work for DeepSeek models too.

**Done when** the handoff demo passes on the parts.

## M2a — Manifest and runner

- Manifest schema (§2 of the design), strict parsing, good errors.
  `budget_usd` refused for an unpriced model, naming it.
- `model = "provider/id"` → `ModelConfig` (design §2, Models); an unknown
  provider is an error listing the known ones.
- `yobuild run` with plugins that are local paths or already installed —
  the plugin folders with their `node_modules` that the yoagent examples
  use today, installed by hand — so M2a does not need M2b's cache: start
  runtimes, load rows, connect `[[mcp]]` servers (yoagent's MCP client,
  stdio and HTTP with headers), wire parts and frontend.
- A scripted CI run of a manifest with an `[[mcp]]` entry, against a stdio
  test server copied from yoagent's `tests/mcp_stdio_test.rs`.
- `yobuild run` prints per-plugin hook time and per-runtime memory
  (design §9), so the decision to promote a part to Rust rests on
  measurements.
- `handoff.toml`, `coding.toml`, `minimal.toml` replace the three examples.

**Done when** `yobuild run handoff.toml --scripted` reproduces today's
demo, plus the new per-plugin hook time and per-runtime memory lines.

## M2b — Install, lock, check

- Resolve and install pinned plugins and stdio MCP servers per ecosystem
  into the cache, write `agent.lock`; installs never run package scripts;
  unpinned entries are refused.
- `yobuild check`: vet every plugin without a model, using the probe
  exposed by yoagent-rutis (a git dependency until its release); list each
  MCP server's tools without calling any.

**Done when** `yobuild run handoff.toml` installs from a clean cache and
runs, and `yobuild check` reports the same as the compatibility matrix
for the plugins it uses.

## M3 — The skill and a gallery

- `SKILL.md` ("build an agent with yobuild"): pick parts by capability
  (from the measured compatibility list), `yobuild check`, write the
  manifest, run scripted, then live; what to do when a plugin is refused.
- Example manifests from plugins that load today:

  | Agent | Parts |
  |---|---|
  | Spreadsheet clerk | `dsh-univer-office` (14 tools) + ask_owner + report |
  | Research brief | `pi-web-access` or `dsh-free-search` + `rpiv-todo` |
  | Code-quality reviewer | `pi-lens` (8 tools) + `@plannotator/pi-extension` |
  | Docs maintainer | `openwiki` (6 tools) + gate on writes |
  | Delegating lead | `pi-subagents` + budget |
  | Visual report | `@dfy-plugins/dsh-visualize` + search |
  | Your MCP server | an `[[mcp]]` entry + ask_owner |

- Tried with Claude Code: a person describes an agent, the coding agent
  builds it with the skill, end to end.

**Done when** three agents in the gallery were built through the skill by
a coding agent, not by hand.

## M4 — Share

- `yobuild new <name>` (a manifest and a prompt to edit), `cargo install
  yobuild` (and a prebuilt binary).
- Docs site, the video re-cut for yobuild, posts.
- Release 0.1. Requires the yoagent-rutis release with the probe from
  crates.io (no git dependencies in a release).

**Done when** someone who has not seen the repository installs yobuild,
runs `yobuild new`, and has a gallery agent running from the docs alone;
0.1 is tagged (and published, if open question 2 says so).

## M5 — Agents that extend themselves

- `find_plugin` / `load_plugin` tools, behind the owner's approval and the
  fail-closed rules; refused plugins never load.
- A demo: an agent missing a capability finds a plugin, asks, loads it, and
  finishes the task.

**Done when** the demo runs live end to end, a refused plugin is shown
never loading, and a denied approval leaves the agent without the plugin.

## Open questions (for review)

1. **Name and home.** Decided: `yologdev/yobuild`, public.
2. **Publish to crates.io?** As a binary crate (`cargo install yobuild`),
   and the library (`yobuild::parts`, `yobuild::frontend`) for people who
   would rather write Rust than a manifest?
3. **The probe as a library.** Decided: exposed from yoagent-rutis as a
   public module (so `check` and the compatibility matrix cannot drift
   apart), together with the adapter files' location.
4. **Manifest format.** TOML (proposed) vs YAML vs a TypeScript config for
   people who live in npm.
5. **Plugin installs.** One cache per ecosystem and lockfile hash (proposed),
   or per agent folder (simpler, bigger)?
6. **The gate.** Decided: Rust (it is on the safety path — design §9), the
   TypeScript version kept as an example. Still open, the rule itself: are the five reasons in design §9 (safety, hot
   path, footprint, target, ownership) the right ones, and is "over 5 % of
   a turn" the right threshold?
7. **Windows.** rutis 0.9 supports it; the frontend's terminal UI and the
   tests are Unix-only today. In scope for 0.1?
8. **Models.** DeepSeek and Claude first? (Any provider in design §2's
   table runs.) DeepSeek pricing: **in progress** in yoagent, not open.
