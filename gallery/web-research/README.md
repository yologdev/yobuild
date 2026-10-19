# Web research

Ask it a question and it researches it on the web, then gives you a short
answer and the sources it used. If your question is too broad, it asks you
to narrow it down instead of guessing.

Built with the [yobuild skill](../../skill/yobuild/SKILL.md) by a coding
agent, from this request: *"I want an agent that researches a question on
the web for me and gives me a short answer with the sources it used. If my
question is too broad, it should ask me to narrow it down rather than
guess."*

## What it is made of

- **Search:** [`dsh-free-search`](https://www.npmjs.com/package/dsh-free-search)
  0.8.1, a DeepSeek Harness (DSH) plugin that searches Bing and reads the
  pages it finds. It needs no search key. Its seven searches run without
  asking; `free_search_test` asks first.
- **Questions to you:** `ask_owner`. When a question is too broad, it
  offers two to four narrower ones and ends the run. Your answer starts the
  next run.
- **A required ending:** every answer has three sections: Short answer,
  Sources used, Waiting on you.
- **Read-only:** it cannot write or run anything on your computer. It can
  still read files in the folder you start it in, so start it in an empty
  folder.
- **Limits:** $0.50 and 20 model turns per run. A question it asks you to
  narrow takes two runs, so it can cost up to $1.00.

## What it needs

- `DEEPSEEK_API_KEY` in the environment (the model is
  `deepseek/deepseek-flash`).
- The network: the searches go to Bing and the pages it finds.
- A working `yobuild`, from a set-up checkout (see the skill).

## Run it

```sh
mkdir -p ~/research && cd ~/research
DEEPSEEK_API_KEY=… path/to/yobuild run path/to/gallery/web-research/agent.toml "your question"
```

Open the URL it prints; the run starts when the page is open. Its
questions to you appear above the message box.

Try it without a model or a key (only the model is scripted; the searches
are real):

```sh
yobuild run path/to/gallery/web-research/agent.toml \
  --scripted path/to/gallery/web-research/scripts/demo.json \
  --once "$(cat path/to/gallery/web-research/scripts/demo.task)"
```

## A live run

2026-10-19, `deepseek/deepseek-flash`, `--once`, the task "When does
Node.js 22 reach end of life?": one search (`domain_search`, endoflife.date,
0.2 s), then:

> **Short answer:** Node.js 22 ("Jod") reaches end of life on 2027-04-30.
> It is in maintenance LTS (since 2025-10-28): critical fixes only, until
> April 2027. High confidence, from the endoflife.date dataset, which
> tracks the official release schedule; not cross-checked against a
> second source.
>
> **Sources used:** endoflife.date, Node.js (https://endoflife.date/nodejs)
>
> **Waiting on you:** none

`Completed`. Its cost is not recorded: `--once` does not print it (the
web page shows each run's).
