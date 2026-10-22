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

## M2a — Manifest and runner (done: the manifest #9, `yobuild run` #11 and #12, measuring #17, allow scoping #18, the examples #23, `ask_owner` for plugins #22, rutis 0.12 #24, who a plugin is #25–#28)

- Manifest schema (§2 of the design), in TOML (decided 2026-10-16),
  strict parsing, good errors.
  `budget_usd` refused for an unpriced model, naming it.
- `model = "provider/id"` → `ModelConfig` (design §2, Models); an unknown
  provider is an error listing the known ones.
- `yobuild run` with plugins that are local paths or already installed —
  pi packages and stdio MCP servers in `node_modules/` beside the manifest,
  a rutis plugin's runtime in its package,
  DSH plugins in `adapters/dsh`, installed by hand — so M2a does not need
  M2b's cache: start runtimes, load rows, connect `[[mcp]]` servers
  (yoagent's MCP client, stdio and HTTP with headers), wire parts and
  frontend. (#11)
- A scripted CI run of a manifest with an `[[mcp]]` entry, against a stdio
  test server (a bash script, `tests/fixtures/echo/`). (#11)
- `yobuild run --measure` says per-plugin hook time and per-runtime memory
  after each run (design §9; #17), so the decision to promote a part to
  Rust rests on measurements: hook time from yoagent-rutis 0.5's
  `take_timings`, a runtime's memory from the OS (rutis gives no pid).
- The built-ins renamed (#14): `[parts]` → `[builtins]` in the manifest,
  `yobuild::parts` → `yobuild::builtins`, the gate's `with_free_tools` →
  `with_allowed_tools` (decided 2026-10-17: "parts" read as plugins moved
  into Rust; "free" as cost).
- `allow = [...]` (#15) on `[[plugin]]` and `[[mcp]]` entries (design §2,
  Allowed tools): the tools the gate lets run without asking — so a
  manifest agent that plans and searches does not ask before each call.
- `handoff.toml`, `coding.toml`, `minimal.toml` replace the three examples (#23;
  `coding_agent.rs` stays for its `--watch`)
  (design §7, "The examples as manifests": `[builtins] tools`,
  `[limits] session_budget_usd`, `policy = true` on a plugin; the demos
  as CI tests — the handoff one answering over the page's WebSocket).
- `ask_owner` as a host service (design §3; #22), so plugins can ask
  the owner too — rutis plugins (Node); not pi extensions or DSH plugins
  (decided 2026-10-18: it would be a yobuild-only API in ecosystems that
  have none, which their plugins would not run elsewhere with; revisited
  if a real plugin needs it); Python and Go once it is checked that they
  can call host services.
- yoagent-rutis 0.6 and rutis 0.12 (#24, done): pi's nested calls only
  within the calling extension.
- Who a plugin is: the row the host sees (design §4; #25, and its code):
  `allow`, a rutis policy plugin's wait, and `ask_owner`'s "asked by"
  keyed on the row (rutis 0.12's `caller_row`); a plugin waiting for a
  missing service named at start (yoagent-rutis 0.6.1's
  `rows::wait_running`, every service name shared).

**Done when** `yobuild run handoff.toml --scripted` reproduces today's
demo, plus the new per-plugin hook time and per-runtime memory lines.

## M2b — Install, lock, check (done: design #29, install and lock #30, check #31)

- Resolve and install pinned plugins and stdio MCP servers per ecosystem
  into the cache, in up to three sets (`pi`, `dsh` over the DSH adapter's
  base, `mcp`), each at `~/.yobuild/plugins/<set>/<hash>/`. Write
  the lock beside the manifest, named after it (`agent.toml` →
  `agent.lock`): per set, npm's own lock (versions, URLs, integrity hashes).
  A stale set is resolved again; `--locked` refuses instead. Installs
  never run package scripts. Unpinned entries are refused (as now).
  `node_modules/` beside the manifest is no longer read. A pi package's
  peers that are not pi's own are installed with it; a DSH package the
  DSH base pins at another version is refused, and so are DSH files
  beside DSH packages, for now.
- `yobuild check`: each plugin alone through yoagent-rutis's `probe` (the
  matrix's verdict; rutis plugins loaded as `run` loads them; MCP servers
  `tools/list`, no call; a pi tool the probe kept only by leaving a
  built-in out is a problem, since `run` refuses it; yoagent-rutis's
  `probe` feature, on crates.io since 0.4). Then the agent together,
  through `run`'s own start without a model: host services, a policy's judge, tool-name
  clashes, an `allow` naming a tool that is not offered. A text report, or
  `--json`; exit 1 on a problem.
- Two PRs after this design: the install and the lock, then `check`.

**Done when** `yobuild run handoff.toml` installs from a clean cache (an
empty `YOBUILD_HOME`) and runs, and `yobuild check` reports the same as
the compatibility matrix for the plugins it uses.

## M3 — The skill and a gallery (design §11; decided 2026-10-19; hints #33, the skill #34, the gallery's design #35)

- `skill/yobuild/SKILL.md` ("build an agent with yobuild"): ask, pick
  parts by capability (from the measured compatibility list), write the
  manifest, `yobuild check`, run scripted, then live; with
  `reference/manifest.md`, `scripted.md`, `parts.md` and `failures.md`.
- What to do when a run fails: every failure `check` and `run` give —
  the built-ins' (the gate, the report, limits and budgets, `ask_owner`,
  facts, scratch), a policy plugin's, the loop check's, the model's and
  the session's — what it means and the fix, in `failures.md` and in
  yobuild itself (`run/hints.rs`: `run` says `what to do: …` after a run
  that did not complete, and in a Notice; `check` gives each problem its
  `fix`, its JSON at version 2).
- The gallery (`gallery/<name>/`, each checked and run scripted in CI),
  first web research (`pi-web-access`), your MCP server, and the
  spreadsheet clerk (`dsh-univer-office`, if `check` passes it); then
  from plugins that load today:

  | Agent | Parts |
  |---|---|
  | Spreadsheet clerk | `dsh-univer-office` (14 tools) + ask_owner + report |
  | Web research | `pi-web-access` + ask_owner + report |
  | Code-quality reviewer | `pi-lens` (8 tools) + `@plannotator/pi-extension` |
  | Docs maintainer | `openwiki` (6 tools) + gate on writes |
  | Delegating lead | `pi-subagents` + budget |
  | Visual report | `@dfy-plugins/dsh-visualize` + search |
  | Your MCP server | an `[[mcp]]` entry + ask_owner |

- Tried with Claude Code: a person describes an agent, the coding agent
  builds it with the skill, end to end.

**Done when** three agents in the gallery were built through the skill by
a coding agent, not by hand, and each is checked and run scripted in CI.

## M3b — The host layer (design §12; proposed 2026-10-19, steps 1–3 built 2026-10-20, step 4 2026-10-21)

What a plugin's own app gives it — DSH's agent and session, pi's trust and
model — provided by yobuild as host services, translated by the adapters,
granted per plugin in the manifest (`grants = { … }`, closed by default;
a grant a host can give only to the whole runtime is written on every
plugin of it or none) and named by `check`.

- **Built (step 1, #42):** the L1 services (`workspace`, `session` with an
  id and generation, `approval`), `grants` read and named by `check`,
  rutis's grants per row.
- **Built (step 2, #45, yoagent-rutis 0.7.1):** the DSH host — a stand-in
  agent per generation in DSH's own `agents`/`sessions`, turns around runs,
  its log in the jsonl backend, approvals and questions through DSH's own
  services; `workspace` and `wake`, the same on every DSH plugin (one
  host); the host's start checked, its services runtime-local.
- **Built (step 3, #46, yoagent-rutis 0.8.0):** pi grants per extension —
  `trust`, `wake` (tagged, bounded), `exec` (`"off"` by default, `"gate"`
  judged by the gate and asked, `"free"`), and `model = "info"` on every pi
  plugin or none; `observe` dropped (every extension sees the conversation
  already, design §6); fixed answers and refused `pi.exec` said by `run`.
- Left: `check` saying fixed answers (no session watches there), and noting
  a pi extension's observer events (the adapter would have to report its
  handlers).
- **Built (step 4, 2026-10-21; design §12 "Step 4"):** every credential
  yoagent reads taken out of plugin runtimes and stdio MCP servers (`env
  -u`; set empty only in `check`'s pi/DSH probes alone and on Windows) and
  of the helpers; a `keys` grant and a server's `env` handing one back on
  purpose, which `check` notes; not a sandbox, said. After the yoagent
  session's review: a program that cannot start said before `env` runs, a
  `keys` grant of an unset name said, the `DYLD_*` note on macOS, a macOS
  CI job (#56).
- **Proposed (step 5, 2026-10-22; design §12 "Step 5"):** plugins with a
  kept environment — every inherited variable left out but a fixed kept
  set (the path, home, locale, terminal, session, proxies, standard
  folders and toolchains) and what an `env` grant (replacing `keys`)
  names; `check` naming a variable a plugin's code reads that it does not
  get. Measured first, on macOS: every plugin the matrix found loading,
  the gallery and the examples load alike with only `PATH`, `HOME`,
  `TMPDIR`, `TERM`, the locale, `USER`, `LOGNAME`, `SHELL` and the model
  key.

**Done when** `dsh-univer-office` and `pi-lens` work in a gallery agent,
their tools' results checked (not only `✓`); the stand-in probe runs inside
rutis as a test in yoagent-rutis; the compatibility matrix is re-measured;
and `check` names every grant and every fixed answer.

Where it stands (2026-10-21): `dsh-univer-office` works in a gallery agent
(`spreadsheet-clerk`: CI to its first asking tool, one live run checking
the `.xlsx`'s totals); `pi-lens` works with `trust = true`, checked by
hand (a type error found), not yet in a gallery agent — its language
servers come from GitHub and npm on first use, which a CI run would fetch
each time; the stand-in probe is a test in yoagent-rutis; the matrix's
plugins re-checked through `yobuild check` on 2026-10-22, on macOS (each
the matrix found loading loads with the tools listed, also with only
`PATH`, `HOME`, `TMPDIR`, `TERM`, the locale, `USER`, `LOGNAME`, `SHELL`
and the model key: design §12 "Step 5"; `dsh-free-search` at the base's
0.8.1, not the matrix's 0.9.1; yoagent's own matrix, its script, not
re-run); `check` names every grant, not yet the fixed answers.

## M3c — Plugin web output and model calls (design §12, L3 and `llm`)

- The frontend's plugin origin and framed UI plugins.
- A metered `llm` for plugins, once yoagent makes a run's outside spend
  public.

**Done when** visualize's chart shows on the page from a sandboxed origin,
and a plugin's model call is charged to the run's budget.

## M4 — Share

- `yobuild new <name>` (a manifest and a prompt to edit), `cargo install
  yobuild` (and a prebuilt binary), `yobuild skill install` (the skill
  into `~/.claude/skills/`).
- Running without the repository: the adapters (today `adapters/`,
  copied by `scripts/copy-adapters.sh`) and the terminal UI installed
  into the cache too (design §4).
- Docs site, the video re-cut for yobuild, posts.
- Release 0.1 (no git dependencies in a release: yoagent-rutis and its
  `probe` come from crates.io already).

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
5. **Plugin installs.** Decided (2026-10-18, design §4): one cache folder
   per set, keyed by the set's hash, shared by agents with the same set,
   not one per agent folder; the lock is JSON carrying npm's own lock (`npm
   ci` verifies it); a stale set is resolved again by default, said with
   what was added and removed, and `--locked` refuses (CI); the adapters
   stay in `adapters/` until M4; `check` vets each plugin alone (the
   matrix's verdict), then the agent together (`run`'s start).
6. **The gate.** Decided: Rust (it is on the safety path — design §9), an
   allow-list, the TypeScript deny-list kept as an example. Still open, the
   rule for what belongs in Rust: are the five reasons in design §9 (safety, hot
   path, footprint, target, ownership) the right ones, and is "over 5 % of
   a turn" the right threshold?
7. **Windows.** rutis supports it; the frontend's terminal UI and the
   tests are Unix-only today. In scope for 0.1?
8. **Models.** DeepSeek and Claude first? (Any provider in design §2's
   table runs.) DeepSeek pricing: done (yoagent 0.25.6).
