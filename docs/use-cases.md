# What you can build with yobuild

An **agent** is a program that uses an AI model to do a task for you with
tools: it searches, reads, writes, runs commands, and asks you when a
decision is yours. yobuild builds agents from parts other people already
publish, so you don't have to write one.

You don't need to be a programmer. Install yobuild, give Claude Code its
skill, and say what you want: "make me an agent that keeps my notes and
answers questions from them." Claude Code asks you a few questions,
picks the parts, writes the agent's settings, checks it, and runs it for
free with made-up model replies. Before anything costs money, it asks
you. The result is an agent of your own, on your computer, that you can
open in your browser or on your phone.

- [Agents people have built](#agents-people-have-built)
- [What else you can build](#what-else-you-can-build)
- [How you get one](#how-you-get-one)
- [Why yobuild](#why-yobuild)
- [Compared with other ways](#compared-with-other-ways)
- [When yobuild is not the right tool](#when-yobuild-is-not-the-right-tool)

## Agents people have built

Each of these is built into yobuild. `yobuild new my-agent --from <name>`
makes a copy you can run, and each copy has a README saying what it
needs. The five gallery agents were built the way you would build one:
a coding agent was given the yobuild skill and the request in quotes.
The costs are from real runs with DeepSeek's `deepseek-flash` model (see
each agent's README). Another model costs what its provider charges.

| Agent | You ask it | It uses | A real run cost |
|---|---|---|---|
| `web-research` | "When does Node.js 20 reach end of life?" — it searches, reads the pages, and answers with its sources and what it still needs from you | a free web search (a DSH plugin) | $0.0027 |
| `notes-keeper` | "When do I plant the tomatoes?" — it reads your folder of notes and answers; any change to a note asks you first | the filesystem MCP server | $0.0016 |
| `code-reviewer` | "Review this project and tell me what to fix first" — it lints and reads, and changes nothing | ESLint's MCP server | $0.0049 |
| `spreadsheet-clerk` | "Build the spreadsheet for expenses-2026-09.csv" — it made an `.xlsx` with totals by category, asking before each step that runs code or writes a file | a spreadsheet plugin (DSH's `dsh-univer-office`) | $0.0109 |
| `code-health` | "Check this TypeScript project" — it lists the type errors a real TypeScript server finds, and an outline of what is where | the `pi-lens` pi package | $0.0034 |
| `handoff` | any task (research, writing, planning, code): you hand it over and walk away; it plans, asks only what is yours to decide without waiting for the answer, and ends with a report | a to-do plan (a pi package) and the web search | depends on the task |
| `coding` | a coding agent in your terminal that asks before anything dangerous | a pi safety policy, web search, questions | depends on the task |

`yobuild new --list` shows them all, and `yobuild new my-agent` alone
writes a starter to fill in.

## What else you can build

An agent is any mix of parts that load. Three kinds are available:

- **MCP servers.** MCP is the common way to give an AI model tools, and
  there are servers for many services and file types. yobuild runs any
  of them, local or remote.
- **pi packages.** These are extensions for the pi coding agent: over
  11,000 on npm. For example: delegating to sub-agents, a wiki of notes,
  web search with paid engines, code health.
- **DeepSeek Harness (DSH) plugins.** Over 7,000 on npm. For example:
  web search, spreadsheets and documents, questions to you.

Not every published plugin loads. Some need their own app around them,
and yobuild refuses those instead of half-running them. The skill's
[list of parts](../skill/yobuild/reference/parts.md) names the ones that
were checked to load, and `yobuild check` tells you about any other
before you trust it.

So these are within reach, each a manifest and a prompt:
- a research assistant for a subject you follow, writing its findings
  into a notes folder;
- a project's helper that answers "where is X?" and "what broke?" and
  never edits anything;
- a clerk that turns the CSVs you download each month into a
  spreadsheet;
- a delegate you hand chores to from your phone, which asks you only
  what it cannot decide.

## How you get one

1. **Install yobuild.** It runs on macOS and Linux. Once 0.1 is
   released, download it for your machine from the
   [releases page](https://github.com/yologdev/yobuild/releases) or
   install it with `cargo install`. Claude Code can do this for you. Agents
   with plugins also need Node.js; the [guide](guide.md#installing) has
   the details.
2. **Give Claude Code the skill:** `yobuild skill install`.
3. **Ask for an agent:** "make me an agent that …". Claude Code:
   - asks what it is for, what it may change, and how you want to use it;
   - picks parts that were checked to load;
   - writes `agent.toml` (the agent's settings, a page you can read) and
     its instructions;
   - runs `yobuild check` and fixes what it finds;
   - runs it scripted (made-up model replies, no key, no cost);
   - asks you before the first live run.
4. **Use it.** `yobuild run agent.toml` prints a link to the agent's
   page. Open it in your browser, type a task, and answer its questions
   there.

You can also start from a copy (`yobuild new research --from
web-research`) and ask Claude Code to change it.

## Why yobuild

- **No code to write.** An agent is a short settings file and a prompt.
  Claude Code writes them, you can read them, and changing the agent
  means changing a line, not a program.
- **Parts that already exist, used as they are.** pi packages, DSH
  plugins and MCP servers load unchanged from npm, so you start from what
  others built. Versions are pinned in a lock file, so the agent you
  checked is the agent that runs, until you change a pin.
- **Checked before you trust it.** `yobuild check` loads every part
  without a model, a key or any cost. It says what each part adds, which
  tools run freely and which ask you, and what would fail, with a fix for
  each problem. It refuses what it cannot run safely instead of half
  running it.
- **It asks before it harms.** The approval gate lets through what it
  knows is harmless: reading, building and testing, writing new files.
  Deleting or replacing your files asks you first. With nobody there to
  answer, it is refused. Each plugin tool asks unless the agent's
  settings name it.
- **It doesn't wait on you.** A question to you doesn't stop the work:
  the agent carries on with what doesn't depend on the answer, and picks
  the answer up whenever it comes, even hours later from your phone,
  while yobuild is running.
- **It tells you what it did, and yobuild checks it.** A run can be
  required to end with a report in the sections you choose. yobuild adds
  facts the model cannot make up: files changed, commits, tokens, tool
  calls, time, and questions still waiting.
- **Spending is capped.** Each agent has a limit per run and per session,
  in dollars, turns and time. The runs above cost under two cents each.
- **Plugins don't get your keys.** Plugins and servers start without
  your credentials and other environment variables, unless the settings
  pass one on by name.
- **Where you are.** Use it in a browser, on your phone on the same
  Wi-Fi, or in the terminal.
- **Yours.** It runs on your computer, with the model provider you choose
  (DeepSeek, Anthropic, OpenAI, Google, a local model through Ollama, and
  others). There's no account and no hosted service. Your task and the
  files the agent reads go to that provider, and to whatever services its
  parts use.

## Compared with other ways

**Doing the task in a coding agent** (Claude Code, the pi coding agent)
works well while you are at the keyboard. An agent
built with yobuild fits the job you want done again and again, or done
while you are away, or done by someone you hand it to. It has only the
parts that job needs, its own spending limit, its own rules on what asks
first, and a page you can open on your phone.

**Writing it with an agent framework or SDK** (LangChain and LangGraph,
CrewAI, the OpenAI Agents SDK, the Claude Agent SDK) gives a programmer
the most control. You write the agent in Python or TypeScript. These
frameworks connect to MCP servers too, and they have ways to pause for a
person's approval. Choose one when you want to write code, run many
agents together, or deploy to a server. Choose yobuild when you'd rather
not write the agent at all, when you want pi packages and DSH plugins
unchanged, and when you want the checks, the approval gate, the report
and the page already there.

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
- **It is young.** 0.1 is the first release: the built-in agents are
  tested on every change, and much else is new.
