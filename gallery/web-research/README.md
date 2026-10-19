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
- **Read-only:** it cannot write, change or delete files in your folders,
  or run commands. It can still read files in the folder you start it in,
  so start it in an empty folder.
- **Limits:** $0.25, 20 model turns and 10 minutes per run; $2.00 in all
  until yobuild stops. A question it asks you to narrow takes two runs, so
  up to $0.50.

## What it needs

- `DEEPSEEK_API_KEY` in the environment (the model is
  `deepseek/deepseek-flash`).
- A working `yobuild`, from a set-up checkout (Node 22.19+ and npm; see
  the skill).
- The network: the searches go to Bing and the pages it finds. The first
  run also installs `dsh-free-search` from npm into yobuild's cache
  (`~/.yobuild`).

## What it keeps outside its folder

`dsh-free-search` keeps the pages it read in a cache,
`~/.cache/dsh-free-search/research.db`, which `cache_search` reads back in
later runs. Set `RESEARCH_CACHE_DIR` to put it elsewhere; delete the folder
to clear it.

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

`scripts/broad.json` shows a question too broad (it asks, and ends with
the question waiting); `scripts/answer.json` the run your answer starts.

## A live run

2026-10-19, `deepseek/deepseek-flash`, `--once`, the task "When does
Node.js 20 reach end of life?": six searches (`domain_search`, `research`
twice, `advanced_search` twice, `platform_search`; 6.9 s of plugin time,
from yobuild's `Plugin time this run` notice), then:

> **Short answer:** Node.js 20 ("Iron") reached end-of-life on 30 April
> 2026. It entered Maintenance LTS in October 2024; after that date it gets
> no patches or security fixes, so any 20.x install is now unsupported. The
> usual upgrade path is Node.js 22 LTS (supported until 30 April 2027) or
> Node.js 24 (until 30 April 2028).
>
> **Sources used:** endoflife.date (Node.js); the nodejs/Release README;
> nodejs.org's EOL page; two articles on Node 20's end of life.
>
> **Waiting on you:** none

`Completed`. Its cost is not recorded: `--once` does not print it (the
web page shows each run's).
