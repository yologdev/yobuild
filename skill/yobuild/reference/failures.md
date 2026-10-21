# When yobuild fails: what it says and what to do

Find the message, then do what its row says.

<!-- For maintainers: a row with a hint comment is one yobuild knows
(src/hints.rs); a test keeps the two in step both ways. -->

**Where yobuild says the fix itself.** For a failure it knows, yobuild
gives the fix as `What to do: …` (or `what to do:`, or `fix`):

- after a run that ended `Stopped`, `Error` or `Rejected`: an Info Notice
  to every frontend (under `--once`, printed among the notices as
  `  Info: What to do: …`); and, except under `--once`, a line
  `  what to do: …` under `yobuild: the last run ended …` on stderr;
- after a tool call refused for a reason it knows: an Info Notice, once a
  run (the run itself may still complete);
- after an Error Notice `A plugin stopped: …` it knows: an Info Notice;
- under `yobuild: …` when `run` is refused at start: `  what to do: …`;
- under `yobuild: …` when `yobuild check` stops with an error: the same;
- in `yobuild check`'s report: a `  fix: …` line under the problem, `fix`
  in `--json`.

The rows below say which failures those are and what the fix means; for
the others yobuild says nothing, and the row is the fix.

**Three kinds of failure:**

- **Refused before anything starts**: `yobuild: …` on stderr, exit 1, no
  agent started. `yobuild check` gives the same error and prints no
  report — read stderr when stdout is empty. `check` does not look at the
  key: a clean `check` does not mean the key is set. (A budget for an
  unpriced model is refused by the manifest itself, so `check` refuses it
  too.)
- **Refused at start**: a plugin, a server or a built-in could not start
  (`check`: the agent is `refused`, with the problem).
- **During a run**: a run ended other than `Completed`, a tool call was
  refused, or a Notice said something stopped.

How a run ended is `Completed`, `Stopped: …`, `Error: …`, `Rejected: …`
or `Aborted: …` — under `--once` the last line on stdout, else
`yobuild: the last run ended …` on stderr. A stop by a limit, a budget or
the loop check reads `[Agent stopped: <reason>]`; a built-in that fails
the run reads `[Extension failed: <name>] <reason>`.

## During a run: how it ended

| What it says | Ends | What it means | What to do |
|---|---|---|---|
| `[Agent stopped: Max turns reached (50/50)]` <!-- hint: max-turns --> | `Stopped` | the turn limit — 50 when `[limits]` sets none | send "continue": a new run, with fresh limits (not under `--once`, which ends after one run); or raise `[limits] max_turns` |
| `[Agent stopped: Max tokens reached (…)]` <!-- hint: max-tokens --> | `Stopped` | the token limit (1M by default) | as turns; `max_tokens` |
| `[Agent stopped: Max duration reached (…)]` <!-- hint: max-duration --> | `Stopped` | the time limit (10 min by default) | as turns; `max_minutes` |
| `[Agent stopped: budget of $1.00 spent ($1.0012)]` <!-- hint: budget --> | `Stopped` | the run's `budget_usd` | send "continue" (each run has its own budget), or raise `budget_usd` |
| `[Agent stopped: session budget of $2.00 spent (…)]` <!-- hint: session-budget --> | `Stopped` | all the session's runs together | "continue" stops again at once: restart (a new conversation — the history lives in the process), or raise `session_budget_usd` |
| `[Agent stopped: repeated tool call — X was called repeatedly with identical arguments after being asked to change approach.]` <!-- hint: loop --> | `Stopped` | the agent called one tool the same way over and over | change the prompt, or what the tool returns; send "continue" |
| `[Agent stopped: <anything else>]` | `Stopped` | a plugin stopped the run | its reason says why; send "continue" only once that cause is gone (yobuild says so) |
| `[Extension failed: report] the answer was still not accepted after 3 continues` <!-- hint: report --> | `Error` | the last answer never named every `[builtins] report` section (checked anywhere in it, any case; the prompt already asks for them) | a model that follows the prompt better; fewer, plainer sections (`report = ["Done", "Next"]`); or remove `report`. A scripted run: its last reply must name them |
| `Auth error: HTTP 401 …` (or 403) <!-- hint: auth --> | `Error` | the model's provider refused the key | the key's variable (`yobuild run` names it when missing), and that the key is valid for this model |
| `Rate limited` (`, retry after …`) <!-- hint: rate-limited --> | `Error` | the provider is rate-limiting, or out of capacity | wait, then send again |
| `API error: …`, `Network error: …`, `Context overflow: …` | `Error` | the model could not be asked | the network; the model name; a shorter task |
| `the run ended unexpectedly: the agent task failed` <!-- hint: task-failed --> | `Error` | the agent loop failed (an Error Notice `The agent's run failed unexpectedly …` comes with it) | the log; restart |
| `input rejected: the plugin host is not running (…)` <!-- hint: host-down --> | `Rejected` | the plugin host shut down | restart |
| `stopped with Ctrl+C` | `Aborted` | the person stopped it | nothing |
| `the terminal UI stopped: …` <!-- hint: terminal-ui --> | exit 1 | the terminal UI's runtime died | the log, named on the next line (`yobuild: log in …`) |

A scripted run (`--scripted`) that uses up its replies does not fail: the
model answers `(no more mock responses)` and, without `report`, the run
ends `Completed` with that answer. With `report`, each continue gets the
same answer, and the run fails as the report row says: add replies, and
make the last one name every section.

## During a run: a tool call refused

These come back to the agent as the tool's result (`Tool call denied: …`)
and show as `name ✗` under `--once`. The run goes on.

| What it says | What it means | What to do |
|---|---|---|
| `the owner blocked it` | the owner answered Block | nothing, if meant |
| `not allowed: the owner declined, or nobody answered` | no frontend, or the question timed out; under `--once` nobody is ever there | open the page (or the terminal) and answer. A plugin's or server's tool that only reads or plans: `allow` it on its entry. `bash`, `write_file`/`write`, `edit_file`/`edit` cannot be allowed (the gate judges each call); with `[builtins] scratch = true` (off by default), what the agent writes in the scratch folder is free |
| `no plugin policy is loaded to judge this call` <!-- hint: no-policy --> | `policy = true`, and the plugin that judges calls went away mid-session (at start, `run` waited for it): every call is denied | restart; the Error Notice `A plugin stopped: …` and the log say why |
| `plugin handler \`X\` is no longer available: its plugin unloaded` (or `… its plugin unloaded during …`) <!-- hint: handler-gone --> | its runtime died or it was unloaded | restart; the log says why |
| `plugin handler \`X\` did not answer \`before_tool\` within 330s` <!-- hint: policy-timeout --> | the policy took longer than its wait (the dialogs' 300 s plus 30 s) | answer its question sooner; the plugin may hang |
| `a required plugin handler failed this run: …` <!-- hint: handler-failed --> | a plugin's hook failed: its calls are denied this run | the log says which; `yobuild check` |
| `the plugin host is not running (…)` <!-- hint: host-down-call --> | the plugin host shut down | restart |
| a policy's own reason (`confirm-dangerous.ts`'s Block) | the policy refused the call | as the policy says |

## During a run: Notices

| What it says | What it means | What to do |
|---|---|---|
| Warning `plugin A allows \`x\`, but it was offered by \`plugin:B\`, not by it: the tool asks` | a plugin's allowed tool is freed only when its own row offers it | allow it on the entry that offers it (`yobuild check` says which before a run) |
| Warning `plugin A allows \`x\`, but it was offered from no plugin row (it must inject \`yoagent\`: …): the tool asks` | a child plugin registered it, its parent not injecting `yoagent` | inject `yoagent` in the parent |
| Warning `plugin A offered no tool \`x\`, which its \`allow\` names (a typo, or a tool it offers only in some runs)` | the name is wrong, or the tool comes only sometimes | fix the name (`yobuild check` lists what it offers) |
| Error `A plugin stopped: plugin \`…\` failed: …` (or `stopped itself`, `cannot load: …`) <!-- hint: plugin-failed --> | a plugin failed after it started | its error says why; the log has more |
| Error `A plugin stopped: the \`X\` runtime is not running (its plugins wait: …)` <!-- hint: runtime-down --> | a plugin runtime died | the log; restart |
| Warning `Stopped early: <reason>.` | facts: any `Stopped` run | `What to do: …` follows it |
| Warning `A DSH plugin's wake was refused: …` / `pi extension X's wake was refused: …` | a plugin's message to the agent was not delivered: its grant is missing, the owner's task has not started, or plugins already started their runs this session | grant `wake` if it should reach the agent; else nothing |
| Warning `pi extension X asked ctx.isProjectTrusted(): answered false (grant \`trust = true\` to change it)` (or `ctx.model`) | the extension was told the fixed answer, and may do less for it | grant what it names, if the person agrees (`trust` lets it run the project's code) |
| Warning `pi extension X ran pi.exec("cmd"): refused …` | the extension tried to run a command its grant does not allow, or the gate's question went unanswered | grant `exec = "gate"` (each command judged, the owner asked) if it should run commands |
| Warning `The DSH host: …` | the stand-in DSH agent's own trouble (its session log, a turn) | the message says which; the log has more |
| an open question (with `facts`: `; 1 question(s) waiting on you`) | the owner has not answered | answer in the page or the terminal: during a run it steers that run; when idle it starts the next |

Not failures, though they read like one: `check`'s note `probed without
the DSH host: its grants are not exercised alone` on a DSH plugin with
`grants` (alone it is probed without them; the scripted run exercises
them); facts' `not a git repository`,
`changes unknown: no snapshot at the start (git: …)` and `changes since
the start unknown (git: …)` (Info: `git init` the folder for facts to tell
what changed); `[builtins] ask_owner is off` (the error a plugin's own
`ask_owner` call gets: the person sees it only if the plugin shows it;
`ask_owner = true` lets it ask).

## Refused at start (and by `yobuild check`)

| What it says | What to do |
|---|---|
| `DSH plugins wait for services nobody provides: …` <!-- hint: dsh-services --> | `[dsh] host_services = true` for DSH's own services, or add the DSH plugin the hint names as a `[[plugin]]` |
| `the DSH host did not start: …` <!-- hint: dsh-host --> | the DSH host (loaded for DSH plugins' `grants`) waits for what it names, or provides no host (its log says why): reinstall the DSH runtime's folder the error names (a `dsh` set's: delete it; `adapters/dsh`: `npm ci` there) and run again; else drop the DSH plugins' `grants` |
| `… (waiting): nothing provides X` <!-- hint: nothing-provides --> | add the plugin that provides X, or remove the inject |
| `a call judge (before_tool) from the policy plugin's row plugin:X (does it inject \`yoagent\`?)` <!-- hint: no-judge --> | the policy plugin must inject `yoagent` and register a handler with `before_tool`; or take `policy = true` off |
| `yobuild cannot start /path/to/server: no such file` (or `it is not a file`, `it is not executable`, `it is not on PATH`, `it is on PATH but not executable`, `it is not on PATH where it could be looked at (…)`, or the system's error; a plugin runtime's prefixed with the runtime's name) <!-- hint: cannot-start --> | the program a stdio MCP server or a plugin runtime (Node) runs: fix the server's path or the permissions (a folder you cannot search), install its package again, or install Node |
| `cannot start node: No such file or directory (os error 2)` <!-- hint: no-node --> | install Node 22.19 or later: the plugin runtimes run on it |
| `cannot take X out of …'s environment: /usr/bin/env is not there, …` <!-- hint: no-env --> | install `env` at `/usr/bin/env` (and `/bin/sh`, needed for a program whose path holds `=`): yobuild starts a plugin runtime or stdio server through it while a credential yoagent reads is taken out of its environment |
| `mcp X: the server lists no tools` <!-- hint: mcp-no-tools --> | the server's arguments, or its version |
| `mcp X: its \`allow\` names "x", which it does not offer (it offers: …)` <!-- hint: mcp-allow --> | use the server's own tool names in its `allow` |
| `cannot make the scratch folder …` | the temp folder is not writable: fix `TMPDIR`, or `scratch = false` |
| `plugins failed to load: [Failure { id: "plugin:X", error: "…" }]` <!-- hint: load-failed --> | the plugin threw while it loaded: its error says why |
| `plugin rows not running: plugin:X (failed): plugin failed: …` <!-- hint: row-failed --> | the plugin threw while it loaded: its error says why |
| `check`: `its tool \`read_file\` is named like yoagent's built-in: \`run\` refuses it` <!-- hint: shadowed-builtin --> | choose another plugin: this one's tool cannot run beside yoagent's own |
| `check`: `plugin:X registered its handlers alone, but none with the other plugins` <!-- hint: not-together --> | it conflicts with another plugin: check them in pairs |
| `check`: `a run would stop at its first request: …` <!-- hint: stops-at-first --> | a plugin stops every run before the model is asked: its reason says why |
| `check`: `it stopped: plugin \`…\` failed: …`, `a plugin stopped: plugin \`…\` …` | as the Notice of that name above |

## Refused before anything starts

| What it says | What to do |
|---|---|
| `agent.toml:3:9: …` | the manifest at that line and column: the message says what it takes |
| `deepseek/deepseek-flash needs its key in DEEPSEEK_API_KEY` | set the variable (never in the manifest); `check` does not look |
| `budget_usd set, but acme/m1 has no price` (the manifest), `budget_usd: … has no price here` (`run`) | another model, or no budget |
| `prompt prompts/x.md: …` | the prompt file (missing; `{scratch}` without `[builtins] scratch`) |
| `agent.lock: its \`pi\` set is missing` / `is not current: …` / `does not agree with itself: …` `(--locked: run without it to resolve the set again)` | run without `--locked`, then commit the new lock |
| `agent.lock: its \`pi\` set is no longer needed (--locked: run without it to update the lock)` | as above |
| `npm 11.5.1 wrote an incomplete lock …` | upgrade npm (11.19 or later), then run again |
| `x@9.9.9 is not on the registry (npm view found no such version)` | the version: `npm view x versions` |
| `plugin X: … is pinned by the DSH base at …` | pin the base's version |
| `DSH files and DSH packages cannot run together yet` | one or the other, for now |
| `plugin X: … does not exist` | the path, relative to the manifest |
