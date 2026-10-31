#!/usr/bin/env bash
# Print a release's notes: its section of CHANGELOG.md (the text under
# `## <version> — <date>`, up to the next `## `), then how to install it.
# The changelog is wrapped at 72 columns and GitHub draws every line break
# of a release's notes, so wrapped lines are joined again: a line goes onto
# the one before it unless either is blank or it begins a list item, a
# heading or a fence.
# `release.yml` drafts each release with them; CI runs it on Cargo.toml's
# version, so a version without its section fails before it is tagged.
#
# Usage: scripts/release-notes.sh <version>   (0.2.0, not v0.2.0; a
# pre-release such as 0.3.0-rc.1 takes 0.3.0's section when it has none)
set -euo pipefail

version=${1:?usage: scripts/release-notes.sh <version>}
changelog="$(dirname "$0")/../CHANGELOG.md"

section_of() {
  awk -v v="$1" '
    /^## / { if (found) exit; found = ($2 == v); next }
    found
  ' "$changelog"
}
section=$(section_of "$version")
# A pre-release (0.3.0-rc.1) without a section of its own takes its release's.
if [ -z "$(printf '%s' "$section" | tr -d '[:space:]')" ] && [ "${version%%-*}" != "$version" ]; then
  section=$(section_of "${version%%-*}")
fi
section=$(printf '%s\n' "$section" | awk '
  function starts(line) { return line ~ /^[ \t]*([-*+] |[0-9]+\. |#|```)/ }
  /^[ \t]*$/ { if (held != "") print held; held = ""; if (printed) blank = 1; next }
  {
    if (held != "" && !starts($0) && !fence) { sub(/^[ \t]+/, ""); held = held " " $0 }
    else {
      if (held != "") print held
      if (blank) print ""
      held = $0; blank = 0; printed = 1
    }
    if ($0 ~ /^[ \t]*```/) fence = !fence
  }
  END { if (held != "") print held }
')

if [ -z "$section" ]; then
  echo "release-notes.sh: CHANGELOG.md has no section for $version (a line \`## $version — <date>\`)" >&2
  exit 1
fi

printf '%s\n\n' "$section"
cat <<NOTES
## Install

Download the tarball for your platform below (macOS or Linux; each has its \`.sha256\`), or \`cargo install yobuild\` (\`cargo binstall yobuild\` downloads the tarball). Plugins need Node 22.19+ and npm 11.19+.

Every release: [CHANGELOG.md](https://github.com/yologdev/yobuild/blob/v$version/CHANGELOG.md).
NOTES
