# What you can build with yobuild

An **agent** is a program that uses an AI model to do a task for you with
tools: it searches, reads, writes, runs commands, and asks you when a
decision is yours. yobuild builds agents from parts other people already
publish, so you don't have to write one.

You don't need to be a programmer. Install yobuild, give Claude Code its
skill, and say what you want: "make me an agent that keeps my notes and
answers questions from them." Claude Code then:
- asks you a few questions;
- picks the parts;
- writes the agent's settings file;
- checks it;
- runs it with made-up model replies, so the model costs nothing (its
  parts still run for real: a search still searches);
- asks you before anything costs money.

The result is an agent of your own, on your computer. You can open it in
your browser, in your terminal, or on your phone on the same Wi-Fi.

- [Agents people have built](#agents-people-have-built)
- [What else you can build](#what-else-you-can-build)
- [How you get one](#how-you-get-one)
- [Why yobuild](#why-yobuild)
- [Compared with other ways](#compared-with-other-ways)
- [When yobuild is not the right tool](#when-yobuild-is-not-the-right-tool)

## Agents people have built

Each of these is built into yobuild. `yobuild new my-agent --from <name>`
makes a copy you can run.

The first five are the gallery. A coding agent built each one with the
yobuild skill, from a request a person would make. Each copy has a README
that quotes the request and says what the agent needs and does.

The costs are from real runs with DeepSeek's `deepseek-flash` model (see
each README). Another model costs what its provider charges.

| Agent | You ask it | It uses | A real run cost |
|---|---|---|---|
| `web-research` | "When does Node.js 20 reach end of life?" It searches, reads the pages, and answers with its sources and what it still needs from you. | a free web search (a DeepSeek Harness plugin) | $0.0027 |
| `notes-keeper` | "When do I plant the tomatoes?" It reads your folder of notes and answers. Any change to a note asks you first. | the filesystem MCP server | $0.0016 |
| `code-reviewer` | "Review this project and tell me what to fix first." It lints and reads, and changes nothing. | ESLint's MCP server | $0.0049 |
| `spreadsheet-clerk` | "Build the spreadsheet for expenses-2026-09.csv." It makes an `.xlsx` with totals by category, asking before each step that runs code or writes a file. | a spreadsheet plugin (DeepSeek Harness's `dsh-univer-office`) | $0.0109 |
| `code-health` | "Check this TypeScript project." It lists the type errors a real TypeScript server finds, and an outline of what is where. | the `pi-lens` pi package | $0.0034 |
| `self-extending-reviewer` | "Review this project and tell me what to fix, most important first." It starts with no linter: it finds the one the project's config names, asks you before loading it and before each call, and changes nothing. | `[extend]`: ESLint's MCP server, found and loaded while it runs | $0.0054 |
| `handoff` | Any task (research, writing, planning, code): you hand it over and walk away. It plans, asks only what is yours to decide without waiting for the answer, and ends with a report. | a to-do plan (a pi package) and the web search | depends on the task; at most $2 a session |
| `coding` | A coding agent in your terminal. A safety policy asks before commands known to be dangerous (`rm -rf` and the like); everything else runs. | a pi safety policy, web search, questions | depends on the task; no spending limit set |

`yobuild new --list` shows them all, and `yobuild new my-agent` alone
writes a starter to fill in.

## What else you can build

An agent is any mix of parts that load. There are four kinds:

- **MCP servers.** MCP is the common way to give an AI model tools, and
  there are servers for many services and file types. yobuild runs most
  of them:
  - local ones, from npm or a program on your computer;
  - remote ones, by address with fixed headers (not ones that need you
    to sign in through a browser).
- **pi packages:** extensions for pi, an open-source coding agent,
  published on npm. For example: sub-agents, a wiki of notes, web search
  with paid engines, code health.
- **DeepSeek Harness (DSH) plugins:** plugins for DeepSeek's agent app,
  published on npm. For example: web search, spreadsheets and documents,
  questions to you.
- **rutis plugins:** small TypeScript or JavaScript plugins written for the plugin
  runtime yobuild uses. yobuild's own page add-ons are rutis plugins.

Not every published plugin loads. Many need their own app around them,
and yobuild refuses those instead of half running them. The skill's
[list of parts](../skill/yobuild/reference/parts.md) names the ones that
were checked to load, and `yobuild check` tells you about any other
before you trust it.

Each of these is within reach, as a settings file and a prompt:
- a research assistant for a subject you follow, writing its findings
  into a notes folder;
- a project's helper that answers "where is X?" and "what broke?" and
  never edits anything;
- a clerk that turns the CSVs you download each month into a
  spreadsheet;
- a delegate you hand chores to from your phone, which asks you only
  what it cannot decide.

## How you get one

1. **Install yobuild.** It runs on macOS and Linux.
   - Download it for your machine from the
     [releases page](https://github.com/yologdev/yobuild/releases), or
     build it with Rust. Claude Code can do this for you.
   - Agents with plugins also need Node.js.
   - The [guide](guide.md#installing) has the details.
2. **Give Claude Code the skill:** `yobuild skill install`.
3. **Ask for an agent:** "make me an agent that …". Claude Code:
   - asks what it is for, what it may change, and how you want to use it;
   - picks parts that were checked to load;
   - writes `agent.toml` (the settings file, a page you can read) and
     the agent's instructions;
   - runs `yobuild check` and fixes what it finds;
   - runs it with made-up model replies (no key, no model cost);
   - asks you before the first live run.
4. **Use it.** `yobuild run agent.toml` either prints a link to the
   agent's page, or, for a terminal agent, opens in the terminal. Type a
   task, and answer its questions there.

You can also start from a copy (`yobuild new research --from web-research`)
and ask Claude Code to change it.

## Why yobuild

First, what it is not: **not a sandbox.** Its parts run with your rights
on your computer (more [below](#when-yobuild-is-not-the-right-tool)).
Within that:

- **No code to write.** An agent is a short settings file and a prompt.
  Claude Code writes them, you can read them, and changing the agent
  means changing a line, not a program.
- **Parts that already exist, used as they are.** pi packages, DSH
  plugins and MCP servers load unchanged from npm, so you start from what
  others built. Versions are pinned in a lock file, so the agent you
  checked is the agent that runs, until you change a pin.
- **Checked before you trust it.** `yobuild check` loads every part
  without a model, a key or any model cost. It says:
  - what each part adds;
  - which tools run freely and which ask you;
  - what would fail, with a fix for each problem.

  It refuses a part that would not load, instead of half running it.
- **It asks before it harms.** With the default approval gate:
  - **free:** reading, building and testing, writing new files, and
    editing files in the project;
  - **asks you first:** deleting or replacing your files, writing
    outside the project or to `.env` and `.git`, and any command it
    doesn't know.
    With nobody there to answer, it is refused.

  Each plugin tool asks unless the settings allow it by name. An agent
  can switch the gate off and rely on a plugin's own policy instead, as
  `coding` does.
- **It doesn't wait on you.** With `ask_owner` on, a question doesn't
  stop the work: the agent carries on with what doesn't depend on the
  answer. It picks the answer up whenever it comes, while yobuild is
  running, even hours later. An answer after the run ended starts a new
  run. The gate's own Allow/Block questions do wait.
- **It reports, and yobuild adds the facts.** A run can be required to
  end with a report in the sections you choose; yobuild sends the agent
  back until the sections are there (it checks the sections, not what
  they say). With `facts` on, yobuild adds numbers the model cannot make
  up:
  - tokens, tool calls, time and questions still waiting;
  - in a git repository, also the files changed and the commits.
- **Spending can be capped.**
  - Settings can cap dollars per run and per session, and turns, tokens
    and time per run.
  - Unless set, a run stops at 50 turns, 1M tokens or 10 minutes.
  - The five gallery agents each cap dollars per run and per session.
    Their runs above cost under two cents each.
- **Plugins don't get your keys from the environment.** Plugins and
  servers start without your environment variables, keys included,
  unless the settings pass one on by name. Your files, a `.env` or a
  `~/.aws/credentials`, can still be read.
- **Where you are.** Use it in a browser, in the terminal, or on your
  phone on the same Wi-Fi (when the agent's settings turn `phone` on).
- **Yours.** It runs on your computer, with the model provider you choose
  (DeepSeek, Anthropic, OpenAI, Google, a local model through Ollama, and
  others). There's no account and no hosted service. Your task and the
  files the agent reads go to that provider, and to whatever services its
  parts use.

## Compared with other ways

**Doing the task in a coding agent** (Claude Code, pi) works for one-off
jobs. Both can also run unattended. A yobuild agent is made for one job
you want done again and again, or hand to someone else:
- only the parts that job needs, at pinned versions;
- its own rules on what asks first, and its own spending limit;
- a page you open in a browser;
- a folder you can give away.

**Writing it with an agent framework or SDK** (LangChain and LangGraph,
CrewAI, the OpenAI Agents SDK, the Claude Agent SDK) gives a programmer
the most control. You write the agent in Python or TypeScript. These
frameworks connect to MCP servers too, and have ways to pause for a
person's approval.
- **Choose one** when you want to write code, run many agents together,
  or deploy to a server.
- **Choose yobuild** when you'd rather not write the agent at all, when
  you want pi packages and DSH plugins unchanged, and when you want the
  checks, the approval gate, the report and the page already there.

**Drawing it in a visual builder** (n8n, Dify, Zapier Agents, OpenAI's
Agent Builder): you connect steps on a canvas, with many web services
ready to plug in. Where they run differs:
- Zapier Agents and Agent Builder run on their company's servers;
- n8n and Dify can also run on a server of your own.

Choose one when the agent lives among web apps, runs for a team, or
follows a flow you want to draw step by step. Choose yobuild when the
agent works on your own folders and files on your computer, needs no
server, and is built from a conversation with Claude Code rather than
drawn.

## When yobuild is not the right tool

- **You need a sandbox.** yobuild is not one. Plugins run with your
  rights, and anything the agent can read it can also send. The gate
  guards against the agent deleting or replacing your files, not against
  a plugin you shouldn't have trusted. For a guarantee, run it in a
  container or on a copy of your files.
- **You use Windows.** Not supported yet.
- **You want a hosted service** for many users, with accounts. yobuild
  runs one person's agents on one computer.
- **The part you need doesn't load.** Many popular DSH plugins need DSH's
  own app around them. `yobuild check` says so, and the skill knows which
  parts load.
- **It is young.** The built-in agents' scripted runs are tested on every
  change, and the gallery's live runs were checked by hand; much else is
  new.
