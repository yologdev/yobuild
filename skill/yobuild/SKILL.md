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
scripts/try-coding-agent.sh --setup-only   # Node 22.19+, npm 11.19+: the adapters
cargo build
```

Then call it by path from the agent's own folder (the agent works in the
folder it is started in): `path/to/yobuild/target/debug/yobuild …`.
Below, `yobuild` means that path.

## The method

1. **Ask** what the agent is for: its task; who answers its questions and
   where (the web page, a phone, the terminal); what it must never do; how
   much it may spend; where it works (a folder, a git repository). Ask
   only what you cannot reasonably assume.
2. **Pick parts by capability**, fewest first: the built-ins (`ask_owner`
   for questions without waiting, `report` for a required ending, `facts`
   for what changed, the gate, a scratch folder, `tools = "read-only"`);
   then a plugin from [reference/parts.md](reference/parts.md); then an MCP
   server. Prefer what `parts.md` lists as loading.
3. **Write** `agent.toml` and its prompt file — keys and patterns in
   [reference/manifest.md](reference/manifest.md). Pin exact versions
   (`npm view <package> version`); `allow` only tools that read or plan
   (never `bash` or the writers: the gate judges those call by call);
   never a secret in a file (`${NAME}` from the environment); set a
   budget.
4. **Check**: `yobuild check agent.toml --json`.
   - Nothing on stdout: an error stopped it before anything started — read
     stderr (`yobuild: …`), which names the file, the entry or the setting,
     and often `  what to do: …`.
   - Else read `ok`. Each problem has a `message` and, when yobuild knows
     one, a `fix`; do it. Read each note and decide. Repeat until `ok` is
     true. `check` does not look at the model's key.
5. **Run scripted** — [reference/scripted.md](reference/scripted.md): a
   reply file that calls the tools the task needs, then
   `yobuild run agent.toml --scripted script.json --once "<task>"`. Expect
   each tool call `✓` (or `✗` where the gate should ask: under `--once`
   nobody answers) and a last line `Completed`.
6. **Run live**, after asking the person (it costs money): the provider's
   key in the environment, then `yobuild run agent.toml` (the web page
   prints its URL; the terminal UI opens). Tell the person how to answer
   the agent's questions. Commit `agent.toml`, its lock (`agent.lock`,
   written on the first run), the prompt and the reply file.
7. **When a run fails**: read how it ended — under `--once` the last line;
   else `yobuild: the last run ended …` — and the `What to do: …` yobuild
   gives. Find it in [reference/failures.md](reference/failures.md) when
   it gives none. Change the manifest or the prompt, and run again,
   scripted first.

## The rules

- Never `gate = "off"` unless the agent runs in a sandbox: it lets every
  tool call run as the agent asks.
- Never write a secret into a file: `${NAME}`, set in the environment.
- Never add plugin packages to yobuild's `adapters/`, and never edit a
  lock by hand: yobuild installs pinned plugins into its cache from the
  lock it writes.
- Never skip `check`, and never run live before a scripted run passed.
- Ask the person before a live run.

## References

- [reference/manifest.md](reference/manifest.md) — every manifest key, with patterns.
- [reference/parts.md](reference/parts.md) — what each built-in, plugin and server gives.
- [reference/scripted.md](reference/scripted.md) — the scripted reply file and its traps.
- [reference/failures.md](reference/failures.md) — every failure, what it means, what to do.
