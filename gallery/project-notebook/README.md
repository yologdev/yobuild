# Project notebook

It answers your questions about the project you start it in, read-only,
and remembers the decisions and notes you tell it from one session to
the next. It has no memory set up in advance: when a task asks it to
remember something, or to recall what was decided, it searches npm for a pi memory package (pi-memory), asks you, and
loads it for the session (`[extend]`, yobuild's M5, stage 2). It never
changes the project.

Built with the yobuild skill (`yobuild skill install`) by a coding
agent, from this request: *"I want a notebook assistant for my projects.
It reads the project and answers my questions about it, and it remembers
the decisions and notes I tell it from one session to the next. I don't
want to set up a memory tool myself: it should find one and ask me
before it loads anything. It must never change my project's files. At
the end of each run, give me a short report of what it answered, what it
remembered, and what it couldn't do. Show me what it does when it loads
a memory tool, when it is pointed at a package it was not offered, and
when I say no."*

## What it is made of

- **Finding a memory:** `[extend]` with `kinds = ["pi"]`. The agent gets
  two tools:
  - `find_plugin` searches npm, live, for pi packages. It installs and
    runs nothing.
  - `load_plugin` loads one of the candidates for this session only:
    `pi-memory`, its tools as `pi-memory__memory_read`,
    `pi-memory__memory_write`, and so on.
- **How a loaded package runs:** in a runtime of its own, with its tools
  only. pi-memory's own hooks are not called, so it does not recall its
  memory into each turn: the agent reads it with `memory_read` before
  relying on it. It gets no grants, no config and none of yobuild's host
  services.
- **The guards:** one package and 3 counted `load_plugin` calls a
  session. Only versions published at least 7 days ago are taken, and
  that holds for every package it brings with it.
- **Reading:** `tools = "read-only"`: read, list and search only, inside
  the project. It has no tool that writes to the project, and no `bash`.
- **The report:** Answers given, Saved to memory, Out of reach.
- **Limits:**
  - per run: $0.25 and 30 model turns;
  - in all: $1.00 until yobuild stops.

## What you are asked, and what it means

Loading pi-memory puts three kinds of question to you, each Allow or
Block:

1. **"Start the pi package pi-memory@… as pi-memory?"**, before any of
   its code runs. The question's details show what the package says of
   itself (unverified), its install scripts (not run), and what its
   code reads from the environment. Allow, and yobuild starts it. This
   is not a sandbox: it runs with your rights.
2. **"Keep the pi package pi-memory? It offers: …"**, with each tool's
   description. Block, and it is stopped.
3. **"Run `pi-memory__memory_write`, a tool the agent loaded this
   session?"** (and `memory_read`), for each call it makes.

Block at the first or second question, and the package is not proposed
again that session: the agent answers from the project and lists what it
could not keep under Out of reach, with the note's text for you to keep. Nothing is written to `agent.toml`.
When yobuild stops, it prints the `[[plugin]]` block that would keep
pi-memory. Added to `agent.toml`, pi-memory runs with its hooks: it
recalls its memory into each turn. When the session ends it also tries
to write a summary to its daily log with the session's model, which
fails under yobuild: an extension is never given a key to call a model
with. `PI_MEMORY_EXIT_SUMMARY=0`, passed by an `env` grant, turns that
attempt off.

## Where it keeps what it remembers

pi-memory writes plain Markdown to `~/.pi/agent/memory/MEMORY.md` in your
home — outside the project, and the same file for every project (and for
pi itself, if you use it). So the agent starts each entry with the
project's folder name as a tag (`#tiny-shop #decision …`) and, reading
back, uses only its own project's entries. To try it without touching
your own memory, start yobuild with a throwaway `HOME`, and
`YOBUILD_HOME` set to your real cache (`YOBUILD_HOME="$HOME/.yobuild"
HOME="$(mktemp -d)" yobuild run …`), so yobuild does not install its
runtimes again under the throwaway home.

## What it does not do

- **It does not recall unasked.** It loads its memory only when a task
  asks it to remember or recall something, so a plain question about the
  project is answered from the project alone.
- **It does not search its memory.** `memory_search` needs `qmd`
  installed; the agent reads the whole of `MEMORY.md` instead.
- **It does not forget or overwrite.** It only appends, and only what you
  told it to keep.
- **It does not run anything** in the project: no build, no tests.

## What it needs

- `DEEPSEEK_API_KEY` in the environment (the model is
  `deepseek/deepseek-flash`).
- `yobuild` installed (see yobuild's README), with Node 22.19+ and npm
  11.19+ on PATH.
- Someone at the web page to answer the questions. Under `--once`
  nobody is there, and `load_plugin` is refused.
- The network: the search runs live, and the package you allow is
  installed from npm into yobuild's cache (`~/.yobuild`).

## Run it

Get a copy of its folder with `yobuild new my-notebook --from
project-notebook` (the paths below then name that folder).

```sh
cd path/to/your-project
DEEPSEEK_API_KEY=… yobuild run path/to/gallery/project-notebook/agent.toml
```

Open the URL it prints, ask about the project, and tell it what to
remember.

Without a model or a key, the reply files show the three cases. Only
the model is scripted: the search really searches, and an allowed
package really runs. Run one without `--once`, in a project like
`tiny-shop` below, and answer on the page:

```sh
yobuild run path/to/gallery/project-notebook/agent.toml \
  --scripted path/to/gallery/project-notebook/scripts/demo.json \
  "$(cat path/to/gallery/project-notebook/scripts/demo.task)"
```

- `scripts/demo.json`: Allow, Allow, then Allow for each memory call
  (read, write, read). The note is appended to `~/.pi/agent/memory/MEMORY.md`.
- `scripts/blocked.json`: Block at the first question. Nothing of it
  runs, and the note is listed under Out of reach.
- `scripts/not-offered.json`: you ask for a package `find_plugin` does
  not return (`brainjar-pi-notes`, which does not exist). It is refused before anything installs, and no question
  is put to you.

yobuild's tests run all three this way, answering over the page's
WebSocket, in a project like the one they expect (`tiny-shop`: a
`README.md`, `package.json` and `src/config.js`, `src/server.js`,
`src/store.js`), each with a home of its own. Their search uses the live
npm registry: npm's registry down fails the test, and the reply files
expect only `pi-memory` among the candidates, taking whatever version is
offered.

Under `--once`, every `load_plugin` call is refused alike ("not under
--once"), so all three look the same there.

## A live run

2026-10-28, `deepseek/deepseek-flash`, on the page, in the project the
tests use (`tiny-shop`). It ran with a throwaway `HOME` and
`YOBUILD_HOME` set to the real cache, and every question was answered
Allow by a script over the page's WebSocket. Two sessions shared that
`HOME`.

The first session's task was `scripts/demo.task`: which port, where
orders are kept, and a decision to remember. It went like this:

1. It read the project's six files and answered from them, naming the
   lines (`PORT`, 3000 by default; a `Map` in `src/store.js`).
2. `find_plugin("memory", kind "pi")` returned `pi-memory` among npm's
   candidates. It proposed it, and pi-memory was started and kept (0.4.2,
   the newest a week old). The second question listed its seven tools.
3. `memory_read` found the memory empty. `memory_write` appended the note
   (`- #tiny-shop #decision 2026-10-28 We keep orders in memory until we
   move to Postgres.`), and a second `memory_read` read it back.

That session cost `$0.0045` (39,960 tokens in, 1,194 out; 8 turns) and
ended `Completed`. Nothing in the project changed. The note was in the
throwaway home's `.pi/agent/memory/MEMORY.md`. After Ctrl+C yobuild
printed the `[[plugin]]` block that would keep pi-memory, and how it
would run there.

The second session, started fresh, was asked "What did we decide about
where tiny-shop keeps its orders, and does the code still match that
decision?". It loaded pi-memory again (asked twice again), read the note,
compared it with `src/store.js` and the README, and said the code still
matches. It wrote nothing, as it had been told nothing new. That session
cost $0.0029 (17,753 tokens in, 1,206 out; 4 turns), and also ended
`Completed`.
