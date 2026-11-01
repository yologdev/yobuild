#!/usr/bin/env bash
# Print a release's notes: its section of CHANGELOG.md (the text under
# `## <version> — <date>`, up to the next `## ` outside a fenced block),
# then how to install it. `release.yml` drafts each release with them; CI
# runs it on Cargo.toml's version, so a version without its section fails
# before it is tagged, and runs `scripts/release-notes-test.sh`.
#
# The changelog is wrapped at 72 columns and GitHub draws every line break
# of a release's notes, so a wrapped line is joined onto the line before
# it, unless:
# - either line is blank;
# - the line begins a block of its own: a list item (`- `, `* `, `+ `,
#   `1. `, `1) `), a heading, a table row (`|`), a quote (`>`) or HTML
#   (`<`);
# - the line before is one nothing goes onto: a heading, a table row,
#   HTML, or a line ending in a hard break (two spaces or `\`);
# - either is in a fenced block (``` or ~~~), which is printed as it is.
# An indented code block is not told from a list item's paragraph: fence
# code instead. Carriage returns are dropped.
#
# Usage: scripts/release-notes.sh <version> [changelog]
#   <version> is 0.2.0, not v0.2.0; a pre-release such as 0.3.0-rc.1 takes
#   0.3.0's section when it has none of its own or an empty one.
#   [changelog] defaults to the repository's CHANGELOG.md.
set -euo pipefail

version=${1:?usage: scripts/release-notes.sh <version> [changelog]}
changelog=${2:-"$(dirname "$0")/../CHANGELOG.md"}

# The section's lines as written; exit 3 when no heading names the version.
section_of() {
  awk -v v="$1" '
    { sub(/\r$/, "") }
    !fence && /^## / { if (found) exit; found = ($2 == v); next }
    /^[ \t]*(```|~~~)/ {
      mark = $0; sub(/^[ \t]*/, "", mark); mark = substr(mark, 1, 3)
      if (!fence) fence = mark; else if (mark == fence) fence = ""
    }
    found
    END { exit found ? 0 : 3 }
  ' "$changelog"
}

# Blank, or only whitespace.
empty() { [ -z "$(printf '%s' "$1" | tr -d '[:space:]')" ]; }

status=0
section=$(section_of "$version") || status=$?
used=$version
base=${version%%-*}
if [ "$base" != "$version" ] && { [ "$status" -eq 3 ] || { [ "$status" -eq 0 ] && empty "$section"; }; }; then
  status=0
  section=$(section_of "$base") || status=$?
  used="$version or $base"
fi
case $status in
  0) ;;
  3) echo "release-notes.sh: $changelog has no section for $used (a line \`## $version — <date>\`)" >&2
     exit 1 ;;
  *) echo "release-notes.sh: $changelog could not be read" >&2
     exit 1 ;;
esac
if empty "$section"; then
  echo "release-notes.sh: $changelog's section for $used is empty" >&2
  exit 1
fi

section=$(printf '%s\n' "$section" | awk '
  function opens(l) { return l ~ /^[ \t]*([-*+][ \t]|[0-9]+[.)][ \t]|#+([ \t]|$)|\||>|<)/ }
  function closed(l) { return l ~ /^[ \t]*(#+([ \t]|$)|\||<)/ || l ~ /  $/ || l ~ /\\$/ }
  function flush() { if (held != "") print held; held = "" }
  function start() { flush(); if (blank) print ""; blank = 0; printed = 1 }
  fence {
    print
    mark = $0; sub(/^[ \t]*/, "", mark)
    if (substr(mark, 1, 3) == fence) fence = ""
    next
  }
  /^[ \t]*(```|~~~)/ {
    start(); print
    mark = $0; sub(/^[ \t]*/, "", mark); fence = substr(mark, 1, 3)
    next
  }
  /^[ \t]*$/ { flush(); if (printed) blank = 1; next }
  held != "" && !opens($0) && !closed(held) { sub(/^[ \t]+/, ""); held = held " " $0; next }
  { start(); held = $0 }
  END { flush() }
')

printf '%s\n\n' "$section"
cat <<NOTES
## Install

Download the tarball for your platform below (macOS or Linux; each has its \`.sha256\`), or \`cargo install yobuild\` (\`cargo binstall yobuild\` downloads the tarball). Plugins need Node 22.19+ and npm 11.19+.

Every release: [CHANGELOG.md](https://github.com/yologdev/yobuild/blob/v$version/CHANGELOG.md).
NOTES
