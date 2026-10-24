# Changelog

What each yobuild release brings. Each release says whether **the DSH
base changed**, because a lock with DSH plugins records it: when it
changed, `--locked` refuses such a lock, and one run without `--locked`
locks it again (commit the new lock).

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
