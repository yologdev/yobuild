---
name: yobuild
description: Build an agent for someone with yobuild — from plugins other ecosystems already publish (pi extensions, DeepSeek Harness plugins, MCP servers) and yobuild's own built-ins — as one TOML manifest that is vetted, run scripted, then live. Use when asked to build, make or set up an agent ("an agent that …", "make me an assistant for …"), or to run pi or DSH plugins or MCP servers as an agent.
---

# Build an agent with yobuild

yobuild runs an agent from a manifest (`agent.toml`): a model, a prompt,
plugins from other ecosystems, a few built-ins of its own (questions to
the owner, a required report, facts, an approval gate), and a frontend
(a web page, a phone, the terminal). You write the manifest; yobuild
installs the pinned plugins, vets them, and runs the agent.

## A working `yobuild`

Until yobuild is installable on its own, it runs from a checkout of
<https://github.com/yologdev/yobuild>, set up once:

```sh
git clone https://github.com/yologdev/yobuild && cd yobuild
scripts/try-coding-agent.sh --setup-only   # Node 22.19+ and npm: the adapters
cargo build
```

Pinned DSH plugins need npm 11.19 or later to install (an older npm
writes an incomplete lock, which yobuild refuses). Call yobuild by path
from the agent's folder: `path/to/yobuild/target/debug/yobuild …` — below,
`yobuild` means that path.

## Where things go

One folder per agent, anywhere (not inside the yobuild checkout); the
agent works in the folder `yobuild run` is started in (usually this one,
or the project it works on):

```text
my-agent/
  agent.toml          the manifest
  agent.lock          written by the first `yobuild check` or `run` (when it pins packages); commit it
  prompts/agent.md    the prompt
  scripts/demo.json   the scripted replies
```

## The method

1. **Ask** what the agent is for: its task; who answers its questions and
   where (the web page, a phone, the terminal); what it must never do; how
   much it may spend; where it works (a folder, a git repository). Ask
   only what you cannot reasonably assume.
2. **Pick parts by capability**, fewest first (when no part does what the
   person asked — a type check without writes, say — tell them before
   building, and what the agent will do instead): the built-ins (`ask_owner`
   for questions without waiting, `report` for a required ending, `facts`
   for what changed, the gate, a scratch folder, `tools = "read-only"`);
   then a plugin from [reference/parts.md](reference/parts.md) at the
   version it lists (**CI** rows first: yobuild's own CI runs them); then
   an MCP server. A part not listed: pin `npm view <package> version`.
3. **Write** `agent.toml` and a first prompt (`check` reads it), then
   revise the prompt once `check` has listed the tools: keys, patterns and
   how to write the prompt in [reference/manifest.md](reference/manifest.md). `allow`
   only tools that read or plan (never `bash` or the writers: the gate
   judges those call by call); never a secret in a file (`${NAME}` from
   the environment); set a budget.
4. **Check**: `yobuild check agent.toml --json` (read the JSON from
   stdout, piped; do not save it into the agent's folder) — it installs and loads
   every plugin and server (it runs their code, and needs the network the
   first time), without a model.
   - Nothing on stdout: an error stopped it before anything started.
     Read stderr: `yobuild: …` names the file, the entry or the setting,
     and `  what to do: …` follows when yobuild knows the fix. (Plugins'
     own warnings — Node's `ExperimentalWarning`, say — reach stderr
     too; they are not errors: when stdout has the JSON, ignore them.)
   - Else read `ok`. Problems are at `entries[].problems` and
     `agent.problems`, each `{ "message", "fix" }` (`fix` when yobuild
     knows one): do the fix. Read each note (`entries[].notes`,
     `agent.notes`) and decide. Repeat until `ok` is true.
   - `agent.tools` lists every tool a run gets, with its `kind` (`free`,
     `judged`, `allowed`, `asks`, `runs`), its `description` and its
     `parameters` (the arguments' JSON Schema): use these names and
     arguments in the prompt and the scripted replies.
   - `installScripts` (per entry, and `agent.installScripts` for the DSH
     base): packages with install scripts, which yobuild did **not** run
     (it installs with scripts off). Nothing to do when `check` is `ok`:
     the plugins loaded without them (loaded: whether their tools work,
     the scripted run shows). Mention them to the person only if
     a plugin later fails for want of a native part.
   - `check` does not look at the model's key. A budget for a model with
     no price is refused already (the manifest is), so a clean `check`
     means the budget is accepted (a scripted run says `budget_usd not
     kept: a scripted model has no price` — the script has no price, not
     the model).
5. **Run scripted** — [reference/scripted.md](reference/scripted.md): a
   reply file that calls the tools the task needs, then
   `yobuild run agent.toml --scripted scripts/demo.json --once "<task>"`.
   Only the model is scripted: plugins and servers run for real (a search
   really searches). Expect each call `✓`; `✗ Tool call denied: …` where
   the gate would ask (under `--once` nobody answers); a `✗` with the
   tool's own error only for a third party's day (scripted.md); and a last
   line `Completed`.
   `--once` shows that a tool ran, not what it returned: to see the
   results, run the same reply file without `--once` and open the page
   (each call is a card there).
6. **Run live**, after asking the person (it costs money): the provider's
   key in the environment, then `yobuild run agent.toml "<task>"` (or
   without a task: the person types it in the page). Each run's cost is
   said when it ends — `  Cost: $…` on stderr with `--once`, after each
   run on the page: tell the person, and set `budget_usd` a few times
   above what a typical run cost. Tell the person:
   - **the web page**: open the URL yobuild prints (the task starts when
     it is open); the agent's questions — `ask_owner`'s, and the gate's
     Allow/Block — appear docked above the message box, the page usable
     meanwhile;
   - **a phone**: with `phone = true`, yobuild prints a link on the local
     network: open it on a phone on the same Wi-Fi;
   - **the terminal UI**: questions are answered in its editor.

   Keep `agent.toml`, `agent.lock`, the prompt and the reply files
   together (commit them, in a repository).
   Once the lock is committed, `--locked` (on `check` and `run`, as in CI)
   refuses a lock that no longer matches the manifest instead of
   resolving it again.
7. **When a run fails**: read how it ended — under `--once` the last
   line; else `yobuild: the last run ended …` on stderr — and the fix
   yobuild gives: `What to do: …` (a notice; under `--once` printed as
   `  Info: What to do: …`), `  what to do: …` under the last-run line,
   or `fix` in `check`. They are the same fixes. When it gives none, find
   the message in [reference/failures.md](reference/failures.md). Change
   the manifest or the prompt, and run again, scripted first.

## The rules

- `gate = "off"` only where something else judges calls: the agent runs
  in a sandbox, or a plugin with `policy = true` is the approval policy.
  It lets every call run as the agent asks.
- Never write a secret into a file: `${NAME}`, set in the environment.
- Never add plugin packages to yobuild's `adapters/`, and never edit a
  lock by hand: yobuild installs pinned plugins into its cache from the
  lock it writes.
- Never skip `check`, and never run live before a scripted run passed.
- Ask the person before a live run.

## References

- [reference/manifest.md](reference/manifest.md) — every manifest key, patterns, writing the prompt.
- [reference/parts.md](reference/parts.md) — what each built-in, plugin and server gives.
- [reference/scripted.md](reference/scripted.md) — the scripted reply file and its traps.
- [reference/failures.md](reference/failures.md) — every failure, what it means, what to do.
