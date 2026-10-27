# Self-extending reviewer

It reviews a JavaScript or TypeScript project and reports what to fix,
most important first. It starts with no linter. When the project's
config names one, it searches for that linter's MCP server, asks you,
and loads it for the session (`[extend]`, yobuild's M5). It never
changes the project.

Built with the yobuild skill (`yobuild skill install`) by a coding
agent, from this request: *"I want a code reviewer for my JavaScript and
TypeScript projects. I don't want to set up a linter for it in advance —
different projects use different tools. When it needs one, it should find
one itself, and ask me before it loads anything. It must never change my
code. Give me a report of what to fix, most important first. Show me what
it does when it loads a linter, when it is pointed at a package it was
not offered, and when I say no."*

## What it is made of

- **Finding a linter:** `[extend]`. The agent gets two tools:
  - `find_plugin` searches the MCP registry and npm, live, for MCP
    servers. It installs and runs nothing.
  - `load_plugin` loads one of the candidates for this session only. For
    an `eslint.config.js` that is ESLint's own server, `@eslint/mcp`,
    loaded as `eslint` with the tool `eslint__lint-files`.
- **The guards:** at most 2 servers and 4 load attempts a session. Only
  versions published at least 7 days ago are taken, and that holds for
  every package the server brings with it.
- **Reading:** `tools = "read-only"`: read, list and search only, inside
  the project. It has no tool that writes, and no `bash`.
- **The report:** Fix first, Fix later, Tools loaded, Not checked.
- **Limits:**
  - per run: $0.25 and 40 model turns;
  - in all: $1.00 until yobuild stops.

## What you are asked, and what it means

Loading a server puts three kinds of question to you, each Allow or
Block:

1. **"Start the MCP server @eslint/mcp@… as eslint?"**, before any of
   its code runs. The question's details show:
   - what the package says of itself (unverified);
   - the install scripts it has, which were not run;
   - what its code reads from the environment.

   Allow, and yobuild starts it. This is not a sandbox: the server runs
   with your rights, and ESLint's server runs your project's
   `eslint.config.*`, which is code.
2. **"Keep the MCP server eslint? It offers: lint-files"**, with each
   tool's description. Block, and it is stopped.
3. **"Run `eslint__lint-files`, …?"**, for each call it makes.

Block at the first or second question, and the package is not proposed
again that session. The agent then reviews by reading and says that
ESLint did not run. Nothing is written to `agent.toml`. When yobuild
stops, it prints the `[[mcp]]` block that would keep the server, for you
to add if you want it every time.

## What it does not do

- **It does not run the TypeScript compiler, the tests or any command.**
  Type errors come from reading, marked so. Its report lists
  `npx tsc --noEmit` under Not checked.
- **It lints `.ts` files only if your ESLint config covers them.** That
  needs typescript-eslint installed in the project (`npm install`).
  Otherwise ESLint reports them as ignored, and the agent says so.
- **It loads only MCP servers.** A pi or DSH plugin, such as pi-lens's
  type checks, has to be in the manifest.

## What it needs

- `DEEPSEEK_API_KEY` in the environment (the model is
  `deepseek/deepseek-flash`).
- `yobuild` installed (see yobuild's README), with Node 22.19+ and npm
  11.19+ on PATH.
- Someone at the web page to answer the questions. Under `--once`
  nobody is there, and `load_plugin` is refused.
- The network: the searches run live, and a server you allow is
  installed from npm into yobuild's cache (`~/.yobuild`).

## Run it

Get a copy of its folder with `yobuild new my-reviewer --from
self-extending-reviewer` (the paths below then name that folder).

```sh
cd path/to/your-project
DEEPSEEK_API_KEY=… yobuild run path/to/gallery/self-extending-reviewer/agent.toml
```

Open the URL it prints and ask for a review.

Without a model or a key, the reply files show the three cases. Only
the model is scripted: the searches really search, and an allowed server
really runs. Run one without `--once` and answer on the page:

```sh
yobuild run path/to/gallery/self-extending-reviewer/agent.toml \
  --scripted path/to/gallery/self-extending-reviewer/scripts/demo.json \
  "$(cat path/to/gallery/self-extending-reviewer/scripts/demo.task)"
```

- `scripts/demo.json`: Allow, Allow, then Allow for the lint call. ESLint
  finds `no-eval`, `eqeqeq` and `no-unused-vars` in `src/util.js`.
- `scripts/blocked.json`: Block at the first question. Nothing is
  installed or run.
- `scripts/not-offered.json`: you ask for a package `find_plugin` did
  not return. It is refused before anything installs, and no question
  is put to you.

yobuild's tests run all three this way, answering over the page's
WebSocket, in a project like the one they expect: an `eslint.config.js`
for `.js` files, `src/util.js` and `src/math.ts`. Their searches use the
live MCP registry and npm. A registry that is down fails the test, and
the candidates change with the sources: the reply files expect only
`@eslint/mcp` among them, and take whatever version is offered.

Under `--once`, every `load_plugin` call is refused alike ("not under
--once"), so all three look the same there.

## A live run

Not made yet.
