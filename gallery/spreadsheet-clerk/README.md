# Spreadsheet clerk

Give it a monthly expenses CSV exported from your bank (date, description,
category, amount) and it builds a spreadsheet with a total per category,
saved as an `.xlsx` next to the CSV. It never overwrites or deletes your
files without asking: if the `.xlsx` is already there, it asks you first.

Built with the [yobuild skill](../../skill/yobuild/SKILL.md) by a coding
agent, from this request: *"I export my monthly expenses from my bank as a
CSV (date, description, category, amount). I want an agent that takes such
a CSV from my folder, builds a spreadsheet with a total per category, and
saves it as an .xlsx next to the CSV. It must never overwrite or delete my
files without asking me first. Please use the dsh-univer-office plugin for
the spreadsheet work."*

## What it is made of

- **Spreadsheets:** [`dsh-univer-office`](https://www.npmjs.com/package/dsh-univer-office)
  0.3.7, a DeepSeek Harness (DSH) plugin, granted `workspace` (its tools
  need a calling agent with a folder: yobuild's DSH host gives it one, in
  the folder you start it in). It works on a `.univer` file next to the
  CSV, in a draft: it imports the CSV, adds a "Totals by category" sheet
  with `SUMIF` formulas and a grand total, and exports the `.xlsx`.
- **Questions to you:** `ask_owner` — which CSV, when there are several;
  whether to replace an `.xlsx` that is already there. Your answer starts
  the next run.
- **Approval:** the tools that change none of your files run without
  asking (`univer_new`, `univer_import`, `univer_unit`, `univer_status`,
  `univer_inspect`, `univer_api`, `univer_lint`). Three ask you first, each
  time (Allow/Block): `univer_worktree` (the draft), **`univer_execute`**
  (the JavaScript that builds the totals sheet — read it before you allow
  it: it runs with your user's full rights) and `univer_export` (it writes
  the `.xlsx`, and would overwrite one).
- **A required ending:** File read, File written, Waiting on you.
- **Read-only otherwise:** yoagent's own tools only read, inside the folder
  you start it in.
- **Limits:** $0.50 and 40 model turns per run; $2.00 in all until yobuild
  stops. A question to you takes two runs, so up to $1.00.

## What it needs

- `DEEPSEEK_API_KEY` in the environment (the model is
  `deepseek/deepseek-flash`).
- A working `yobuild`, from a set-up checkout (Node 22.19+ and npm; see
  the skill).
- The network once: the first run installs `dsh-univer-office` and its
  packages (230 in all) from npm into yobuild's cache (`~/.yobuild`).
- About 1 GB of memory for the DSH runtime while it runs.

## What it leaves behind

- Next to your CSV: the `.xlsx`, and the `.univer` working file it was
  built in (`expenses-2026-09.univer`; the next run reuses it; delete it
  when done).
- In yobuild's DSH home (a private temp folder, removed when yobuild
  stops; your own with `YOAGENT_DSH_HOME`): the DSH session log and the
  plugin's state file. The plugin starts a helper process on 127.0.0.1
  while it runs.

## Run it

```sh
cd ~/expenses          # the folder holding the CSV
DEEPSEEK_API_KEY=… path/to/yobuild run path/to/gallery/spreadsheet-clerk/agent.toml \
  "Build the spreadsheet for expenses-2026-09.csv"
```

Open the URL it prints; the run starts when the page is open. Its
questions appear above the message box.

Try it without a model or a key (only the model is scripted; the plugin is
real), in a folder holding a CSV like the one below:

```sh
yobuild run path/to/gallery/spreadsheet-clerk/agent.toml \
  --scripted path/to/gallery/spreadsheet-clerk/scripts/demo.json \
  --once "$(cat path/to/gallery/spreadsheet-clerk/scripts/demo.task)"
```

The scripted runs stop at the draft (`univer_worktree` asks, and under
`--once` nobody answers): the draft's id comes from its result, which a
reply file cannot know. `scripts/which.json` shows two CSVs and a question;
`scripts/answer.json` the run your answer starts; `scripts/exists.json` an
`.xlsx` already there, asked about first.

## A live run

2026-10-21, `deepseek/deepseek-flash`, in a folder holding two months'
CSVs, the task "Build the spreadsheet for expenses-2026-09.csv" (8 rows),
the questions answered Allow in the page: `Completed`, 19 turns, $0.0109
(202,316 tokens in, most of them cache reads; 4,426 out). It wrote
`expenses-2026-09.xlsx` with two sheets: "Expenses" (the CSV) and "Totals
by category":

| Category | Total |
|---|---|
| Housing | 1200 |
| Groceries | 112.75 |
| Transport | 96.3 |
| Utilities | 72.4 |
| Entertainment | 24 |
| Grand total | 1505.45 |

each total a `SUMIF` over the Expenses sheet, the grand total a `SUM` —
all as the CSV adds up.
