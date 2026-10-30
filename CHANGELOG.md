# Changelog

What each yobuild release brings. Each release says whether **the DSH
base changed**, because a lock with DSH plugins records it: when it
changed, `--locked` refuses such a lock, and one run without `--locked`
locks it again (commit the new lock).

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
- **yoagent 0.26.1 and yoagent-rutis 0.10.0.**
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
