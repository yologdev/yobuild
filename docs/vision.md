# Vision

## The problem

Building an agent today means choosing between two bad starts:

- **Start from an agent app** (a coding agent, a chat app) and bend it. You
  inherit its UI, its session model and its idea of what an agent is for.
- **Start from a framework or a bare loop** and write everything: tools,
  policies, the frontend, the plumbing to get plugins in.

Meanwhile, the useful capabilities already exist. pi has 11,500+ packages
on npm and the DeepSeek Harness 7,200+; MCP servers cover most SaaS APIs.
But each lives inside its own host. Reusing one means adopting that host.

## The bet

**An agent is mostly a selection of capabilities, plus a little glue that
makes it *this* agent.** If a small, trusted loop can load other ecosystems'
plugins unchanged — and refuse, rather than half-run, what it cannot honour
— then building an agent becomes choosing parts and writing the glue.

yoagent showed the loop half: one `Extension` contract carries tools, tool
policies, input checks, prompt notes and stop checks, and the yoagent-rutis
bridge maps pi extensions and DSH plugins onto it. The handoff example
showed the agent half: a working delegate from an off-the-shelf planner, an
off-the-shelf search, and three small parts.

yobuild makes that the normal way to build:

1. **Pick parts by capability** from plugins that are measured to work.
2. **Declare the agent** in a manifest: model, prompt, plugins, built-ins,
   frontend.
3. **Run it** — scripted first, then live — and change parts, not code.

## Who it is for

- **People prototyping an agent** for a task of their own (a research
  delegate, a spreadsheet clerk, a code reviewer) who want something
  working in hours, then want to keep it.
- **Coding agents building agents for people.** A skill teaches Claude Code
  or any coding agent the method: find parts, vet them, write the manifest,
  run it. Most people will meet yobuild this way.
- **Plugin authors** in pi, DSH or rutis who want their work to run in more
  places without a rewrite.

## Principles

1. **Reuse before writing.** A capability that exists as a plugin is loaded,
   not reimplemented. New code is the glue only.
2. **Vet before trusting.** Every plugin is probed before use: what it
   offers, what it needs, whether it loads. Compatibility is reported as
   measured numbers, never as "10k plugins work". (Today, top 20 by
   downloads: pi 12 usable, DSH 4.)
3. **Fail closed.** A plugin that hooks something the loop cannot enforce is
   refused. A policy that cannot run denies. A question nobody answers is
   not answered for them.
4. **The owner decides.** Destructive actions ask first; with nobody there,
   they are refused. Questions do not block the work, and do not get
   invented answers.
5. **Facts over claims.** The agent's report is checked; what the host can
   count (files changed, tokens, tool calls, open questions) is added by
   the host, not the model.
6. **Small core, honest edges.** yobuild adds what agents built this way
   share, and documents what it does not guarantee (it is not a sandbox).

## What it is not

- **Not a new loop.** The loop is yoagent's; when a capability belongs in
  every agent, it goes there.
- **Not a plugin ecosystem of its own.** It hosts others' plugins; its own
  parts are few and generic.
- **Not a sandbox.** Plugins run with their process's rights. The approval
  gate is a guardrail for selected operations.
- **Not an agent app.** No accounts, no hosted service, no marketplace —
  at least not in this plan.

## What success looks like

- A useful agent from a manifest and at most one small custom tool,
  in an afternoon.
- Coding agents using the skill to build agents for their users, end to end.
- `handoff` and `coding` as manifests, not hand-written binaries.
- Other people's agents in an examples gallery, built this way.
