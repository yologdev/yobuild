#!/usr/bin/env bash
# Copy the pi and DeepSeek Harness (DSH) adapters of the yoagent-rutis crate
# Cargo.lock resolves (its `plugins/`: `pi/`, `dsh/`, `yoagent.d.ts`) into
# `adapters/`, where the examples and tests load them and `npm ci` installs
# their packages. The crate's own files, in cargo's registry, are never
# installed into (`yoagent_rutis::adapters`). A copy of another version is
# replaced; one of this version is left as it is (with its node_modules).
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

if [ "$(cat "$dest/.version" 2>/dev/null)" = "$version" ]; then
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
echo "$version" > "$dest/.version"
echo "copied: adapters/ (yoagent-rutis $version)"
