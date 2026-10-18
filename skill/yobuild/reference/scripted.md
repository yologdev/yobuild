# The scripted reply file (`--scripted`)

`yobuild run agent.toml --scripted script.json --once "<task>"` runs the
agent with a model that answers from the file, in order — no provider, no
key, no cost. Use it to see that the manifest works before a live run.

## The format

A JSON list. Each reply is exactly one of:

```json
{ "text": "the model's answer" }
{ "tools": [{ "name": "a_tool", "arguments": { "…": "…" } }] }
```

Nothing else: an unknown key is an error. A `tools` reply may call several
tools at once; `arguments` defaults to `{}`. Each reply is one model turn:
after a `tools` reply the tools run and the next reply is read.

```json
[
  { "tools": [{ "name": "list_files", "arguments": { "path": "." } }] },
  { "tools": [{ "name": "ask_owner", "arguments": { "question": "Which file first?", "options": ["a.md", "b.md"] } }] },
  { "text": "## Done\n- Listed the folder.\n## Next\n- Read a.md." }
]
```

Tool names are as the agent sees them: yoagent's (`bash`, `read_file`,
`write_file`, `edit_file`, `list_files`, `search`), `ask_owner`, a
plugin's own names, an MCP server's as `<server>__<tool>`.

## What to expect under `--once`

Each tool call prints `  name ✓` or `  name ✗ <why>` on stderr, then the
last answer and how the run ended on stdout (`Completed`, `Stopped: …`,
`Error: …`). Nobody is there to answer the gate or `ask_owner`:

- `bash`, the writers and editors, and any tool not allowed print
  `✗ Tool call denied: not allowed: the owner declined, or nobody
  answered` — expected, unless `allow` or the scratch should free it.
- `ask_owner` returns at once (`Asked the owner as #1 …`); the question
  stays open and is never answered.

## The traps

- **With `[builtins] report`**, the last `text` reply must name every
  section (anywhere, any case: `## Done` …). Else yobuild sends the model
  back, the model answers `(no more mock responses)` each time, and after
  3 continues the run ends `Error: [Extension failed: report] …`.
- **Running out of replies** without `report`: the model answers
  `(no more mock responses)` and the run ends `Completed` with that
  answer — check the last answer, not only the outcome.
- **Limits count**: a reply list longer than `[limits] max_turns` stops
  the run (`Stopped: [Agent stopped: Max turns reached …]`).
- **The agent's folder** is the one `yobuild run` starts in: paths in
  `arguments` are relative to it.
