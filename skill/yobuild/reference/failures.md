# When yobuild fails: what it says and what to do

Find the message, then do what its row says. yobuild says the fix too:
after a run that did not complete, `run` gives `What to do: …` (a Notice,
and the last line on stderr), and `check` gives each problem a `fix`.
A row with a hint comment (in the source of this file) is one yobuild
itself knows (`src/hints.rs`); a test keeps the two in step.

There are three kinds of failure:

- **Refused before anything starts**: `yobuild: …` on stderr, exit 1, no
  agent started. `yobuild check` gives the same error and prints no
  report: read stderr when stdout is empty.
- **Refused at start**: a plugin, a server or a built-in could not start.
  `check`: the agent is `refused`, with the problem.
- **During a run**: a run ended other than `Completed`, a tool call was
  refused, or a Notice said something stopped.

How a run ended is `Completed`, `Stopped: …`, `Error: …`, `Rejected: …`
or `Aborted: …` — under `--once` the last line on stdout, else
`yobuild: the last run ended …` on stderr. A stop by a limit, a budget or
the loop check reads `[Agent stopped: <reason>]`; a built-in that fails
the run reads `[Extension failed: <name>] <reason>`. The tables quote the
reason.

## During a run: how a run ended

| What it says | Ends | What it means | What to do |
|---|---|---|---|
| `Max turns reached (50/50)` <!-- hint: max-turns --> | `Stopped` | the run hit its turn limit — 50 when `[limits]` sets none | send "continue": a new run, with fresh limits (not under `--once`, which ends after one run); or raise `[limits] max_turns` |
| `Max tokens reached (…)` <!-- hint: max-tokens --> | `Stopped` | the token limit (1M by default) | as turns; `max_tokens` |
| `Max duration reached (…)` <!-- hint: max-duration --> | `Stopped` | the time limit (10 min by default) | as turns; `max_minutes` |
| `budget of $1.00 spent ($1.0012)` <!-- hint: budget --> | `Stopped` | the run's `budget_usd` | send "continue" (each run has its own budget), or raise `budget_usd` |
| `session budget of $2.00 spent (…)` <!-- hint: session-budget --> | `Stopped` | all the session's runs together | "continue" stops again at once: restart (a new conversation — the history lives in the process), or raise `session_budget_usd` |
| `repeated tool call — X was called repeatedly with identical arguments after being asked to change approach.` <!-- hint: loop --> | `Stopped` | the agent called one tool the same way over and over | change the prompt, or what the tool returns; send "continue" |
| any other stop (a plugin's) | `Stopped` | a plugin stopped the run | send "continue"; its reason says why |
| `[Extension failed: report] the answer was still not accepted after 3 continues` <!-- hint: report --> | `Error` | the last answer never named every `[builtins] report` section (checked anywhere in it, any case; the prompt already asks for them) | a model that follows the prompt better; fewer, plainer sections (`report = ["Done", "Next"]`); or remove `report`. Scripted: the last reply must name them |
| `(no more mock responses)` <!-- hint: no-more-replies --> | `Error` | a `--scripted` run used up its replies (often: `report` sent it back) | add replies; with `report`, make the last one name every section (`scripted.md`) |
| the provider's error, `429` / `rate limit` <!-- hint: rate-limit --> | `Error` | the provider is rate-limiting | wait, then send again |
| the provider's error, `401` <!-- hint: unauthorized --> | `Error` | the provider refused the key | the key's variable (`yobuild run` names it when missing) |
| the provider's error, otherwise (the network, the model) | `Error` | the model could not be asked | the network; the model name; send again |
| `The agent's run failed unexpectedly (see the host's logs); later runs may be missing tools.` <!-- hint: run-failed --> | `Error` | the agent loop failed | the log; restart |
| `input rejected: the plugin host is not running (…)` <!-- hint: host-down --> | `Rejected` | the plugin host shut down | restart |
| `stopped with Ctrl+C` | `Aborted` | the person stopped it | nothing |
| `the terminal UI stopped: …` | exit 1 | the terminal UI's runtime died | the log file it names |

## During a run: a tool call refused

These come back to the agent as the tool's result (`Tool call denied: …`)
and show as `name ✗` under `--once`.

| What it says | What it means | What to do |
|---|---|---|
| `the owner blocked it` | the owner answered Block | nothing, if meant |
| `not allowed: the owner declined, or nobody answered` | no frontend, or the question timed out; under `--once` nobody is ever there | open the page (or the terminal) and answer. A plugin's or server's tool that only reads or plans: `allow` it on its entry. `bash`, `write_file`/`write`, `edit_file`/`edit` cannot be allowed (the gate judges each call); with `[builtins] scratch = true` (off by default), what the agent writes in the scratch folder is free |
| `no plugin policy is loaded to judge this call` <!-- hint: no-policy --> | `policy = true`, and the plugin that judges calls went away mid-session (at start, `run` waited for it) | restart; the Error Notice `A plugin stopped: …` and the log say why |
| `plugin handler \`X\` is no longer available: its plugin unloaded` <!-- hint: handler-gone --> | its runtime died or it was unloaded | restart; the log says why |
| `plugin handler \`X\` did not answer \`before_tool\` within …` <!-- hint: policy-timeout --> | the policy took longer than its wait (the dialogs' plus 30 s) | answer its question sooner; the plugin may hang |
| a policy's own reason (`confirm-dangerous.ts`'s Block) | the policy refused the call | as the policy says |
| `a required plugin handler failed this run: …` <!-- hint: handler-failed --> | a plugin's hook failed | the log; `yobuild check` |
| `the plugin host is not running (…)` | the plugin host shut down | restart |

## During a run: Notices

| What it says | What it means | What to do |
|---|---|---|
| Warning `plugin A allows \`x\`, but it was offered by \`plugin:B\`, not by it: the tool asks` | a plugin's allowed tool is freed only when its own row offers it | allow it on the entry that offers it (`yobuild check` says which before a run) |
| Warning `plugin A allows \`x\`, which no plugin offered` | a typo, or offered only in some runs | fix the name |
| Error `A plugin stopped: …`, `the \`X\` runtime is not running (its plugins wait: …)` | a plugin or its runtime died | the log; restart |
| Warning `Stopped early: <reason>.` | facts: the run stopped before its report | `What to do: …` follows it |
| an open question (with `facts`: `; 1 question(s) waiting on you`) | the owner has not answered | answer in the page or the terminal: during a run it steers that run; when idle it starts the next |

Not failures, though they read like one: facts' `not a git repository`,
`changes unknown: no snapshot at the start (git: …)` and `changes since
the start unknown (git: …)` (Info: `git init` the folder for facts to tell
what changed); `[builtins] ask_owner is off` (the error a plugin's own
`ask_owner` call gets: the person sees it only if the plugin shows it;
`ask_owner = true` lets it ask).

## Refused at start (and by `yobuild check`)

| What it says | What to do |
|---|---|
| `DSH plugins wait for services nobody provides: …` <!-- hint: dsh-services --> | `[dsh] host_services = true` for DSH's own services, or add the DSH plugin the hint names as a `[[plugin]]` |
| `… (waiting): nothing provides X` <!-- hint: nothing-provides --> | add the plugin that provides X, or remove the inject |
| a policy plugin's judge never registered (does it inject `yoagent`?) | inject `yoagent` in the plugin, or take `policy = true` off |
| an MCP server with no tools | the server, or its arguments |
| `its \`allow\` names x, which it does not offer (it offers: …)` <!-- hint: mcp-allow --> | use the server's own tool names in its `allow` |
| `cannot make the scratch folder …` | the temp folder is not writable: fix `TMPDIR`, or `scratch = false` |
| `its tool \`read_file\` is named like yoagent's built-in: \`run\` refuses it` <!-- hint: shadowed-builtin --> | choose another plugin: this one's tool cannot run beside yoagent's own |
| `registered its handlers alone, but none with the other plugins` <!-- hint: not-together --> | it conflicts with another plugin: check them in pairs |
| `a run would stop at its first request: …` <!-- hint: stops-at-first --> | a plugin stops every run before the model is asked: its reason says why |
| `it stopped: …`, `a plugin stopped: …` <!-- hint: plugin-stopped --> | the plugin failed while loading or after: its error says why; the log has more |

## Refused before anything starts

| What it says | What to do |
|---|---|
| `agent.toml:3:9: …` | the manifest at that line and column: the message says what it takes |
| `deepseek/deepseek-flash needs its key in DEEPSEEK_API_KEY` | set the variable (never in the manifest) |
| `budget_usd set, but acme/m1 has no price` | another model, or no budget |
| `prompt prompts/x.md: …` | the prompt file (missing; `{scratch}` without `[builtins] scratch`) |
| `agent.lock: its \`pi\` set is missing / is not current … (--locked: run without it to resolve the set again)` | run without `--locked`, then commit the new lock |
| `npm 11.5.1 wrote an incomplete lock …` | upgrade npm (11.19 or later), then run again |
| `x@9.9.9 is not on the registry` | the version: `npm view x versions` |
| `plugin X: … is pinned by the DSH base at …` | pin the base's version |
| `DSH files and DSH packages cannot run together yet` | one or the other, for now |
| `plugin X: … does not exist` | the path, relative to the manifest |
