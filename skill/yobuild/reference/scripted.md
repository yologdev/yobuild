# The scripted reply file (`--scripted`)

`yobuild run agent.toml --scripted scripts/demo.json --once "<task>"` runs
the agent with a model that answers from the file, in order — no provider,
no key, no model cost. Use it to see that the manifest works before a live
run. Only the model is scripted: plugins and servers run for real (a
search tool really searches, a server really starts), so it may need the
network.

## The format

A JSON list. Each reply is exactly one of:

```json
{ "text": "the model's answer" }
```

```json
{ "tools": [{ "name": "a_tool", "arguments": { "path": "notes.md" } }] }
```

A call may also say what its result must contain, with `expect` (a text,
or a list of texts that must all appear):

```json
{ "tools": [{ "name": "files__read_text_file", "arguments": { "path": "{cwd}/notes.md" }, "expect": "groceries" }] }
```

Nothing else: an unknown key is an error, and an empty `expect` too.
`{cwd}` in an argument's text is
the folder the run starts in, as in the prompt — for a tool that takes
absolute paths (`"{cwd}/src/index.ts"`), so the reply file runs in any
folder. A `tools` reply may call several
tools at once. Give `arguments` as the tool's schema asks (`check --json`
lists each tool's `parameters`); `{}` for a tool that takes none — left
out, it is `null`, which some tools refuse. Each reply is one model turn:
after a `tools` reply the tools run and the next reply is read; a `text`
reply ends the run.

An example, for a manifest with `ask_owner = true` and `report = ["Done",
"Next"]`:

```json
[
  { "tools": [{ "name": "list_files", "arguments": { "path": "." } }] },
  { "tools": [{ "name": "ask_owner", "arguments": { "question": "Which file first?", "why": "Both look current.", "options": ["a.md", "b.md"] } }] },
  { "text": "## Done\n- Listed the folder; asked which file first (#1).\n## Next\n- Read the one the owner picks." }
]
```

Tool names are as the agent sees them (`check --json`'s `agent.tools`):
yoagent's (`bash`, `read_file`, `write_file`, `edit_file`, `list_files`,
`search`; with `tools = "read-only"` only the three that read), `ask_owner`,
a plugin's own names, an MCP server's as `<server>__<tool>`.

## What to expect under `--once`

`<name>: ready in 1.2s` on stderr once everything has loaded. Each tool
call prints `  name ✓` or `  name ✗ <why>` there; notices as
`  Info: …` / `  Warning: …` — with a budget, a Warning `budget_usd not
kept: a scripted model has no price` (`budget_usd and session_budget_usd
not kept: …` with both; the script has no price, not the model); with
plugins, an Info `Plugin time this run: …`; when the run
ends, `  Cost: not priced (0 tokens in, 0 out; N turns)` (a reply file
reports no usage; a live run says its dollars and tokens there); then the last answer and how the run ended on
stdout (`Completed`, `Stopped: …`, `Error: …`). A `✓` says the tool ran,
not what it found: give the calls that matter an `expect`. A result that
does not contain an expected text becomes an error the model sees,
printed ``  <tool> ✗ the reply file expected `X` in the result: <its first
line>`` — the run goes on and can still end `Completed`, so read the `✗`
lines. A call that never ran (denied, or no such tool) has its `expect`
said as a Warning: ``the reply file's expectation for `x` was not
checked: the call did not run``. To see
whole results, run the reply file without `--once` and open the page — or
make the files a run reads such that only the right one can succeed (a file only in the folder the run starts in shows
that a server's `"."` is that folder). A denied call also writes a log
line (`WARN … tool call denied by middleware …`) beside its `✗`. A reply
file's `text` is invented, sources and all
(a placeholder is fine): it shows how the run ends, not what the tools
found. Nobody is there to answer:

- A call the gate would ask about prints `✗ Tool call denied: not
  allowed: the owner declined, or nobody answered`: a `bash` command it
  cannot read, a write over the owner's files or outside the project, a
  plugin's or server's tool not allowed. A `bash` command it can read
  (`ls`, `cat`, `git status`, the project's tests), a new file in the
  project, the agent's own files and the scratch folder run: `✓`.
- `ask_owner` prints `✓`: the question stays open and is never answered.
  To try what happens once it is answered (name the files by what they
  show: `scripts/broad.json`, `scripts/answer.json`), run a second reply
  file with
  the answer as the task, in the form yobuild sends it — the question as
  asked, whole, then the answer on a line of its own:
  `[Owner's answer to #1] <the question>⏎<the answer>`. Unlike a real
  answer, this run has none of the first run's history: the model sees
  only that message.
- A tool that runs and fails (a search engine down, a rate limit, a page
  not found) prints `✗ <its own error>` — not `Tool call denied: …`. That
  is the tool's day, not the manifest: run again, or script around it.
- A tool the agent does not have (one not in `agent.tools`): `✗` —
  `Tool call denied: …` when the gate asks about it first (nobody
  answers), or `Tool <name> not found` when the gate would let it run (one
  of yoagent's writers with `tools = "read-only"`, say).
- With `[extend]`, `find_plugin` searches the live sources (the
  network, as a search plugin does) and prints `✓`; `load_plugin` prints
  `✗ load_plugin refused it: not under --once: nobody is there to allow it
  (nothing was counted)`, after a Warning notice saying the same. That
  refusal comes before every other, so under `--once` every `load_plugin`
  call looks alike, whatever the reply file means to show. A call to the
  tools it would have loaded prints `✗ Tool call denied: …`, and its
  `expect` the Warning "… was not checked: the call did not run". All of
  these are expected under `--once`: it shows the search and the agent's
  proposal; the rest is seen on the page (below).
- A tool that reports (a linter, diagnostics, a type check) can print
  `✓` with a false "clean": a tool that cannot do its work may say it
  found nothing. Give it a file with a known problem and look for that
  finding — on the page, or in a live run's answer — before relying on it.

## The traps

- **A tool that warms up.** Each run of a reply file starts in a folder
  the plugin may never have seen: a call that builds an index or starts a
  language server can answer "retry shortly", or nothing, the first time.
  Do in every reply file what `parts.md` says for that part (pi-lens:
  `lens_diagnostics` twice, the `expect` on the second) — one reply file
  passing does not mean the others will.
- **With `[builtins] report`**, the last `text` reply must name every
  section (anywhere, any case: `## Done` …; `report = "handoff"` needs
  Done, Waiting on you, Decisions, Next). Else yobuild sends the model
  back, the model answers `(no more mock responses)` each time, and after
  3 continues the run ends `Error: [Extension failed: report] …`.
- **Running out of replies** without `report`: the model answers
  `(no more mock responses)` and the run ends `Completed` with that
  answer — check the last answer, not only the outcome.
- **The same call over and over** (same tool, same arguments): after 3
  in a row yoagent asks the agent to change approach, then stops the run
  (`Stopped: [Agent stopped: repeated tool call — …]`). Vary the replies.
- **Limits count**: more `tools` replies than `[limits] max_turns` (50
  by default) stop the run (`Stopped: [Agent stopped: Max turns reached
  …]`).
- **The agent's folder** is the one `yobuild run` starts in: paths in
  `arguments` are relative to it. To keep the person's sample files apart
  from the agent's own, run from a work folder beside it: `cd work`, then
  `yobuild run ../agent.toml --scripted ../scripts/demo.json --once
  "<task>"`.
- **Ids from earlier results** (a tool that returns `wt-…` or `u-…` that
  the next call needs) cannot be written in a reply file: the replies are
  fixed before the run. Call such tools only up to the first one that
  needs an id, or that asks (nobody answers under `--once`), and end
  there; the rest of the flow is shown by the live run. Do not reach into
  a plugin's own files for ids to make a reply file pass.
- **A run that leaves files behind** (a new `.univer`, a cache) changes
  the next run's start: a tool that refuses to overwrite fails the second
  time. Start each scripted run from the same files.

## A reply file that loads a server (`[extend]`)

`find_plugin`'s candidates come from the live sources and change: give
its call an `expect` that holds as they do — the package's name, never a
version, a count or a position — and give `load_plugin` no `version` (it
takes the one offered). The next reply calls the loaded tools by
`<name>__<tool>` (parts.md: the default `name`), with arguments their
schema takes — `check` cannot list them, as the server is not known
before the run; parts.md has those of the servers listed there. To see
them, and that the call and its `expect` hold, put the server in a
throwaway copy of the manifest as an `[[mcp]]` entry at the version
`find_plugin` offers (in a folder of its own, outside the agent's), `check`
it and run that one call scripted; then delete the copy. Run it
without `--once`, on the page: the person answers both questions
(start it, then keep it) and each call of its tools. Under `--once` the
same file stops at `load_plugin`'s refusal: that run shows the search.

Do not answer the page yourself (no script or client of your own
playing the person): run each reply file under `--once`, then tell the
person how to run it on the page and what to answer — Allow, Allow and
Allow for each call; or Block — and ask what they saw.

Three reply files show the shape of a run with `[extend]`:

- `scripts/demo.json`: the load allowed, its tools used (each call asked
  and allowed);
- a package `find_plugin` did not return, refused before anything
  installs (`… is not among this session's candidates …`): the agent
  goes on without it. The task names the package (the owner's friend
  recommended it, say); the prompt has the agent search for a package
  the owner names, propose it, and load nothing in its place when it is
  refused;
- the person says Block: the agent reports what it could not do.

Each file's last `text` is the report the agent would give on the page,
where the load went as the file means: under `--once` it reads against
the `✗` lines above it.
