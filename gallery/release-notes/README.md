# Release notes

It drafts release notes for a git project. It takes every commit since the
last version tag (`v1.2.3`) and groups them into features, fixes and other
changes, in plain words for the people who use the project. It then says
which commits it was not sure how to group. It only reads: it never
commits, tags or changes a file.

Built with the yobuild skill (`yobuild skill install`) by a coding agent,
from this request: *"I want an agent that drafts release notes for my
project: everything since the last version tag, grouped into features,
fixes and other changes, in plain words for the people who use it. It
should only read: never commit, tag or change my files. At the end, give
me the draft and tell me which commits it wasn't sure how to group. Show
me what it does on my project, and what it does when there is no tag
yet."*

It is the gallery's agent with **a tool of its own**. None of the parts
yobuild lists reads git history in an agent that may not write. So the
coding agent wrote one small rutis plugin for it, and that plugin is the
whole of its custom code.

## What it is made of

- **Its tool:** `plugins/release-history.ts`, a rutis plugin in its own
  npm package (`plugins/package.json`, with `@arcships/rutis` and
  `@arcships/rutis-runtime` 0.12.0). It offers two tools, both allowed:
  - `version_tags`: the `vX.Y.Z` tags in HEAD's history, newest first;
  - `commits_since`: each commit since a tag (or in all of history when
    there is none), with its message and the files it changed.

  It runs `git` with fixed arguments (`--no-pager`, `--no-optional-locks`,
  no signature checks) and refuses an argument that is not a version tag.
  It is granted `workspace`, which gives it the folder yobuild runs in.
- **Reading:** `tools = "read-only"`: read, list and search only, inside
  the project. It has no `bash` and no tool that writes.
- **The report:** "Release notes draft" and "Unsure how to group".
- **Limits:** $0.25 and 20 model turns per run; $1 in all until yobuild
  stops.

## What it does not do

- **It does not tag or publish.** The draft is its last answer; the
  release is yours.
- **It does not read the changes themselves.** It reads each commit's
  message and the files it touched, and the files as they are now.
- **Its plugin is your code, not a sandbox.** yobuild's gate judges the
  agent's tool calls, not what a plugin runs. This one runs only the git
  commands written in it. Read `plugins/release-history.ts` before you
  change it.

## What it needs

- `DEEPSEEK_API_KEY` in the environment (the model is
  `deepseek/deepseek-flash`).
- `yobuild` installed (see yobuild's README), with Node 22.19+ and npm
  11.19+ on PATH, and `git`.
- **Once, before the first run:** its plugin's package installed. yobuild
  installs the plugins an agent pins, but not a plugin of the agent's own:

  ```sh
  cd my-release-notes/plugins && npm ci
  ```

  Until then `yobuild check` and `run` refuse, naming that command.

## Run it

Get a copy of its folder with `yobuild new my-release-notes --from
release-notes` (the paths below then name that folder), and install its
plugin's package as above.

```sh
cd path/to/your-project
DEEPSEEK_API_KEY=… yobuild run path/to/gallery/release-notes/agent.toml
```

Open the URL it prints and ask it to draft the release notes.

Try it without a model or a key, in a repository like the one `scripts/`
expects: a tiny CLI tagged `v0.1.0` and `v0.2.0`, then a fix, a feature
and a docs change (`tests/gallery_test.rs` makes it). Only the model is
scripted; git really runs.

```sh
yobuild run path/to/gallery/release-notes/agent.toml \
  --scripted path/to/gallery/release-notes/scripts/demo.json \
  --once "$(cat path/to/gallery/release-notes/scripts/demo.task)"
```

`scripts/no-tag.json` is the same history before any tag: the notes then
cover every commit, as a first release.

## A live run

2026-10-30, `deepseek/deepseek-flash`, `--once`, in the sample repository
the tests make, from a copy made with `yobuild new --from release-notes`
(its plugin's package installed with `npm ci`). The task was
`scripts/demo.task`:
- **Calls:** `version_tags`, `list_files`, `commits_since` with `v0.2.0`,
  and three `read_file`s (the files the commits touched). Plugin time
  was 189 ms; its runtime took 66 MB.
- **Cost:** `Cost: $0.0013 (7016 tokens in, 563 out; 4 turns)`.
- **The draft:** the shout helper under Features, the greeting's
  punctuation under Fixes, and the README under Other changes, in plain
  words with no hashes.
- **Unsure how to group:** `601002b` "feat: add shout command". It is
  only a helper, so it may not be wired into the CLI yet.

`Completed`. The repository's HEAD, tags and files were as before.
