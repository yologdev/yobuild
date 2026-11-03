# Changelog

What each yobuild release brings. Each release says whether **the DSH
base changed**, because a lock with DSH plugins records it: when it
changed, `--locked` refuses such a lock, and one run without `--locked`
locks it again (commit the new lock).

## Unreleased

- **A sign-in for remote MCP servers** (OAuth in your browser; design
  §14 "Stage 4"):
  - **A server the agent loads** (`[extend] kinds = ["remote"]`) that
    answers 401 with a sign-in is signed in: you are asked whether to
    sign in, then shown a link to open in your browser (printed where
    yobuild runs in phone mode). Its sign-in must be on the server's own
    site. The token is kept for the session only, never shown to the
    agent, and revoked at the end. The block printed at the end carries
    `auth = "oauth"`.
  - **In `agent.toml`:** an `[[mcp]]` with `url` and `auth = "oauth"`
    is signed in each session once the page or the terminal UI is open,
    its sign-in on any site, and the task waits for it. Not under
    `--once`; `check` notes it. Its URL may hold no `${…}`, and no
    `Authorization` header goes beside it.
  - **Hints** for what you can act on: no PKCE, no registration, a
    sign-in on another site, cancelled, timed out, ended.
- **Sessions survive a restart** (design §15). Every session is saved
  under yobuild's home (`sessions/`, one per agent and folder, readable
  by you only; nothing in the project) after each run, and `yobuild run
  agent.toml --resume` continues it: the conversation, the open
  `ask_owner` questions (asked again, with their numbers) and what the
  session spent (`session_budget_usd` still holds). A run without
  `--resume` starts a new conversation, which replaces the saved one
  once its first run ends. `[session] save = false` keeps an agent's
  conversations off the disk. A scripted reply may say what the
  conversation must hold (`sees`). The DSH base did not change.
- **yoagent 0.26.6** (by way of 0.26.4 and 0.26.5). Three more models
  with their real limits and prices: `openai/gpt-6.1-terra`,
  `zai/glm-5.3-flashx` and `qwen/qwen3.8-omni-flash`; Meta's
  `muse-spark-1.3` and `muse-wonder-1.0` are now priced, so a manifest
  may name them. An MCP server over HTTP that echoes a credential
  header's value of 8 characters or more (`Authorization`, a cookie, a
  name holding `key`, `token`, `secret`, `auth` or `session`) back in an
  error body or a JSON-RPC error message has it shown as `[redacted]`.
- **rutis 0.20.0 (yoagent-rutis 0.12).** yobuild's
  plugin runtimes move from rutis 0.12 to 0.20.0 (by way of 0.19.3),
  which talks only to its neighbouring releases, so every runtime moves
  at once:
  - **A rutis plugin of your own** must pin `@arcships/rutis` and
    `@arcships/rutis-runtime` at exactly 0.20.0 in its package (`check`
    and `run` say so for the runtime, with the command). `definePlugin`
    is unchanged, but a plugin's `ctx.use` and `ctx.get` now see only the
    services it or its row declares: list what it uses in `inject`, and
    what it can do without in `optional` (read with `ctx.get`).
  - **A plugin's cleanups** run one at a time, in reverse order: one
    that waits for another's would wait for ever.
  - **An error a plugin leaves uncaught** no longer ends its runtime:
    rutis reports it (in the log) and the plugins there go on. A plugin
    calling `process.exit` still ends it.
  - **A plugin waiting for something that cannot come** (a service
    nobody provides while nothing could still provide it, a provider that
    failed, a cycle) is refused half a second after it is seen, said as
    rutis says it (`waiting for X (no row provides it)`, with `did you
    mean Y?` for a misspelt name).
  - **rutis's own errors** go to the log, not over the terminal UI; a
    plugin's event listener or filter that throws is also a Warning
    notice.
  - **A plugin the agent loads** is refused if it would use a service a
    plugin of the agent's own declares it provides.
  - **The DSH base changed** (the DSH adapter's rutis pins): `--locked`
    refuses a lock with DSH plugins made before; one run without it
    locks it again.

## 0.2.1 — 2026-11-01

- **yoagent 0.26.3.** Gemini and Vertex now stream non-ASCII text
  whole: a character split across two HTTP chunks (Chinese text, emoji;
  in answers, thinking and tool-call arguments) came back as U+FFFD.
  Other providers were never affected. yoagent-rutis is unchanged
  (0.10.0), and so is the DSH base.

## 0.2.0 — 2026-10-30

Agents that extend themselves: an agent can find a tool it lacks and
load it for the session, with the person's yes each time.

- **`[extend]` in the manifest** gives the agent two tools:
  - `find_plugin(need, kind?)` searches live, with no list built in:
    - MCP servers over stdio: the MCP registry and npm;
    - pi and DSH packages: npm, by their marks;
    - remote MCP servers: the MCP registry.

    It returns up to 10 candidates with what they claim of themselves,
    unverified, and runs nothing.
  - `load_plugin(package, name?)` loads one of them for the session:
    - It installs without running install scripts, taking only versions
      `min_age_days` old (7 by default).
    - The person is asked before anything of it runs, then again with its
      tools in front of them. Each call of its tools asks too.
    - At the session's end yobuild prints the `[[mcp]]` or `[[plugin]]`
      block that would keep it; nothing is written to the manifest.
  - **Limits:** `max_loads` and `max_attempts` per session, and `kinds`
    says what may be found (`"mcp"`, `"pi"`, `"dsh"`, `"remote"`).
  - Refused under `--once` (nobody to ask) and with `gate = "off"`.
- **What a loaded plugin gets:**
  - **A pi or DSH package:** it runs in a runtime of its own with its
    tools only. It gets no hooks, grants, `config` or host services.
  - **A remote MCP server:** it is reached over https only, never at an
    IP address or a local name.
    - The person names the environment variable that holds a
      credential; its value is never shown, logged or given to the
      agent.
    - A server that needs a sign-in (OAuth) is refused: yobuild cannot
      sign in yet.
  - **Stopping:** a loaded server or package that stops is said, and its
    tools go.
- **MCP servers:**
  - **Stdio:** each one runs in a process group of its own, so Ctrl+C is
    yobuild's, and closing a server stops what it started.
  - **Ctrl+C, SIGHUP and SIGTERM** are heard from the start of `run` and
    `check`, so a stop while starting stops what was started.
  - **An HTTP server's URL** is checked again once its `${…}` values are
    filled in. What its errors say shows each filled value as its
    `${NAME}`, in tool calls too.
- **Four new gallery agents** (`yobuild new --from <name>`):
  - `self-extending-reviewer` finds and loads ESLint's MCP server;
  - `project-notebook` loads the `pi-memory` pi package and remembers
    your decisions across sessions;
  - `docs-helper` connects to Context7, a remote MCP server, and answers
    from a library's docs at your project's version;
  - `release-notes` drafts release notes from git history. It is the
    first with a tool of its own, a small rutis plugin that the coding
    agent wrote. `yobuild new` says to run `npm ci --ignore-scripts` in its
    `plugins/`.
- **A tool of your own:** the skill now covers writing a rutis plugin:
  - its package, with both `@arcships/rutis` and its runtime;
  - the tool contract;
  - host services through grants;
  - arguments from the model, checked before use.

  A missing runtime is now told `npm ci --ignore-scripts` when the
  package's lock pins it, and otherwise what the lock pins.
- **The skill** covers `[extend]`: which kinds to allow, how to script
  the search, and why the load is tried on the page.
- **Tried on Claude:** web-research, code-reviewer and docs-helper ran
  live on `anthropic/claude-haiku-5-5`, unchanged but for `model` (their
  READMEs, "On Claude"). Until now only DeepSeek had been run live.
- **yoagent 0.26.2 and yoagent-rutis 0.10.0.** yoagent's compaction now
  keeps the task prompt and the newest turn when a context budget is
  small.
- **The DSH base:** unchanged since 0.1.0, so 0.1.0's locks still hold
  under `--locked`.

## 0.1.0 — 2026-10-25

The first release: build an agent from its parts and run it, without the
repository.

- **An agent is a manifest** (`agent.toml`), read strictly:
  - a model (any provider yoagent knows) and a prompt;
  - limits and budgets, per run and per session;
  - plugins, taken unchanged from other ecosystems: pi extensions,
    DeepSeek Harness (DSH) plugins and rutis plugins;
  - MCP servers, over stdio or HTTP;
  - yobuild's own built-ins and its own plugins (`yobuild:todo-plan`,
    `yobuild:search-links`, `yobuild:confirm-dangerous`);
  - a frontend: the web page, a phone, or the terminal.
- **`yobuild run`:**
  - the agent works in the folder it starts in;
  - `--once` runs one task, without a frontend, and says each tool call;
  - `--scripted` replaces the model with a reply file, so no key is
    needed, and a reply file's `expect` checks what a tool returned.
- **`yobuild check`:** vets every plugin and server without a model or a
  key, first each alone and then together. It says what each adds, how the
  gate treats each tool, and what `run` would refuse, with a fix for each
  problem (`--json` for programs).
- **Pinned plugins are installed** into `~/.yobuild` from the lock beside
  the manifest (`agent.lock`). Their packages' install scripts are never
  run, and `--locked` is for CI.
- **The built-ins:**
  - `ask_owner`: a question that doesn't block the run;
  - a required `report`;
  - `facts` the host measures;
  - an allow-list approval gate;
  - a scratch folder;
  - `tools = "read-only"`.
- **Grants** say what each plugin's host gives it: the folder, project
  trust, the model, waking the agent, running commands judged by the gate,
  and named environment variables. Everything is off unless granted.
- **The kept environment:** plugins and servers start without the
  person's credentials and other variables, unless an entry passes one
  on. This is not a sandbox: they still have the person's files.
- **`yobuild new <folder> [--from <agent>]`:**
  - a starter to fill in;
  - or a copy of one of the gallery's five agents (web research, a notes
    keeper, a code reviewer, a spreadsheet clerk, a code-health checker),
    each built through the skill and checked in CI;
  - or one of the three examples (handoff, coding, minimal).
- **`yobuild skill install`:** the skill a coding agent follows to build
  an agent for someone. It goes to Claude Code's skills folder by
  default, or `--dir` for another agent.
- **yobuild's own runtime files** (the pi and DSH adapters, the terminal
  UI) are built into the binary and installed into `~/.yobuild/runtime/`
  on first need. This needs Node 22.19+ and npm 11.19+.
- **Platforms:** macOS and Linux. Windows is not supported yet:
  `yobuild run` and `check` say so.
- **The DSH base:** the first one (yoagent-rutis 0.8.2's DSH adapter).
