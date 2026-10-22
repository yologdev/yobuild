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
| web search and pages | `pi-web-access@0.41.0` | 4 tools (untested by yobuild's CI: `check` first) |
| delegate to sub-agents | `pi-subagents@0.78.0` | 4 tools, 2 skills (skills are not loaded by yobuild) |
| a wiki of notes | `openwiki@0.7.2` | 6 tools |
| a plan-review policy | `@plannotator/pi-extension@0.28.22` | tool-call hooks (no tools) |
| ask the user (pi's way) | `@juicesharp/rpiv-ask-user-question@2.13.8` | 1 tool — prefer `ask_owner` |
| an MCP bridge | `pi-mcp-adapter@5.3.1` | 1 tool — prefer `[[mcp]]` |
| code health: type errors, lint, an outline of the project | `pi-lens@4.4.3` | **needs `grants = { trust = true }`** — without it pi-lens starts no language server, and its diagnostics say "unavailable" (never a finding). 8 tools: `lens_diagnostics` (`{source: "lsp", scope: "paths", paths: [...]}` probes files; the session cache alone is not proof of clean), `project_report`, `symbol_search`, `module_report`, `read_symbol`, `read_enclosing`, `effective_config`, `pi_lens_activate_tools` — all read. Checked by hand on 2026-10-21 (not in CI): a TypeScript type error found (`error[typescript] (2345)`). What `trust` lets it do — tell the person: it downloads and runs language servers and scanners (from GitHub and npm) into `~/.pi-lens`, and runs them on the project, the project's own code among what they load. Its first call in a fresh `~/.pi-lens` may come back "inconclusive" while those install (about 20 s); a second call finds. Its 4 skills are not loaded |

For linting with no language servers to install, ESLint's MCP server
(below).

Refused by yobuild's pi adapter (they hook what yoagent cannot enforce, or
need pi's app): `billion-context`, `pi-goal-x`, `@companion-ai/feynman`,
`pi-background-tasks`, `@narumitw/pi-usage`. Load but give nothing as
installed: `pi-mcp-extension`, `@langfuse/pi-observability-plugin`,
`@langchain/langsmith-pi-extension`.

### DSH (`[[plugin]] dsh = "…"`)

| Capability | Package | What yoagent gets |
|---|---|---|
| web search | `dsh-free-search@0.8.1` **CI** | with the tested config, `config = { provider = "bing", disabledEngines = [], bingMarket = "en-US" }` (keep it): `multi_search`, `research`, `advanced_search`, `domain_search`, `platform_search`, `video_search`, `cache_search` — allow these seven — and `free_search_test` (leave it asking). `research` searches and reads the top pages; `multi_search` compares many; `advanced_search` filters by time; `domain_search` looks up public sources (package registries, end-of-life dates, arXiv, …), not one site — `platform_search` is one site (Wikipedia, GitHub, …). It keeps a cache of the pages it read, outside the agent's folder: `~/.cache/dsh-free-search/research.db` (`RESEARCH_CACHE_DIR` moves it), which `cache_search` reads back across runs — tell the person. It adds prompt guidance (text in the system prompt on using its tools: no need to repeat it). The searches return extracted page text; `check --json` describes each. No `[dsh] host_services` needed. 0.8.1 is the DSH base's own pin: another version is refused |
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

Your own plugin, in TypeScript, written with the rutis SDK
(`@arcships/rutis`), in an npm package of its own (`"type": "module"`,
`@arcships/rutis-runtime` 0.12.0 installed):

```ts
import { definePlugin } from '@arcships/rutis'

export default definePlugin({
  inject: ['yoagent'],              // required: its tools are its own only through it
  apply(ctx: any) {
    ctx.effect(ctx.use('yoagent').register('my-tools', {
      async tools() {
        return [{ name: 'shout', description: 'Say it loudly', parameters: { type: 'object', properties: { text: { type: 'string' } } } }]
      },
      async call_tool(call: any) {            // call.tool, call.args, call.signal
        return { text: String(call.args.text).toUpperCase() }   // or a string; is_error: true for a failure
      },
    }))
  },
})
```

`[[plugin]] rutis = "plugins/shout.ts"`, `runtime = "node"`. (yobuild's
`frontend/plugins/` are UI plugins, which offer no tools.)

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
