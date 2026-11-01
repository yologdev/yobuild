#!/usr/bin/env bash
# Check scripts/release-notes.sh on tests/fixtures/release-notes/: each
# Markdown shape it must keep or join (`1.0.0.md` is the expected notes),
# CRLF line endings, a pre-release, the last section, and the refusals.
# CI runs it on Linux's awk; run it on macOS's too after changing the
# script.
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
notes="$here/scripts/release-notes.sh"
fixtures="$here/tests/fixtures/release-notes"
changelog="$fixtures/CHANGELOG.md"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
failed=0

fail() { echo "FAIL: $*" >&2; failed=1; }

# The same notes, every shape as expected.
same() { # <what> <expected file> <version> <changelog>
  if ! "$notes" "$3" "$4" > "$work/out" 2> "$work/err"; then
    fail "$1: exited non-zero: $(cat "$work/err")"
  elif ! diff -u "$2" "$work/out" > "$work/diff"; then
    fail "$1: notes differ:"; cat "$work/diff" >&2
  fi
}

# Refused, saying <text>.
refused() { # <what> <text> <version> <changelog>
  if "$notes" "$3" "$4" > /dev/null 2> "$work/err"; then
    fail "$1: exited 0"
  elif ! grep -qF -- "$2" "$work/err"; then
    fail "$1: said \"$(cat "$work/err")\", not \"$2\""
  fi
}

same "1.0.0" "$fixtures/1.0.0.md" 1.0.0 "$changelog"

sed 's/$/\r/' "$changelog" > "$work/crlf.md"
same "1.0.0 with CRLF line endings" "$fixtures/1.0.0.md" 1.0.0 "$work/crlf.md"

sed 's|/blob/v1.0.0/|/blob/v1.0.0-rc.1/|' "$fixtures/1.0.0.md" > "$work/rc.md"
same "1.0.0-rc.1, taking 1.0.0's section" "$work/rc.md" 1.0.0-rc.1 "$changelog"

"$notes" 0.7.0 "$changelog" > "$work/last"
grep -qx 'The last section, at the end of the file.' "$work/last" \
  || fail "0.7.0, the last section: $(head -1 "$work/last")"

refused "a version with no section" "has no section for 0.6.0" 0.6.0 "$changelog"
refused "an empty section" "section for 0.8.0 is empty" 0.8.0 "$changelog"
refused "a pre-release whose release's section is empty" \
  "section for 0.8.0-rc.1 or 0.8.0 is empty" 0.8.0-rc.1 "$changelog"
refused "a changelog that is not there" "could not be read" 1.0.0 "$work/none.md"

if [ "$failed" -ne 0 ]; then exit 1; fi
echo "ok: release-notes.sh keeps every shape, reads CRLF, takes a pre-release's release, and refuses what it must"
