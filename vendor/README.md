# vendor

`yoagent-rutis/plugins/` holds the pi and DeepSeek Harness (DSH) adapters
(`pi/`, `dsh/`, and the `yoagent.d.ts` they import) exactly as the
[yoagent-rutis](https://crates.io/crates/yoagent-rutis) crate in `Cargo.lock`
ships them. The crate does not expose a path to them, and they need their own
`npm ci`, so they are copied here.

- `scripts/check-vendor.sh` checks the copy against the crate (CI runs it).
- After upgrading yoagent-rutis: `scripts/check-vendor.sh --update`, then
  `npm ci` in `pi/` and `dsh/`.

Do not edit these files here; change them in yoagent-rutis.
