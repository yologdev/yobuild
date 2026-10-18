#!/usr/bin/env bash
# Hand a task to the handoff agent (examples/handoff.toml) and walk away. It
# works in the directory you run this from — the project you hand off — and
# prints a URL to follow it, answer its questions and read its reports.
#
#   mkdir research && cd research && git init
#   path/to/yobuild/scripts/try-handoff.sh --live "research the best headphones; ask my budget"
#   path/to/yobuild/scripts/try-handoff.sh --phone "…"   # serve on the LAN, for a phone
#   path/to/yobuild/scripts/try-handoff.sh --demo         # the scripted model (no key)
#
# --live needs DEEPSEEK_API_KEY; --demo answers with examples/scripts/handoff.json
# (you are the owner: answer its question in the page). Other flags go to
# `yobuild run` (--web ADDR, --measure). Needs Rust (cargo), Node 22.19+ and npm.
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
args=()
for arg in "$@"; do
  case "$arg" in
    -h|--help) sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    --live)
      [ -n "${DEEPSEEK_API_KEY:-}" ] || { echo "error: --live needs DEEPSEEK_API_KEY" >&2; exit 1; } ;;
    --demo) args+=(--scripted "$repo/examples/scripts/handoff.json") ;;
    --phone) args+=(--web 0.0.0.0:8787) ;;
    *) args+=("$arg") ;;
  esac
done
# The plugin runtimes and the examples' packages (installed once; checks
# Rust, Node and npm first).
"$repo/scripts/try-coding-agent.sh" --setup-only >/dev/null
exec cargo run --quiet --manifest-path "$repo/Cargo.toml" -- \
  run "$repo/examples/handoff.toml" ${args[@]+"${args[@]}"}
