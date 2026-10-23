# Code health

It checks a TypeScript project's health: the type errors a real
TypeScript language server finds, and an outline of what is where. It
never changes the project's files.

Built with the [yobuild skill](../../skill/yobuild/SKILL.md) by a coding
agent, from this request: *"I'd like a code-health checker for a
TypeScript project: it should find type errors and give me an outline of
what is where. Read-only — it must never change my files."*

## What it is made of

- **Type errors and the outline:** the pi package
  [`pi-lens`](https://www.npmjs.com/package/pi-lens) 4.4.3, granted
  `trust` and `model = "info"`. Seven of its tools only read, and they run
  without asking:
  - `lens_diagnostics`, a TypeScript language server probing the files;
  - `project_report`, `module_report`, `symbol_search`, `read_symbol`,
    `read_enclosing` and `effective_config`.

  `pi_lens_activate_tools` changes which tools a run is offered, so it
  asks.
- **Reading:** `tools = "read-only"`, so read, list and search only,
  inside the project. It has no tool that writes, and no `bash`.
- **Questions to you:** `ask_owner`, when the task does not match the
  project (a package that is not there, say).
- **The report:** Type errors, Project outline, Not checked.
- **Limits:**
  - per run: $0.50 and 40 model turns;
  - in all: $2.00 until yobuild stops.

## What `trust` lets pi-lens do

You agree to this by running it:
- **Downloads.** pi-lens fetches into `~/.pi-lens`:
  - `typescript` and `typescript-language-server`, from npm, at unpinned
    `^` ranges;
  - `opengrep` and `typos-lsp`, from GitHub.

  That is about 157 MB after a first run.
- **What runs on your project.** Those servers run on your project, and
  your project's own code is among what they load.
- **Your files.** pi-lens writes nothing in the project. Its state goes
  to `~/.pi-lens`. But it runs `git status` and the like there, which may
  refresh `.git/index`.
- **Memory.** The pi runtime takes about 800 MB to 900 MB of memory.

## What it does not do

- **It does not run your build, tests or linter**, nor `npx tsc --noEmit`.
  The report lists those under Not checked. Type errors come from the
  language server, file by file, for the files it probes.
- **Not every probe finds something.** In a project new to pi-lens, the
  first `lens_diagnostics` may find nothing, and a server still starting
  (its first download, a slow machine) answers "inconclusive". The prompt
  handles this:
  - it probes twice, giving each probe a minute (`waitMs: 60000`);
  - it reads pi-lens's `Outcomes:` line;
  - it never reports "clean" from an unconfirmed check.

## What it needs

- `DEEPSEEK_API_KEY` in the environment (the model is
  `deepseek/deepseek-flash`).
- A working `yobuild`, from a set-up checkout (Node 22.19+ and npm; see
  the skill).
- The network:
  - on the first run, yobuild installs pi-lens from npm into its cache
    (`~/.yobuild`);
  - on pi-lens's first probe, its servers download as above, which takes
    about 25 s;
  - later probes take about 17 s each, the server starting.

## Run it

From your project's folder (the agent reads the folder it is started
in):

```sh
cd path/to/your-project
DEEPSEEK_API_KEY=… path/to/yobuild run path/to/gallery/code-health/agent.toml
```

Open the URL it prints and ask for a health check. Its questions appear
above the message box. The gate may also ask Allow/Block for a tool the
agent does not have, or for `pi_lens_activate_tools`. Block it.

Try it without a model or a key, in a folder holding the sample its
reply files were written against:
- `tsconfig.json` with `strict` and `noEmit`, including `src`;
- `src/sum.ts`, whose `sum(1, "2")` passes text where a number goes.

Only the model is scripted; pi-lens really runs. Each reply file checks
what the tools found with `expect`, the type error among them:

```sh
yobuild run path/to/gallery/code-health/agent.toml \
  --scripted path/to/gallery/code-health/scripts/demo.json \
  --once "$(cat path/to/gallery/code-health/scripts/demo.task)"
```

`scripts/question.json` names a module that is not there, so the agent
asks which one is meant and checks `src/` meanwhile.

## A live run

2026-10-23, the second build's agent (the third, here, adds `waitMs` and
the `Outcomes:` rule to its probes), `deepseek/deepseek-flash`, `--once`,
in the sample (a fresh folder; pi-lens's servers already fetched), with
the task "Check this TypeScript project: type errors and an outline of
what is where."
- **Calls:** 12 — `list_files`, `read_file`, `search`, `lens_diagnostics`
  twice, `project_report` twice, `symbol_search` and `module_report`.
  - The first `project_report` and the `symbol_search` came back
    "retry shortly", as pi-lens does in a folder new to it.
  - The two probes agreed.
- **Your files:** no change.
- **Cost:** `$0.0034 (35913 tokens in, 1255 out; 7 turns)`.
- **What it reported:**
  - the type error at `src/sum.ts:4`, `typescript:2345`, `sum(1, "2")`,
    with the likely fix;
  - an outline: `sum` and `total` exported, `total` found by a `search`
    since `module_report` leaves constants out;
  - under Not checked: no `package.json`, so no build, tests,
    `npx tsc --noEmit` or lint.

`Completed`.
