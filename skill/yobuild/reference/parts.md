# Parts: what each gives an agent

Pick the fewest parts that do the job: the built-ins first, then a
plugin, then an MCP server. Whatever you pick, `yobuild check` decides —
a plugin listed here can still need a setting this list does not know.

## The built-ins (`[builtins]`)

| Need | Part | What it does |
|---|---|---|
| ask the owner without stopping | `ask_owner = true` | the tool `ask_owner` (`question`, optional `why` and `options`): it returns at once with a number; the answer arrives later as a message `[Owner's answer to #N] …` — steering a run going on, or starting the next |
| a required ending | `report = "handoff"` or `["Done", "Next"]` | the run is not done until the last answer names every section (else it is sent back, 3 times, then fails) |
| what changed | `facts = true` | after each run: the git diff since the start, new commits and files, tokens, tool calls, open questions |
| approval before risky calls | `gate = "allow-list"` (the default) | reading is free; `bash` commands and writes are judged; every other tool asks the owner unless an entry's `allow` names it |
| a place to work freely | `scratch = true` | a fresh temp folder (`{scratch}` in the prompt) the gate leaves alone |
| reading only | `tools = "read-only"` | yoagent's read, list and search, confined to the agent's folder |
| a spending cap | `[limits] budget_usd`, `session_budget_usd` | stops a run (or the session) at that many dollars |
| tools found while it runs | `[extend]` | `find_plugin` and `load_plugin`: the agent searches for an MCP server (or a pi or DSH package, or a remote server) it lacks and loads it for the session, the owner asked twice (below) |

## Plugins that load

**CI** rows are the pins yobuild's own examples run, checked by its CI on
every change: use these versions. The rest are from yoagent's
compatibility matrix (`docs/evals/plugin-compatibility.md`, run
2026-10-16: each package loaded unchanged, its tools listed, none
called), at the version it tested — re-checked through `yobuild check`
on 2026-10-22, where each loads with the tools listed (the matrix's
`dsh-free-search` 0.9.1 is refused: use the **CI** row's 0.8.1, the DSH
base's pin). As of that date, so `yobuild check` decides. Tool names and
arguments: `yobuild check --json` lists them (`agent.tools[].parameters`).

### pi (`[[plugin]] pi = "…"`)

| Capability | Package | What yoagent gets |
|---|---|---|
| a plan / to-do list | `@juicesharp/rpiv-todo@2.12.0` **CI** | the tool `todo` |
| web search and pages | `pi-web-access@0.41.0` | 4 tools (untested by yobuild's CI: `check` first). Its paid engines read their own keys (`TAVILY_API_KEY`, `BRAVE_API_KEY`, `EXA_API_KEY`, …): plugins start without the person's variables, so each needs `grants = { env = ["TAVILY_API_KEY"] }` (`check` notes what its code reads but does not get; every pi plugin of the agent takes the same `env` grant) |
| delegate to sub-agents | `pi-subagents@0.78.0` | 4 tools, 2 skills (skills are not loaded by yobuild) |
| a wiki of notes | `openwiki@0.7.2` | 6 tools |
| a plan-review policy | `@plannotator/pi-extension@0.28.22` | tool-call hooks (no tools) |
| ask the user (pi's way) | `@juicesharp/rpiv-ask-user-question@2.13.8` | 1 tool — prefer `ask_owner` |
| an MCP bridge | `pi-mcp-adapter@5.3.1` | 1 tool — prefer `[[mcp]]` |
| code health: type errors, lint, an outline of the project | `pi-lens@4.4.3` **CI** | **needs `grants = { trust = true }`** — without it pi-lens starts no language server, and its diagnostics say "unavailable" (never a finding). 8 tools: `lens_diagnostics` (`{source: "lsp", scope: "paths", paths: [...]}` probes files; the session cache alone is not proof of clean), `project_report`, `symbol_search`, `module_report`, `read_symbol`, `read_enclosing`, `effective_config`, `pi_lens_activate_tools` — all read; leave `pi_lens_activate_tools` asking: it changes which tools the run is offered. Grant `model = "info"` too: it reads `ctx.model`. In the gallery (`code-health`): a TypeScript type error found (`typescript:2345`), checked by a reply file's `expect`. It writes nothing in the project: its state goes to `~/.pi-lens` (about 157 MB after a first run, its servers included; the pi runtime about 800 MB of memory), but it runs `git status` and the like there, which may refresh `.git/index` — tell the person. What `trust` lets it do — tell the person: it downloads and runs language servers and scanners (from GitHub and npm) into `~/.pi-lens`, and runs them on the project, the project's own code among what they load. Its first `lens_diagnostics` in a fresh `~/.pi-lens` downloads about 87 MB — `typescript` and `typescript-language-server` from npm at unpinned `^` ranges, `opengrep` and `typos-lsp` from GitHub — and takes about 25 s; later ones about 17 s, the server starting. **In every project folder new to it, its first `lens_diagnostics` may find nothing** — a second call on the same paths finds: in a reply file, call it twice everywhere you call it, the `expect` on the second; in the prompt, tell the agent the same. Each probe waits 15 s a file for the server by default (`waitMs`), and a server still starting — its first download, a slow machine (CI) — outlasts that: give `waitMs: 60000` on every probe, in reply files and in the prompt. **With a TypeScript on `PATH`** (`npm i -g typescript`, some CI images), pi-lens 4.4.3 takes it instead of installing its own and its TypeScript server is unavailable: every probe comes back unconfirmed (its log, `~/.pi-lens/sessionstart.log`, says "ensure typescript: already available at tsc" then "lsp spawn typescript: unavailable"); it has no setting for it — tell the person, and run it where no global `tsc` is on `PATH`. Read its `Outcomes:` line: `clean=N` or `findings=N` is an answer; `inconclusive`, `unavailable` or "Checks not confirmed" is not — never "clean", say it under what was not checked. `symbol_search` on a fresh project may answer "Word index is building … retry this query shortly", and `project_report` "No review graph cached for this workspace yet — a build was kicked off in the background; retry this call shortly" (its graph starts building at that first call, in every new folder): in a reply file, call both early (their `✗` is expected) and `project_report` again near the end, with the `expect`. `read_enclosing` finds nothing for a top-level statement ("No enclosing symbol/callback found": read the line with `read_file` instead), and `module_report`'s `summary` lists functions and classes, not exported constants (add a `search` for `export` to an outline). Two identical calls in a row are fine; a third trips yoagent's loop check. Its `PI_LENS_DISABLE_LSP_INSTALL` / `PI_LENS_DISABLE_TOOL_INSTALL` reach it only granted (below). Where toolchains live (`GOPATH`, `CARGO_HOME`, `JAVA_HOME`, `VIRTUAL_ENV`, …) is kept for it; its own settings (`PI_LENS_*`, `PILENS_*`) need `grants = { env = ["PI_LENS_*", "PILENS_*"] }` (on every pi plugin of the agent alike). Its 4 skills are not loaded |

For linting with no language servers to install, ESLint's MCP server
(below).

Refused by yobuild's pi adapter (they hook what yoagent cannot enforce, or
need pi's app): `billion-context`, `pi-goal-x`, `@companion-ai/feynman`,
`pi-background-tasks`, `@narumitw/pi-usage`. Load but give nothing as
installed: `pi-mcp-extension`, `@langfuse/pi-observability-plugin`
(its `LANGFUSE_PUBLIC_KEY` / `LANGFUSE_SECRET_KEY` / `LANGFUSE_HOST` need
an `env` grant, the same on every pi plugin), `@langchain/langsmith-pi-extension`.

### DSH (`[[plugin]] dsh = "…"`)

| Capability | Package | What yoagent gets |
|---|---|---|
| web search | `dsh-free-search@0.8.1` **CI** | with the tested config, `config = { provider = "bing", disabledEngines = [], bingMarket = "en-US" }` (keep it): `multi_search`, `research`, `advanced_search`, `domain_search`, `platform_search`, `video_search`, `cache_search` — allow these seven — and `free_search_test` (leave it asking). `research` searches and reads the top pages; `multi_search` compares many; `advanced_search` filters by time; `domain_search` looks up public sources (package registries, end-of-life dates, arXiv, …), not one site — `platform_search` is one site (Wikipedia, GitHub, …). It keeps a cache of the pages it read, outside the agent's folder: `~/.cache/dsh-free-search/research.db` (`RESEARCH_CACHE_DIR` moves it, granted: `grants = { env = ["RESEARCH_CACHE_DIR"] }`, the same on every DSH plugin of the agent), which `cache_search` reads back across runs — tell the person. The cache uses `node:sqlite`, so Node prints an `ExperimentalWarning` about SQLite on stderr: harmless, not a failure. It adds prompt guidance (text in the system prompt on using its tools: no need to repeat it). The searches return extracted page text; `check --json` describes each. No `[dsh] host_services` needed. 0.8.1 is the DSH base's own pin: another version is refused |
| ask the user (DSH's way) | `@deepseek-ai/dsh-tool-ask-user@0.2.0-rc.2` with `@deepseek-ai/dsh-user-questions@0.2.0-rc.2` **CI** | the tool `ask_user_question` — prefer `ask_owner` |
| spreadsheets, documents, slides | `dsh-univer-office@0.3.7` **CI** | **needs `grants = { workspace = true }`** (its tools read the calling agent: without it every call fails `SESSION_SCOPE_UNAVAILABLE`). 14 tools on a `.univer` file in the agent's folder, edited in drafts ("worktrees"): `univer_new` (a new `.univer`, never overwriting), `univer_import` (a CSV, xlsx, docx, pptx as a new unit in a draft), `univer_unit`, `univer_status`, `univer_inspect`, `univer_api`, `univer_lint` — these change no existing file of the person's: allow them. Leave asking: **`univer_execute`** (runs JavaScript with Node's whole power — it can read or write any file and reach the network: never allow it), **`univer_export`** (writes the user-facing file, `.xlsx` and the like, and **overwrites one that exists** without a word: keep it asking, and have the prompt ask with `ask_owner` before exporting over an existing file), `univer_worktree` (its `merge` and `discard` change the `.univer`'s main version — and `allow` is per tool, so its `create`, step 2 of every flow, asks too), `univer_print_pdf`, `univer_compile_svg`, `univer_screenshot`, `univer_resources` (they write files). Its workflow lives in 8 DSH skills, which yobuild does not load (`check --json` shows `guidanceChars: 0`): write it into the prompt — new (or reuse) the `.univer`, `univer_worktree` create, `univer_import`, `univer_status` / `univer_inspect`, `univer_execute`, `univer_export`. Its tools take ids from earlier results (`wt-…`, `u-…`): a reply file can call them only up to the first one that needs such an id (`scripted.md`). So a run puts about three Allow/Block questions to the person — the draft (`univer_worktree`), each `univer_execute`, the export — and the totals or any computed sheet exist only through `univer_execute`: tell the person, before the live run, that approving it runs the code shown, with their machine's full power. The `.univer` working file stays next to the person's files (name it after the input, and say so in the report). It starts a local helper process (127.0.0.1) on the first call that needs it, writes a state file in the DSH home, and its runtime takes up to about 1 GB of memory |

Not usable in yobuild, checked by hand on 2026-10-19 — do not offer it:
`@dfy-plugins/dsh-visualize@0.1.5` (charts) does not install: its peers
conflict with yobuild's DSH base (npm `ERESOLVE`). A clean `check` shows that a plugin loads, not that its
tools work: the scripted run calls them.

Most popular DSH plugins need DSH's own app (its web server, model layer,
agents): `yobuild check` refuses them with "DSH plugins wait for services
nobody provides". Some need only DSH's own host services (files,
attachments, credentials, storage, sessions, skills): set
`[dsh] host_services = true`, as `check`'s fix says.

### rutis (`[[plugin]] rutis = "…"`)

A tool of your own, in TypeScript with the rutis SDK: for the one thing no
listed part does (reading git history in a read-only agent, an exact
calculation, a private API). Keep it small: one plugin, a few tools.

**Where it lives.** In the agent's folder, in an npm package of its own,
say `plugins/`:
- `plugins/package.json`: `"type": "module"`, `"private": true`, and
  dependencies `@arcships/rutis` (the SDK the plugin imports) and
  `@arcships/rutis-runtime` (the runtime yobuild starts it in), both at
  exactly `0.12.0`;
- `plugins/package-lock.json`, from `npm install --ignore-scripts` there.
  The runtime brings `tsx`, `typescript` and `esbuild`; all of them work
  without install scripts.
- the plugin file, `plugins/<name>.ts`.

Keep `package.json` and the lock with the agent, not `node_modules` (a
`.gitignore` beside `agent.toml` holding `plugins/node_modules/`). This
package is the one install yobuild does not do: the person runs `npm ci
--ignore-scripts` in `plugins/` once, and again after `yobuild new --from`
copies the agent (it prints the command). Until then `run` and `check`
refuse, naming the command. Say it in the
README's "What it needs". (The rule "never install plugin packages by
hand" is about pinned plugins, which yobuild installs.)

```ts
import { definePlugin } from '@arcships/rutis'
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'

const run = promisify(execFile)
// Read-only by construction: no pager, no lock file, and the repository's
// (and the person's) config kept from running programs or changing output.
const GIT = ['--no-pager', '--no-optional-locks', '-c', 'log.showSignature=false', '-c', 'column.ui=never']

export default definePlugin({
  inject: ['yoagent', 'host_workspace'],   // `yoagent`: required; the rest: services its grants allow
  apply(ctx: any) {
    // register(name, handler) returns a disposer; ctx.effect calls it when the plugin unloads.
    ctx.effect(ctx.use('yoagent').register('git-history', {
      async tools() {
        return [{
          name: 'git_log',
          description: 'Commits since a tag, newest first',
          parameters: { type: 'object', properties: { since: { type: 'string' } }, additionalProperties: false },
        }]
      },
      async call_tool(call: any) {               // call.tool: the name; call.args: its arguments
        const since = (call.args ?? {}).since
        if (since !== undefined && (typeof since !== 'string' || !/^v\d+\.\d+\.\d+$/.test(since))) {
          return { text: `not a version tag (vX.Y.Z): ${JSON.stringify(since)}`, is_error: true }
        }
        const cwd = await ctx.use('host_workspace').cwd()   // the agent's folder; needs `workspace`
        const range = since ? [`refs/tags/${since}..HEAD`] : ['HEAD']
        const { stdout } = await run('git', [...GIT, 'log', '--format=%h %s', ...range, '--'],
          { cwd, signal: call.signal, maxBuffer: 1 << 20 })   // a stopped run aborts it; a throw is a failed call
        return { text: stdout || 'no commits' }
      },
    }))
  },
})
```

```toml
[[plugin]]
rutis   = "plugins/git-history.ts"
runtime = "node"
grants  = { workspace = true }     # host_workspace.cwd(); see manifest.md's grants
allow   = ["git_log"]              # it only reads: let it run without asking
```

**The contract.**
- `tools()` returns `{ name, description, parameters }` per tool,
  `parameters` a JSON Schema object. It is asked at each run.
- `call_tool(call)`:
  - `call.tool` is the tool's name.
  - `call.args` holds the arguments as they reach the plugin: the
    model's, perhaps rewritten by an earlier handler. Read them as
    `call.args ?? {}`, and check each.
  - `call.signal` aborts when the run is stopped. Pass it on (`execFile`
    and `fetch` take a `signal`); a tool that ignores it runs to the end,
    side effects included.

  It returns one of:
  - a string;
  - `{ text }`, or `{ content: [blocks] }` (not both);
  - with `is_error: true`, the text becomes the error the model sees
    (`✗`);
  - `details`, optional: data for the page, not for the model.

  Any other key or shape is refused as "`call_tool` returned an
  unexpected value". A throw is a failed call too.
- **Its arguments come from the model.** Check each one's type and shape
  before you use it. Never pass one to a shell. Give a program fixed
  arguments, with a value never in an option's place: a `since` of
  `--output=/tmp/x` must be refused, not handed to `git`.
- **Host services through grants.** Add the service to `inject` and call
  it through `ctx.use(…)`, with its grant on the `[[plugin]]`:
  - `host_workspace`: `cwd()`, the agent's folder (`workspace = true`).
    The plugin's own `process.cwd()` is not the agent's folder. The rest
    of the grants are in `manifest.md`.
- **It is the person's code.** The gate does not judge what the plugin
  runs. Its `process.env` is the kept environment (`PATH` and `HOME`
  among it), plus what its `env` grant passes. Read-only, as in the
  example, is a property of the code you write.
- **A failure must not look like an answer.** Say what was cut or left
  out, never "none":
  - **Errors:** report the error's own line. For git, that is its
    `fatal:` or `error:` line, not its first, which is often a
    `warning:`.
  - **Exit codes:** read an exit code with the output it comes with.
    `git rev-parse --verify -q HEAD` means "no commit yet" only when it
    exits 1 with nothing on stderr.
  - **Size:** cap what a call returns (say 50 KB, and so many files per
    item), and say when it was cut.
  - **Parsing:** check each record has the fields you expect. A commit
    message can hold any byte but NUL, so a separator you chose may
    appear in it. Count the records you could not read, and say so.
- **A tool that reads git**, in particular:
  - **Flags:** `--no-pager` and `--no-optional-locks` (git then writes
    no lock file, and no refreshed index). Use no subcommand that writes.
  - **Config:** the repository's config and the person's (`HOME` is
    kept) apply, and can run programs or change the output. Turn off
    what the command reads:
    - `-c log.showSignature=false` (gpg);
    - `-c column.ui=never` (several names on a line);
    - `-c core.fsmonitor=false`;
    - `--no-ext-diff --no-textconv` on `log -p` and `diff`;
    - `GIT_NO_LAZY_FETCH=1` in the environment the plugin gives git
      (`execFile`'s `env`: `{ ...process.env, GIT_NO_LAZY_FETCH: '1' }`),
      since a partial clone would fetch;
    - and whatever else the commands you run read: these are the usual
      ones, not all.
  - **Shallow clones:** a shallow clone has part of the history and
    tags. Check `git rev-parse --is-shallow-repository`, and say so.
  - **Refs outside HEAD:** a ref the model names may not be in HEAD's
    history. Check it with `git merge-base --is-ancestor <ref> HEAD`
    before reading `<ref>..HEAD`.
  - **Names you filter out:** a tag that does not match your pattern
    (`1.2.3`, `v2.0.0-rc.1`) is not "no tag". List those left out.
- **Trying it:**
  - `check --json` lists its tools and their kinds.
  - A scripted reply file `expect`s its results (`scripted.md`).
  - For a state the sample project is not in (no tag yet, an empty
    file), make a copy of the sample (`cp -R`), change the copy into that
    state, and write a reply file of its own for it. A reply file's
    `expect`s hold for its own sample only.
  - **A reply file for refusals:** calls meant to fail, such as a bad
    argument (`--output=/tmp/x` where a name goes) or a name that does
    not exist. It `expect`s each error, to show the refusals hold.

yobuild's own plugins are named, not given a path:
- `rutis = "yobuild:todo-plan"` draws a pi to-do plan (`rpiv-todo`'s
  `todo`) on the web page;
- `rutis = "yobuild:search-links"` turns search results into links on the
  page;
- `pi = "yobuild:confirm-dangerous"` (with `policy = true`) is a deny-list
  that asks before `rm -rf` and the like.

The first two are UI plugins: they offer no tools.

## MCP servers (`[[mcp]]`)

Any MCP server works: stdio (`npm = "<package>@<version>"`, run from
yobuild's cache) or HTTP (`url`, and a `name`). `check` lists its tools;
`allow` the ones that only read.

| Capability | Package | What yoagent gets |
|---|---|---|
| lint with the project's ESLint config | `@eslint/mcp@0.3.15` **CI** | with `name = "eslint"`: `lint-files` (`filePaths`, absolute: build them from `{cwd}`) — it only reports, allow it. It runs the project's `eslint.config.*` (that file's code runs) with its own ESLint 10; the plugins a config imports (typescript-eslint) come from the project's `node_modules`, so a project that has not run `npm install` gets an import error; without a config, `✗ Could not find config file.`; a file type the config does not cover, `File ignored because no matching configuration was supplied` (`.ts` needs typescript-eslint). Its results say to ask the user before fixing: a read-only agent cannot fix anyway |
| files in a folder | `@modelcontextprotocol/server-filesystem@1.0.0` **CI** | with `name = "files"`, `args = ["."]`: `read_file`, `read_text_file`, `read_media_file`, `read_multiple_files`, `list_directory`, `list_directory_with_sizes`, `directory_tree`, `search_files`, `get_file_info`, `list_allowed_directories` (reading: allow these) and `write_file`, `edit_file`, `create_directory`, `move_file` (writing: leave them asking; no delete), seen as `files__<tool>`. `search_files` matches file names (a glob), not contents: to search inside files, yoagent's `search`. `read_multiple_files` succeeds when some reads inside it fail (`✓` proves nothing about one file: `read_text_file` does). `edit_file` with `dryRun: true` writes nothing, yet asks all the same. 2.0.0 came out on 2026-10-19, unchecked: `check` it before moving |

A stdio server's `args` reach it as written: a relative path (`"."`) is
the folder `yobuild run` starts in — the server runs there, as the agent
does — not the manifest's folder. `yobuild check` starts the server too,
in the folder `check` runs in: run it from a folder the server may touch.

## Servers the agent finds itself (`[extend]`)

When the person cannot say in advance which tool a task will need, give
the agent `[extend]` instead of guessing a server (manifest.md has the
keys). It gets two tools, both allowed:

- `find_plugin(need)` searches the MCP registry and npm, live, for MCP
  servers over stdio, and returns up to 10 candidates with what their
  packages claim (unverified) and the version it would take. It runs
  nothing. Search with the tool's own name as packages spell it: the need
  `"eslint"` finds ESLint's server (`@eslint/mcp`), `"lint TypeScript"`
  does not — its name and description hold neither word as written.
  The candidates can include unrelated packages: the agent judges them
  (in the prompt: take the tool's own server, `@eslint/mcp` for ESLint).
  When the person cannot name the tool in advance, the prompt has the
  agent learn it from the project: the config it finds
  (`eslint.config.js` → `"eslint"`, `biome.json` → `"biome"`).
- `load_plugin(package, name?, args?)` loads one candidate for the
  session: give no `version` (it takes the one `find_plugin` offered).
  The person is asked twice — before anything of it runs, then with its
  tools in front of them — and its tools are the agent's from its next
  step as `<name>__<tool>`, each call asked. The default `name` is the
  package's without its scope and `mcp`/`server` words, or the scope when
  nothing else is left: `@eslint/mcp` loads as `eslint`, its tool
  `eslint__lint-files`. A package `find_plugin` did not return is refused
  before anything installs. The version taken is the one offered that
  day, which can be newer than a pin listed above.

When `kinds` lists `"pi"` or `"dsh"` (alone or beside `"mcp"`),
`find_plugin(need, kind)` searches npm for pi packages (the `pi` field or
the keyword `pi-package`) or DSH packages (the `dsh` field or
`dsh-plugin`), and `load_plugin` loads one as it loads a server, with no
`args`. A loaded package runs in a runtime of its own with its tools
only: its hooks are not called (a pi extension's own `tool_call` policy
still judges its own tools' calls), and it gets no grants, no `config`
and no host services. So a package that needs one — pi-lens's `trust`,
a DSH package's `workspace` or DSH's host services (most DSH packages) —
goes in the manifest, where it runs with its hooks and grants; so does a
rutis plugin, which `[extend]` does not find. Of the pi packages checked
on 2026-10-28, `pi-web-access`, `pi-memory`, `@juicesharp/rpiv-todo` and
`@hypabolic/pi-hypa` load this way; one that asks through pi's dialogs
(`rpiv-ask-user-question`) gets no one to ask. Its tools are
`<name>__<tool>` as a server's, the default `name` made as a server's
(without the scope: `@juicesharp/rpiv-todo` loads as `rpiv-todo`;
`pi-memory` as `pi-memory`, its tools `pi-memory__memory_write` …), and the questions name it: "Start the pi package …", "Keep the pi
package …".

- `pi-memory` (checked 2026-10-28, 0.4.2): `memory_write`,
  `memory_read`, `memory_forget`, `memory_restore`, `memory_status`,
  `scratchpad` and `memory_search`. It keeps plain Markdown in
  `~/.pi/agent/memory/` — the person's home, outside the agent's folder,
  shared with pi itself if they use it — so what one session writes the
  next can read. Loaded, its recall at the start of each turn (a hook)
  does not happen: the prompt has the agent call `memory_read` (or
  `memory_status`) before it relies on what it remembered.
  `memory_search` needs `qmd` installed (else it says so); `memory_read`
  works without it. Its arguments, from its published code:
  `memory_write {"target": "long_term" | "daily", "content": "<Markdown>",
  "mode": "append" | "overwrite"}` (`mode` for `long_term`, `append` by
  default; the result begins `Appended to MEMORY.md`, or `Appended to
  daily log: …`), `memory_read {"target": "long_term" | "scratchpad" |
  "daily" | "list", "date": "YYYY-MM-DD"}` (the file's text). One memory
  serves every project the person uses it in (one `MEMORY.md`; its folder
  is `PI_MEMORY_DIR` when that is set — a loaded package does not get it
  — else `.pi/agent/memory` under `HOME`): a
  notebook for several projects tags each note with the project (`#tags`
  in `content`, which pi-memory suggests) and reads back only its own. Its writes ask, as every loaded tool's call does.
  Put in the manifest instead (the `[[plugin]]` block yobuild prints at
  the end), it runs with its hooks: what it remembers is added to each
  turn (with today's and yesterday's daily log), and at the session's
  end it tries to write a summary to the daily log with the session's
  model (on by default in pi-memory). Under yobuild that summary is not
  written: a pi extension is told which model runs only with `model =
  "info"`, and never given a key to call it with (pi-memory logs that it
  could not resolve one). `PI_MEMORY_EXIT_SUMMARY=0`, passed by an `env`
  grant, turns the attempt off. Its writes then run without asking only if
  `allow` names them.

When `kinds` lists `"remote"`, `find_plugin(need, kind "remote")`
searches the MCP registry only, for servers that run elsewhere and are
reached over https (streamable HTTP; SSE-only ones are left out, and so
are addresses that take values or name this machine or its network).
Nothing of a remote runs here: each call sends its arguments to the
server's host, which answers as it likes that day. `load_plugin` takes
the registry's name (`io.github.upstash/context7`), no `version` and no
`args`; the default `name` is the name's own part without its
`mcp`/`server` words (`context7`) — the namespace's last label when
nothing else is left (`com.notion/mcp` → `notion`), a `.` made `-` —
its tools `context7__<tool>`. The
person is asked "Connect to the remote MCP server … at <host> as …?",
then, for each header the server takes a credential in (required, or
optional and secret), which environment variable holds it — never the
value: the variable is set where yobuild runs, and its value is sent to
that host only, said nowhere. A variable yoagent reads for a model's key
(`DEEPSEEK_API_KEY` …) is refused there, as is one not set; each header
is asked up to 3 times. An empty answer leaves an optional header off
and refuses a server whose header is required; an optional header still
without a usable variable after 3 answers is not sent (a Warning). Then "Keep the remote MCP server …? It
offers: …". A server that needs a sign-in (OAuth) is refused at the
connection (HTTP 401 or 403): yobuild cannot sign in yet. The block printed at
the end is an `[[mcp]]` with `url` and, for a credential, `headers`
naming its variable.

- Context7 (`io.github.upstash/context7` at
  `https://mcp.context7.com/mcp`, checked 2026-10-30): up-to-date
  documentation for libraries, without a key
  (its `Authorization` header is optional: answer the variable question
  with nothing; with a key, more requests a day). Search it as
  `find_plugin("context7", kind "remote")`: the registry also returns
  others' servers with "context7" in their names (forks, paid ones that
  need a key) — the prompt names `io.github.upstash/context7` as the one
  to take. Its tools: `resolve-library-id {"libraryName": "<name, as the
  library spells it: Next.js>", "query": "<what the person asks>"}`
  (both required; its result lists libraries as `Context7-compatible
  library ID: /org/project`, best match first, each with a line
  `Versions: v5.1.0, …` — Express is `/expressjs/express`), then
  `query-docs {"libraryId": "/org/project" or "/org/project/<version>"
  (the version as that line spells it: `/expressjs/express/v5.1.0`),
  "query": "<one concept>", "language": "<optional: TypeScript, Python,
  …>"}` (the docs' text and code snippets; for Express 5's async errors
  it begins `### Express Router Promise Support`). Call
  `resolve-library-id` first: the id comes from its result, unless the
  person gives one. A library's id is the same from run to run, so a
  reply file may write it; Context7 can still change what it lists any
  day, as a third party's server may. Each call sends the query to
  Context7: the prompt keeps the person's private code and secrets out
  of it.
- Which kinds to list in `[extend] kinds`: the one this list documents
  for the need (a memory: `"pi"`, pi-memory; a library's docs:
  `"remote"`, Context7). A kind with no documented package for it
  widens the search to packages you cannot script against before the
  page run; add it only when the person wants that choice.

Someone must be there to answer: the web page or the terminal. Under
`--once` `load_plugin` is refused (scripted.md). In the prompt, tell the
agent when to look for a tool (what it lacks, in the owner's words), to
search by the tool's name, to load at most what the task needs, and to
say in its report what it loaded and what it could not do when the person
said Block. Name the package the agent should prefer when this list
documents one for the need (`pi-memory` for a memory: its tools' names
and arguments are known, so the prompt and reply files can use them);
let it judge the candidates on what they claim only where none is
documented — and then the prompt cannot name their tools, so it says to
read them from `load_plugin`'s result. When the session ends yobuild prints the `[[mcp]]` or
`[[plugin]]` block that would keep each one loaded: the person decides
whether to add it to `agent.toml`.
