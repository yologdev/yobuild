# Roadmap

Draft for review. Each milestone ends with something that runs.

## M0 — Bootstrap (done 2026-10-15)

- Repository, license, CI (fmt, clippy, tests; Node for the pi and DSH
  runtimes). Bun, Go and Python runtimes join CI when an agent uses them
  (M2a).
- Move **yoagent-frontend** here with its history (`git subtree split`);
  depend on **yoagent ≥ 0.25.4** and
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

## M1 — Parts (done 2026-10-16)

- One crate, `yobuild` (decided 2026-10-15): the frontend is
  `yobuild::frontend` (done, #2); the parts become `yobuild::parts`.

- `yobuild::parts`: `ask_owner`, `report`, `facts`, `scratch`, `gate`,
  extracted from `handoff_agent` with their own tests (done: #3, #7).
  `ask_owner` is a tool here; offering it to plugins as a host service
  moves to M2a, once Python and Go plugins are confirmed able to call
  one.
- The gate is a Rust `before_tool` (decided 2026-10-15: no Node needed
  for safety, microseconds instead of ~3 ms, tests beside the Rust),
  asking the owner through the session. It started as a port of
  `confirm-dangerous.ts`'s deny-list; after four review rounds kept finding
  ways around it, it became an allow-list (programs, each with its
  options), read through one pipeline (#7, after three more reviews). The
  pi extension stays as an example. (yoagent's `ToolGate` is a
  different thing — a decision model judging each call — and could be
  added on top later.)
- `handoff_agent` rebuilt on the parts (done): the same behaviour, except
  the gate is now an allow-list (it asks about more), in much less code.
- **In parallel, in yoagent** (separate PRs, so M2 is not blocked waiting
  on a release):
  - yoagent-rutis exposes the ecosystem probe as a public module, and a
    way to locate the pi and DSH adapter files (decided 2026-10-15);
    `yobuild check` needs the first, and the second lets yobuild drop
    `vendor/`. Done: yoagent-rutis 0.4.0 (`adapters`, `probe`; yoagent
    #318); yobuild copies the adapters with `scripts/copy-adapters.sh`.
  - DeepSeek pricing in yoagent's price table, so `budget_usd` and cost
    facts work for DeepSeek models too. Done: yoagent 0.25.6, at the peak
    rate (an upper bound; off-peak is half).

**Done when** the handoff demo passes on the parts. It does (CI, every
push). The yoagent-rutis PR above continues alongside M2a (decided
2026-10-16): M2a does not need it; M2b's `yobuild check` does.

## M2a — Manifest and runner (in progress: the manifest #9, `yobuild run` #11)

- Manifest schema (§2 of the design), in TOML (decided 2026-10-16),
  strict parsing, good errors.
  `budget_usd` refused for an unpriced model, naming it.
- `model = "provider/id"` → `ModelConfig` (design §2, Models); an unknown
  provider is an error listing the known ones.
- `yobuild run` with plugins that are local paths or already installed —
  pi packages, stdio MCP servers and the rutis runtime in `node_modules/`
  beside the manifest,
  DSH plugins in `adapters/dsh`, installed by hand — so M2a does not need
  M2b's cache: start runtimes, load rows, connect `[[mcp]]` servers
  (yoagent's MCP client, stdio and HTTP with headers), wire parts and
  frontend. (#11)
- A scripted CI run of a manifest with an `[[mcp]]` entry, against a stdio
  test server (a bash script, `tests/fixtures/echo/`). (#11)
- `yobuild run` prints per-plugin hook time and per-runtime memory
  (design §9), so the decision to promote a part to Rust rests on
  measurements.
- `handoff.toml`, `coding.toml`, `minimal.toml` replace the three examples.
- `ask_owner` as a host service, so plugins can ask the owner too — after
  checking that Python and Go plugins can call host services.

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
4. **Manifest format.** Decided (2026-10-16): TOML — read by serde in
   the Rust core, strictly, with no runtime needed to read a manifest.
5. **Plugin installs.** One cache per ecosystem and lockfile hash (proposed),
   or per agent folder (simpler, bigger)?
6. **The gate.** Decided: Rust (it is on the safety path — design §9), an
   allow-list, the TypeScript deny-list kept as an example. Still open, the
   rule for what belongs in Rust: are the five reasons in design §9 (safety, hot
   path, footprint, target, ownership) the right ones, and is "over 5 % of
   a turn" the right threshold?
7. **Windows.** rutis 0.9 supports it; the frontend's terminal UI and the
   tests are Unix-only today. In scope for 0.1?
8. **Models.** DeepSeek and Claude first? (Any provider in design §2's
   table runs.) DeepSeek pricing: done (yoagent 0.25.6).
