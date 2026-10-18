# Parts: what each gives an agent

Pick the fewest parts that do the job: the built-ins first, then a
plugin, then an MCP server. Whatever you pick, `yobuild check` decides —
a plugin listed here can still need a setting this list does not know.

## The built-ins (`[builtins]`)

| Need | Part | What it does |
|---|---|---|
| ask the owner without stopping | `ask_owner = true` | the tool `ask_owner` (question, why, options): it returns at once with a number; the answer arrives later as the agent's next message |
| a required ending | `report = "handoff"` or `["Done", "Next"]` | the run is not done until the last answer names every section (else it is sent back, 3 times, then fails) |
| what changed | `facts = true` | after each run: the git diff since the start, new commits and files, tokens, tool calls, open questions |
| approval before risky calls | `gate = "allow-list"` (the default) | reading is free; `bash` commands and writes are judged; every other tool asks the owner unless an entry's `allow` names it |
| a place to work freely | `scratch = true` | a fresh temp folder (`{scratch}` in the prompt) the gate leaves alone |
| reading only | `tools = "read-only"` | yoagent's read, list and search, confined to the agent's folder |
| a spending cap | `[limits] budget_usd`, `session_budget_usd` | stops a run (or the session) at that many dollars |

## Plugins that load

From the compatibility matrix (yoagent's
`docs/evals/plugin-compatibility.md`, run 2026-10-16: each package loaded
unchanged, its tools listed, none called). **CI** marks a pin yobuild's
own CI runs (its examples) — the others are as of that date: pin the
version, then `yobuild check`.

### pi (`[[plugin]] pi = "…"`)

| Capability | Package | What yoagent gets |
|---|---|---|
| a plan / to-do list | `@juicesharp/rpiv-todo@2.12.0` **CI** | the tool `todo` |
| web search and pages | `pi-web-access@0.41.0` | 4 tools |
| delegate to sub-agents | `pi-subagents@0.78.0` | 4 tools, 2 skills (skills are not loaded by yobuild) |
| a wiki of notes | `openwiki@0.7.2` | 6 tools |
| code health (lint, types) | `pi-lens@4.4.3` | 8 tools |
| a plan-review policy | `@plannotator/pi-extension@0.28.22` | tool-call hooks (no tools) |
| ask the user (pi's way) | `@juicesharp/rpiv-ask-user-question@2.13.8` | 1 tool — prefer `ask_owner` |
| an MCP bridge | `pi-mcp-adapter@5.3.1` | 1 tool — prefer `[[mcp]]` |

Refused by yobuild's pi adapter (they hook what yoagent cannot enforce, or
need pi's app): `billion-context`, `pi-goal-x`, `@companion-ai/feynman`,
`pi-background-tasks`, `@narumitw/pi-usage`. Load but give nothing as
installed: `pi-mcp-extension`, `@langfuse/pi-observability-plugin`,
`@langchain/langsmith-pi-extension`.

### DSH (`[[plugin]] dsh = "…"`)

| Capability | Package | What yoagent gets |
|---|---|---|
| web search | `dsh-free-search@0.8.1` **CI** | with `config = { provider = "bing", … }`: `multi_search`, `research`, `advanced_search`, `domain_search`, `platform_search`, `video_search`, `cache_search` (and `free_search_test`), prompt guidance |
| ask the user (DSH's way) | `@deepseek-ai/dsh-tool-ask-user@0.2.0-rc.2` with `@deepseek-ai/dsh-user-questions@0.2.0-rc.2` **CI** | the tool `ask_user_question` — prefer `ask_owner` |
| spreadsheets and documents | `dsh-univer-office@0.3.7` | 14 tools |
| charts and diagrams | `@dfy-plugins/dsh-visualize@0.1.5` | 1 tool |

Most popular DSH plugins need DSH's own app (its web server, model layer,
agents): `yobuild check` refuses them with "DSH plugins wait for services
nobody provides". Some need only DSH's own host services: try
`[dsh] host_services = true`.

### rutis (`[[plugin]] rutis = "…"`)

Your own plugin, in TypeScript, written with the rutis SDK
(`@arcships/rutis`): `definePlugin({ inject: ['yoagent'], apply(ctx) {
ctx.use('yoagent').register(name, { tools(), call_tool() }) } })`. See
yobuild's `frontend/plugins/` for examples.

## MCP servers (`[[mcp]]`)

Any MCP server works: stdio (`npm = "<package>@<version>"`, run from
yobuild's cache) or HTTP (`url`). `check` lists its tools; `allow` the ones
that only read. A well-known one: `@modelcontextprotocol/server-filesystem`
(`args = ["<folder>"]`; read tools `read_text_file`, `list_directory`,
`search_files`, and writing ones to leave asking). Pin its current
version: `npm view @modelcontextprotocol/server-filesystem version`.
