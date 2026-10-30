# Roadmap

Each milestone ends with something that runs. The open questions at the end are for review.

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
  `fix`, its JSON at version 2 then; 3 since step 5).
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

Since 2026-10-30 the gallery also holds `release-notes`, its first agent
with a tool of its own: one small rutis plugin the coding agent wrote
(design §11 "A tool of its own"). That is the vision's "a manifest and at
most one small custom tool", walked end to end. Its first build found
that the skill imported rutis's SDK but named only its runtime to
install. The skill also sketched the tool contract and grants in code
too thinly to go without guessing. The skill was fixed and the agent
built again.

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
- Built: `check` saying fixed answers (2026-10-23, design §12 "Fixed
  answers in `check`": watched as `run` does, in its probes, which run
  ungranted (read with each plugin's grants), and the agent together; a
  tool's own questions stay `run`'s).
- Left: noting a pi extension's observer events (the adapter would have to report its
  handlers).
- **Built (step 4, 2026-10-21; design §12 "Step 4"):** every credential
  yoagent reads taken out of plugin runtimes and stdio MCP servers (`env
  -u`; set empty only in `check`'s pi/DSH probes alone and on Windows) and
  of the helpers; a `keys` grant and a server's `env` handing one back on
  purpose, which `check` notes; not a sandbox, said. After the yoagent
  session's review: a program that cannot start said before `env` runs, a
  `keys` grant of an unset name said, the `DYLD_*` note on macOS, a macOS
  CI job (#56).
- **Step 5 (2026-10-22; design §12 "Step 5"):** plugins with a kept
  environment — every inherited variable left out but a fixed kept set
  (the path, home, locale, terminal, session, proxies, standard folders
  and toolchains) and what an `env` grant (replacing `keys`) names: built
  (5a, #58), and `check` naming a variable a plugin's code reads that it
  does not get, `environment` and `leftOut`, one counted note (5b).
  Measured first, on macOS: every plugin the matrix found loading, the
  gallery and the examples load alike with only `PATH`, `HOME`, `TMPDIR`,
  `TERM`, the locale, `USER`, `LOGNAME`, `SHELL` and the model key.

**Done when** `dsh-univer-office` and `pi-lens` work in a gallery agent,
their tools' results checked (not only `✓`); the stand-in probe runs inside
rutis as a test in yoagent-rutis; the compatibility matrix is re-measured;
and `check` names every grant and every fixed answer.

Where it stands (2026-10-21, with later items dated): `dsh-univer-office` works in a gallery agent
(`spreadsheet-clerk`: CI to its first asking tool, one live run checking
the `.xlsx`'s totals); `pi-lens` works with `trust = true`, checked by
hand (a type error found), and since 2026-10-23 in a gallery agent,
`gallery/code-health/` (design §11 "pi-lens in the gallery": results
checked by a reply file's `expect`, the LSP probe in CI with a shared
`HOME`); the
stand-in probe is a test in yoagent-rutis; the matrix's
plugins re-checked through `yobuild check` on 2026-10-22, on macOS (each
the matrix found loading loads with the tools listed, also with only
`PATH`, `HOME`, `TMPDIR`, `TERM`, the locale, `USER`, `LOGNAME`, `SHELL`
and the model key: design §12 "Step 5"; `dsh-free-search` at the base's
0.8.1, not the matrix's 0.9.1; yoagent's own matrix, its script, not
re-run); `check` names every grant, and the fixed answers a pi extension is given as it
starts (2026-10-23; a tool's own, `run`).

## M3c — Plugin web output and model calls (design §12, L3 and `llm`)

- The frontend's plugin origin and framed UI plugins.
- A metered `llm` for plugins, once yoagent makes a run's outside spend
  public.

**Done when** visualize's chart shows on the page from a sandboxed origin,
and a plugin's model call is charged to the run's budget.

## M4 — Share (done 2026-10-25; design §13; built 2026-10-24: `yobuild:` built-ins, the runtime sets, `yobuild new`, `yobuild skill install`, the release groundwork, `docs/guide.md`, `docs/use-cases.md`)

- `yobuild new <folder> [--from <agent>]` (a manifest, prompt and demo to
  edit, or a gallery agent or example; built), `yobuild skill install
  [--project | --dir]` (the skill, free of repository paths, where a
  coding agent reads it; built), `cargo install yobuild` and a prebuilt
  binary (0.1).
- Running without the repository (design §13): the adapters and the
  terminal UI as runtime sets in the cache, built into the binary;
  built-in plugins named `yobuild:<name>`; Node and npm checked first.
- README (its pitch), `docs/guide.md` and `docs/use-cases.md` (what you can build, why, compared with other ways).
- Release 0.1, 2026-10-25 (no git dependencies in a release: yoagent-rutis and its
  `probe` come from crates.io already).

**Done when** someone who has not seen the repository installs yobuild,
runs `yobuild new`, and has a gallery agent running from the docs alone;
0.1 is tagged and published on crates.io — checked in CI by a
`fresh-machine` job with the checkout moved out of reach.

**Done:** 0.1.0 is tagged, on crates.io and on the releases page (8
files: 4 tarballs and their checksums).
- **In CI, on every push:** the `fresh-machine` job runs on Linux. It
  copies out the binary built from the checkout, then moves the checkout
  out of reach and runs `skill install` and `new --from web-research`
  and `--from coding`. Each agent is checked, and its demo runs to
  `Completed`.
- **By hand, on 2026-10-25:** I checked the released macOS arm64
  tarball myself, not a newcomer. It ran in an empty `HOME`, with only
  the binary, Node 24.7 and npm 11.19 installed beyond the system. The
  run was scripted, with no key:
  - its checksum matched;
  - `skill install` and `new --list` worked;
  - `new research --from web-research`, then `check agent.toml`, gave
    `ok`;
  - the scripted demo `--once` ran its searches and ended `Completed`;
  - the web page served, and `/ws` refused a request without the token.
- **Not yet:** nobody has checked the Linux tarballs by hand, and no
  newcomer has followed the docs alone.

Seen there: `dsh-free-search`'s own cache loads `node:sqlite`, which Node
(22.5 and later) still marks experimental, so Node prints an
`ExperimentalWarning` on stderr. It is harmless, and it comes from the
plugin. The skill's `parts.md` says so.

Later: a docs site, the video re-cut for yobuild, posts.

## M5 — Agents that extend themselves (step 1, MCP stdio servers: done 2026-10-27; design §14)

- `find_plugin` / `load_plugin` tools, behind the owner's approval and the
  fail-closed rules; refused plugins never load (design §14). Found by
  searching the MCP registry and npm, with no catalogue built in; none of
  a candidate's code runs before the owner allows it, and its tools are
  checked and shown before the agent gets them; loaded for the session
  only. Step 1: MCP stdio servers.
- A demo: an agent missing a capability finds a plugin, asks, loads it, and
  finishes the task.

**Done when** the demo runs live end to end, a refused plugin is shown
never loading, and a denied approval leaves the agent without the plugin.

**Done (step 1):** the gallery's `self-extending-reviewer` is the demo:
- **Live:** on 2026-10-27 it ran end to end ($0.0054). It found ESLint's
  server, was allowed to start and keep it, and linted with it.
- **A refused plugin:** its `not-offered` reply file proposes a package
  `find_plugin` did not return. It is refused before anything installs.
- **A denied approval:** in its `blocked` reply file the owner says
  Block, and the agent reviews without ESLint and says so.

`gallery_test` runs all three on the page in CI, against the live
sources. The no-tools refusal is tested against a stub registry
(`load_plugin_test`).

**Done (stage 2, pi and DSH packages; §14, "Stage 2: pi and DSH
plugins"):**
- **What it does:** `find_plugin` searches npm for pi and DSH packages,
  and `load_plugin` starts each in a runtime of its own, with its tools
  only.
- **The demo:** the gallery's `project-notebook` finds and loads
  `pi-memory`.
  - **Live:** on 2026-10-28 one session remembered a decision ($0.0045)
    and a second recalled it ($0.0029).
  - **A refused plugin:** its `not-offered` reply file is refused before
    anything installs.
  - **A denied approval:** in its `blocked` reply file the agent answers
    without a memory and gives the unsaved note.
- **Tests:** `gallery_test` runs all three on the page in CI.

**Done (stage 3, remote MCP servers; §14, "Stage 3: remote MCP
servers"):**
- **What it does:** `find_plugin` searches the MCP registry for servers
  reached over https, and `load_plugin` connects to one. Nothing of it
  runs here. The owner names a credential's environment variable at the
  question; its value is never asked for, shown or given to the agent; a server that needs a sign-in (OAuth)
  is refused.
- **The demo:** the gallery's `docs-helper` connects to Context7 with no
  key and answers from a library's docs at the project's version.
  - **Live:** on 2026-10-30 it answered an Express 5 question from
    Context7's docs ($0.0046).
  - **A refused plugin:** its `not-offered` reply file names a server the
    registry does not return, refused before anything connects.
  - **A denied approval:** in its `blocked` reply file the agent answers
    without the docs and marks the answer from memory.
- **Tests:** `gallery_test` runs all three on the page in CI, against
  the live registry and Context7.

Later: rutis plugins (§14, "Later steps"), and a sign-in (OAuth) for
remote servers that need one.

## Open questions (for review)

1. **Name and home.** Decided: `yologdev/yobuild`, public.
2. **Publish to crates.io?** Decided: yes, from 0.1 (`cargo install
   yobuild`). The library (`yobuild::builtins`, `frontend`, `install`,
   `manifest`, `run`) is published with it, but with no API promise.
   Still open: should it be promised to people who would rather write
   Rust than a manifest?
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
7. **Windows.** Decided for 0.1: not supported. `run` and `check` say
   so, and CI keeps it compiling. rutis supports Windows. The terminal
   UI, the kept environment (`env -u`), `--measure` (`ps`) and the tests
   are Unix-only today.
8. **Models.** Answered by 0.1. The manifest takes:
   - yoagent's 11 providers;
   - any OpenAI-compatible endpoint (`base_url` + `api_key_env`);
   - local models (`ollama`, `local`).

   The gallery and the examples use `deepseek/deepseek-flash`. Claude
   was run live on 2026-10-30: `anthropic/claude-haiku-5-5` ran
   web-research, code-reviewer and docs-helper (on the page, a remote
   server loaded) unchanged but for `model`, each `Completed` with its
   report, $0.010 in all (their READMEs, "On Claude"). A local model has
   not been run at all (not yet tried: it needs a runner and a model that
   calls tools), and it gets no dollar budget, because it has no price.
   DeepSeek pricing: done (yoagent 0.25.6).
