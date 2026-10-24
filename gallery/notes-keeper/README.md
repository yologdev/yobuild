# Notes keeper

It answers questions about the markdown notes in a folder and tidies them
up: fixing headings and lists, adding an index, renaming or moving notes.
It works through an MCP server, the official filesystem one, and asks you
before it changes or creates any file.

It is also the pattern for plugging in any MCP server: see
[Your own server](#your-own-server).

Built with the yobuild skill (`yobuild skill install`) by a coding
agent, from this request: *"I keep my notes as markdown files in a folder.
I want an agent that answers questions about them and can tidy them up —
but through an MCP server, the official filesystem one, since that's how
I'll plug in my own servers later. It must ask me before it changes or
creates any file."*

## What it is made of

- **The server:** [`@modelcontextprotocol/server-filesystem`](https://www.npmjs.com/package/@modelcontextprotocol/server-filesystem)
  1.0.0 as `files`, given `"."`: the folder you start yobuild in, and
  nothing outside it. The agent sees its tools as `files__<tool>`.
  - Its ten reading tools run without asking (`allow`).
  - `write_file`, `edit_file`, `create_directory` and `move_file` ask you
    first, every time: Allow or Block, above the message box. Block leaves
    the file as it is.
  - It has no tool that deletes: the agent suggests a deletion instead.
- **yoagent's own tools:** `tools = "read-only"`, so read, list and search
  only, inside the same folder. The agent uses `search` to look inside
  notes, since the server's `search_files` matches file names. There is no
  `bash`: it runs no commands.
- **Limits:** $0.25 and 40 model turns per run; $1.00 in all until yobuild
  stops.

## What it needs

- `DEEPSEEK_API_KEY` in the environment (the model is
  `deepseek/deepseek-flash`).
- `yobuild` installed (see yobuild's README), with Node 22.19+ and npm
  11.19+ on PATH.
- The network on the first run only: yobuild installs the server from npm
  into its cache (`~/.yobuild`).

It keeps nothing outside your notes folder.

## Run it

Get a copy of its folder with `yobuild new my-notes-keeper --from notes-keeper` (the
paths below then name that folder).

```sh
cd ~/notes
DEEPSEEK_API_KEY=… yobuild run path/to/gallery/notes-keeper/agent.toml
```

Open the URL it prints and type a question or a tidy-up request. Each
change asks you on the page.

Try it without a model or a key, in a folder of notes (only the model is
scripted; the server is real):

```sh
yobuild run path/to/gallery/notes-keeper/agent.toml \
  --scripted path/to/gallery/notes-keeper/scripts/demo.json \
  --once "$(cat path/to/gallery/notes-keeper/scripts/demo.task)"
```

The demo reads `groceries.md` and `projects/garden.md`, then tries an
edit, a new file and a rename. Under `--once` nobody is there to answer,
so all three are refused and nothing changes. `scripts/question.json`
answers a question, and shows the server refusing a path outside the
folder.

## Your own server

In `agent.toml`, the `[[mcp]]` entry is all that changes:

- `npm = "<package>@<version>"` for a server on npm (`bin` too, when the
  package has several), or `url = "https://…"` for one over HTTP;
- `args` and `env` for a stdio server, `headers` for an HTTP one, with
  secrets as `${NAME}` from the environment, never written in the file;
- `name`: its tools become `<name>__<tool>`;
- `allow`: run `yobuild check agent.toml --json` and list the tools that
  only read. Leave the ones that change anything asking.

## A live run

2026-10-19, `deepseek/deepseek-flash`, `--once`, in a folder holding the
demo's two notes, the task "When do I plant the tomatoes, and what goes
next to them?": `files__directory_tree`, `search`, `files__read_text_file`,
`Cost: $0.0016 (10387 tokens in, 359 out; 3 turns)`, then:

> **Plant tomatoes after the last frost — mid May**, and **basil goes next
> to them**.
>
> That's all from `projects/garden.md` (the only note that mentions
> tomatoes). Nothing in your other notes (`groceries.md`) relates to this.

`Completed`.
