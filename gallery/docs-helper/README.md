# Docs helper

Ask it how to do something with a library your project uses. It finds
the version your project depends on, looks the answer up in that
library's current documentation, and reports:
- the answer;
- where it came from;
- what it could not check.

It sets up no docs tool in advance. When it needs one, it searches the
MCP registry for a remote docs server (Context7, which needs no key),
asks you, and connects to it for the session (`[extend]` with
`kinds = ["remote"]`). It never changes your project.

## What it is made of

- **Finding docs:** `[extend]`, remote MCP servers only. Nothing is
  installed or run on your machine: each call sends its query to the
  server's host.
  - `find_plugin` searches the MCP registry.
  - `load_plugin` connects to one candidate for this session. It prefers
    `io.github.upstash/context7` (at `mcp.context7.com`), connected as
    `context7`. Its tools are `context7__resolve-library-id` and
    `context7__query-docs`.
- **The guards:** at most 1 server and 3 counted `load_plugin` calls a
  session.
- **Reading:** `tools = "read-only"`: read, list and search only, inside
  the project. It has no tool that writes, and no `bash`.
- **The version:** the installed one (`node_modules/<lib>/package.json`),
  else the lockfile's, else the range in `package.json`, said as such.
- **The report:** Short answer, Sources used, Left unverified.
- **Limits:** $0.25 and 30 model turns per run; $1.00 in all until
  yobuild stops.

## What you are asked, and what it means

Connecting to a docs server asks you four things on the page:
1. **"Connect to the remote MCP server io.github.upstash/context7 at
   mcp.context7.com as context7?"**, before anything is sent. Allow or
   Block.
2. **"Which environment variable holds the `Authorization` header for
   context7?"** Context7 needs no key: answer with nothing (an empty
   answer). Never paste a key itself.
3. **"Keep the remote MCP server context7? It offers: …"**, with each
   tool's description. Allow or Block.
4. **"Run `context7__…`?"**, for each call. Allow or Block.

Each query goes to Context7's servers. The prompt keeps your code, file
contents and secrets out of the queries; only the library's concepts are
sent.

Block at the first or third question, and the server is not proposed
again that session. The agent then answers from what it can read here,
marks anything from memory as unchecked, and says so in the report.
When yobuild stops, it prints the `[[mcp]]` block that would keep
Context7, for you to add to `agent.toml` if you want it every time.

## What it needs

- `DEEPSEEK_API_KEY` in the environment (the model is
  `deepseek/deepseek-flash`).
- `yobuild` installed. Nothing of the docs server runs here, so it
  needs no Node or npm.
- Someone at the web page to answer. Under `--once` nobody is there, and
  `load_plugin` is refused.
- The network: the MCP registry and Context7.

## Run it

Get a copy of its folder with `yobuild new my-docs --from docs-helper`
(the paths below then name that folder).

```sh
cd path/to/your-project
DEEPSEEK_API_KEY=… yobuild run path/to/gallery/docs-helper/agent.toml
```

Open the URL it prints and ask your question.

Without a model or a key, the reply files show the three cases. Only the
model is scripted: the registry search and Context7 are real. Run one
without `--once`, from the project, and answer on the page:

```sh
yobuild run path/to/gallery/docs-helper/agent.toml \
  --scripted path/to/gallery/docs-helper/scripts/demo.json \
  "$(cat path/to/gallery/docs-helper/scripts/demo.task)"
```

- `scripts/demo.json`: Allow; nothing at the variable question; Allow;
  then Allow for each of the two calls. Context7 resolves Express
  (`/expressjs/express`, versions including `v5.1.0`) and returns
  Express 5's docs on promise support in route handlers.
- `scripts/blocked.json`: Block at the first question. Nothing is sent
  to Context7; the answer is marked as from memory.
- `scripts/not-offered.json`: the task names a server,
  `io.github.acme/express-docs`, that the registry does not return. It
  is refused before anything connects, no question is put to you, and
  the agent connects to nothing else in its place.

Each reply file was written for the sample project: a `package.json`
with `"express": "^5.1.0"` and `src/app.js`. Its last answer is written
in advance, so it shows how a run ends, not what the docs said.

yobuild's tests run all three this way, answering over the page's
WebSocket, in the sample project (`package.json` and `src/app.js`).
They use the live MCP registry and Context7: either one down, or
Context7 no longer listing `v5.1.0` for Express or answering about its
promise support, fails the test. The reply files expect only
`io.github.upstash/context7` among the candidates.

Under `--once`, every `load_plugin` call is refused alike ("not under
--once"), so all three look the same there: they show the search.

## A live run

2026-10-30, `deepseek/deepseek-flash`, on the page, in the sample
project the tests use (`package.json` and `src/app.js`). The task was
`scripts/demo.task`, and every question was answered by a script over
the page's WebSocket: Allow, nothing at the variable question, Allow,
then Allow for each call. It went like this:

1. It read `package.json` and `src/app.js`, and found only the range
   `^5.1.0`: no lockfile, no `node_modules`.
2. `find_plugin("context7", kind "remote")` returned Context7 among the
   registry's candidates. It connected to `io.github.upstash/context7`
   with no key, and kept it.
3. `context7__resolve-library-id` listed Express's ids and versions.
   Two `context7__query-docs` calls on `/expressjs/express/v5.1.0`
   returned Express 5's router promise support and its notes on error
   handlers.

It answered that Express 5 passes a rejected promise from an async
handler to the error middleware by itself, and that the project lacks
that middleware. It gave a four-argument handler to add after the
route, and the lines it applies to. Under Left unverified it said which
parts the docs it got did not state (the four-argument rule, the
`err.status` field), marked from memory, and that the exact version is
unknown without a lockfile.

The run cost `$0.0046` (34,167 tokens in, 1,690 out; 8 turns) and ended
`Completed`. Nothing in the project changed. After Ctrl+C yobuild
printed the `[[mcp]]` block that would keep Context7 (`url =
"https://mcp.context7.com/mcp"`, no headers).
