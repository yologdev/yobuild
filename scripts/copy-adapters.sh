#!/usr/bin/env bash
# Copy the pi and DeepSeek Harness (DSH) adapters of the yoagent-rutis crate
# Cargo.lock resolves (its `plugins/`: `pi/`, `dsh/`, `yoagent.d.ts`) into
# `adapters/`, where the examples and tests load them and `npm ci` installs
# their packages. The crate's own files (in cargo's registry, the folder
# `yoagent_rutis::adapters` names; found here through `cargo metadata`) are
# never installed into.
#
# The copy is kept while it is that version, from that folder, with the
# same files (a checksum, node_modules aside); otherwise it is copied again,
# its node_modules dropped: a changed source (a path or `[patch]`
# dependency) and a copy edited by hand are both replaced.
#
# Needs cargo and node.
#
#   scripts/copy-adapters.sh     # then: npm ci in adapters/pi and adapters/dsh
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
dest="$repo/adapters"

# Exactly one yoagent-rutis in the lock, else say so rather than pick one.
crate=$(cargo metadata --format-version 1 --manifest-path "$repo/Cargo.toml" --locked |
  node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{
    if(!s.trim())process.exit(1) // cargo failed and said why
    const ps=JSON.parse(s).packages.filter(p=>p.name==="yoagent-rutis");
    if(ps.length!==1){console.error(`error: expected one yoagent-rutis in Cargo.lock, found ${ps.length}`);process.exit(1)}
    console.log(ps[0].version+" "+require("path").dirname(ps[0].manifest_path))})')
version=${crate%% *}
src="${crate#* }/plugins"

# A checksum of the adapters' files in `$1` (paths relative, node_modules aside).
files_sum() {
  (cd "$1" && find pi dsh yoagent.d.ts -type f ! -path '*/node_modules/*' -print0 2>/dev/null |
    LC_ALL=C sort -z | xargs -0 cksum | cksum)
}

stamp="$version $src $(files_sum "$src")"
if [ -f "$dest/.stamp" ] && [ "$(cat "$dest/.stamp")" = "$stamp" ] &&
  [ "$(files_sum "$dest")" = "$(files_sum "$src")" ]; then
  echo "up to date: adapters/ (yoagent-rutis $version)"
  exit 0
fi
rm -rf "$dest"
mkdir -p "$dest"
for d in pi dsh; do
  mkdir -p "$dest/$d"
  (cd "$src/$d" && tar -cf - --exclude node_modules .) | (cd "$dest/$d" && tar -xf -)
done
cp "$src/yoagent.d.ts" "$dest/yoagent.d.ts"
# Last: an interrupted copy has no stamp, so the next run copies again.
echo "$stamp" > "$dest/.stamp"
echo "copied: adapters/ (yoagent-rutis $version)"
