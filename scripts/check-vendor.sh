#!/usr/bin/env bash
# Check that vendor/yoagent-rutis/plugins is byte for byte the adapters in the
# yoagent-rutis crate Cargo.lock resolves (pi/, dsh/, yoagent.d.ts). They are
# copied because the crate exposes no path to them; a yoagent-rutis upgrade
# must refresh the copy, and this fails until it does.
#
#   scripts/check-vendor.sh           # check
#   scripts/check-vendor.sh --update  # replace the copy with the crate's
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
vendor="$repo/vendor/yoagent-rutis/plugins"

crate=$(cargo metadata --format-version 1 --manifest-path "$repo/Cargo.toml" --locked |
  node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{
    const p=JSON.parse(s).packages.find(p=>p.name==="yoagent-rutis");
    if(!p){process.exit(1)} console.log(p.version+" "+require("path").dirname(p.manifest_path))})')
version=${crate%% *}
dir=${crate#* }

if [ "${1:-}" = --update ]; then
  for d in pi dsh; do
    find "$vendor/$d" -mindepth 1 -maxdepth 1 ! -name node_modules -exec rm -rf {} +
    (cd "$dir/plugins/$d" && tar -cf - --exclude node_modules .) | (cd "$vendor/$d" && tar -xf -)
  done
  cp "$dir/plugins/yoagent.d.ts" "$vendor/yoagent.d.ts"
  echo "vendor/yoagent-rutis/plugins updated to yoagent-rutis $version"
  exit 0
fi

status=0
for d in pi dsh; do
  diff -r -x node_modules "$dir/plugins/$d" "$vendor/$d" || status=1
done
diff "$dir/plugins/yoagent.d.ts" "$vendor/yoagent.d.ts" || status=1
if [ $status -ne 0 ]; then
  echo "error: vendor/yoagent-rutis/plugins differs from yoagent-rutis $version; run scripts/check-vendor.sh --update" >&2
  exit 1
fi
echo "vendor/yoagent-rutis/plugins matches yoagent-rutis $version"
