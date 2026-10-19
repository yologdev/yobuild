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
called), at the version it tested: as of that date, so `yobuild check`
decides. Tool names and arguments: `yobuild check --json` lists them
(`agent.tools[].parameters`).

### pi (`[[plugin]] pi = "…"`)

| Capability | Package | What yoagent gets |
|---|---|---|
| a plan / to-do list | `@juicesharp/rpiv-todo@2.12.0` **CI** | the tool `todo` |
| web search and pages | `pi-web-access@0.41.0` | 4 tools (untested by yobuild's CI: `check` first) |
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
| web search | `dsh-free-search@0.8.1` **CI** | with the tested config, `config = { provider = "bing", disabledEngines = [], bingMarket = "en-US" }` (keep it): `multi_search`, `research`, `advanced_search`, `domain_search`, `platform_search`, `video_search`, `cache_search` — allow these seven — and `free_search_test` (leave it asking). `research` searches and reads the top pages; `multi_search` compares many; `advanced_search` filters by time; `domain_search` looks up public sources (package registries, end-of-life dates, arXiv, …), not one site — `platform_search` is one site (Wikipedia, GitHub, …). It keeps a cache of the pages it read, outside the agent's folder: `~/.cache/dsh-free-search/research.db` (`RESEARCH_CACHE_DIR` moves it), which `cache_search` reads back across runs — tell the person. It adds prompt guidance (text in the system prompt on using its tools: no need to repeat it). The searches return extracted page text; `check --json` describes each. No `[dsh] host_services` needed. 0.8.1 is the DSH base's own pin: another version is refused |
| ask the user (DSH's way) | `@deepseek-ai/dsh-tool-ask-user@0.2.0-rc.2` with `@deepseek-ai/dsh-user-questions@0.2.0-rc.2` **CI** | the tool `ask_user_question` — prefer `ask_owner` |
| spreadsheets and documents | `dsh-univer-office@0.3.7` | 14 tools (the matrix loaded it with DSH's host services: likely needs `[dsh] host_services = true`) |
| charts and diagrams | `@dfy-plugins/dsh-visualize@0.1.5` | 1 tool (likewise) |

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
`allow` the ones that only read. A well-known one:
`@modelcontextprotocol/server-filesystem` (`name = "files"`,
`args = ["<folder>"]`): with `yobuild check`, 1.0.0 lists `read_file`,
`read_text_file`, `read_media_file`, `read_multiple_files`,
`list_directory`, `list_directory_with_sizes`, `directory_tree`,
`search_files`, `get_file_info`, `list_allowed_directories` (reading:
allow these) and `write_file`, `edit_file`, `create_directory`,
`move_file` (writing: leave them asking). The agent sees them as
`files__<tool>`.
