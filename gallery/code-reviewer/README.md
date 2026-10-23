# Code reviewer

It reviews the code quality of a TypeScript project: lint problems, type
errors and risky patterns. Then it reports what to fix, most important
first. It never changes the project.

Built with the [yobuild skill](../../skill/yobuild/SKILL.md) by a coding
agent, from this request: *"I want an agent that reviews the code quality
of my TypeScript project — lint problems, type errors, risky patterns —
and gives me a report of what to fix, most important first. It must not
change my code."*

## What it is made of

- **Linting:** ESLint's own MCP server,
  [`@eslint/mcp`](https://www.npmjs.com/package/@eslint/mcp) 0.3.15, as
  `eslint`. Its one tool, `lint-files`, runs your project's
  `eslint.config.*` and only reports: it runs without asking.
- **Reading:** `tools = "read-only"`, so read, list and search only,
  inside the project. It has no tool that writes and no `bash`.
- **Questions to you:** `ask_owner`, for what is yours to decide (which
  folders to leave out, say). It never waits for an answer.
- **The report:** Fix first, Fix soon, Worth a look, Not checked, Waiting
  on you.
- **Facts:** after each run, what changed in the repository since the
  start. It should always be "no changes".
- **Limits:** $0.50 and 40 model turns per run; $2.00 in all until
  yobuild stops.

## What it does not do

- **It does not run the TypeScript compiler.** No part yobuild offers
  runs `tsc` without also letting the agent write files. It finds type
  errors by reading, marks them so, and every report asks you to run
  `npx tsc --noEmit` and `npx eslint .`.
- **It doesn't use pi-lens.** The skill's first choice for linting and
  types was the pi package `pi-lens`. Under yobuild it treats the project
  as untrusted, starts no language server, and reports every file clean.
  The coding agent found this out and switched to ESLint's server. That
  was before yobuild could grant `trust` (design §12, step 3): with
  `grants = { trust = true }` pi-lens finds type errors (design §11,
  "pi-lens in the gallery").

## What it needs

- `DEEPSEEK_API_KEY` in the environment (the model is
  `deepseek/deepseek-flash`).
- A working `yobuild`, from a set-up checkout (Node 22.19+ and npm; see
  the skill).
- Your project's ESLint config, and its dependencies installed
  (`npm install`). The server brings its own ESLint 10, but a config's
  plugins (typescript-eslint, for `.ts` files) come from your
  `node_modules`. Without typescript-eslint, ESLint skips `.ts` files: the
  agent says so and reviews them by reading. The config is code, and it
  runs.
- The network on the first run only: yobuild installs the server from npm
  into its cache (`~/.yobuild`).

## Run it

```sh
cd path/to/your-project
DEEPSEEK_API_KEY=… path/to/yobuild run path/to/gallery/code-reviewer/agent.toml
```

Open the URL it prints and ask for a review. Its questions appear above
the message box. The gate may also ask Allow/Block for a tool the agent
does not have (`write_file`, `bash`). Block it: either answer changes
nothing.

Try it without a model or a key, in a project like the one `scripts/`
expects (only the model is scripted; ESLint really runs):

```sh
yobuild run path/to/gallery/code-reviewer/agent.toml \
  --scripted path/to/gallery/code-reviewer/scripts/demo.json \
  --once "$(cat path/to/gallery/code-reviewer/scripts/demo.task)"
```

`scripts/no-writes.json` asks it to fix things: every write, edit, command
and read outside the project is refused.

## A live run

2026-10-19, `deepseek/deepseek-flash`, `--once`, in a tiny project (an
ESLint config for `.js` only; `src/util.js` with an unused variable, `==`
and `eval`; `src/math.ts` passing a string where a number is due), the
task "Review this project's code quality and tell me what to fix first.":
16 calls (`read_file`, `list_files`, `search`, `eslint__lint-files`), no
change to the project, `Cost: $0.0049 (24776 tokens in, 3224 out; 7
turns)`. It reported ESLint's three errors in `src/util.js`
(`no-unused-vars` 2:9, `eqeqeq` 3:13, `no-eval` 4:12), the type error in
`src/math.ts:5` (found by reading), that the config leaves the `.ts`
files unlinted, and more. `Completed`.

The first live run showed none of ESLint's findings. yoagent 0.25.7 sent
the model only the first of a tool result's text blocks, and ESLint's
server answers in three: an intro, the findings, a note. yoagent 0.25.9
sends every block (yologdev/yoagent#347).
