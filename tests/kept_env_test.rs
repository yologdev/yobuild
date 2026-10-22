//! The kept environment (design §12 "Step 5") with a real Node runtime: a
//! runtime inherits only what `inherit` keeps and what `keep` (a name or a
//! prefix) and its own `env` entries say, and rutis's channel token in
//! loopback handover still reaches it (it connects), a stale one in this
//! process's environment notwithstanding.
//!
//! A file of its own, holding one test only: it sets process-wide variables
//! — `RUTIS_LOCAL_HANDOVER` (read by rutis in this process as it spawns a
//! runtime) among them — before anything else runs. Needs
//! `npm ci` in `frontend/plugins/`; skipped (passing, with `SKIPPED:`)
//! without it unless `YOAGENT_RUTIS_REQUIRE_RUNTIMES=1`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rutis::Ctx;
use yobuild::frontend::host::{Inherit, PluginHost, Row};

fn plugins() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("frontend/plugins")
}

#[cfg(unix)]
#[test]
fn a_runtime_inherits_only_the_kept_environment_and_what_it_is_granted() {
    if !plugins()
        .join("node_modules/@arcships/rutis-runtime")
        .exists()
    {
        assert!(
            std::env::var("YOAGENT_RUTIS_REQUIRE_RUNTIMES").as_deref() != Ok("1"),
            "the plugins' packages are not installed (npm ci in {})",
            plugins().display()
        );
        println!("SKIPPED: npm ci in {} first", plugins().display());
        return;
    }
    // Before any thread of this test process reads the environment: the
    // token rutis sets on the command it spawns, and a stale one here that
    // must neither be taken out (it would remove rutis's) nor win.
    std::env::set_var("RUTIS_LOCAL_HANDOVER", "loopback");
    std::env::set_var("RUTIS_CHANNEL_TOKEN", "stale");
    std::env::set_var("YOBUILD_KEPT_A", "a");
    std::env::set_var("YOBUILD_KEPT_B", "b");
    std::env::set_var("YOBUILD_UNRELATED", "u");
    std::env::set_var("YOBUILD_GRANTED_X", "x");
    // Set here too: the launch's own value wins, not taken out.
    std::env::set_var("YOBUILD_SET", "parent");
    std::env::set_var("RUTIS_CA", "/nonexistent/ca.pem");
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(kept());
}

#[cfg(unix)]
async fn kept() {
    let root = Ctx::root().unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), r#"{ "type": "module" }"#).unwrap();
    let names = [
        "PATH", "YOBUILD_KEPT_A", "YOBUILD_KEPT_B", "YOBUILD_UNRELATED", "YOBUILD_GRANTED_X",
        "YOBUILD_SET", "RUTIS_CA", "RUTIS_LOCAL_HANDOVER",
    ];
    let writer = |name: &str| {
        let file = dir.path().join(format!("{name}.mjs"));
        let out = dir.path().join(format!("{name}.txt"));
        // How the runtime's channel came (its spec is an argument: `tcp:…`
        // in loopback handover, which needs rutis's token, `fd:…`
        // otherwise), and the token as a plugin sees it: the runtime reads
        // it, so never the stale one (whether it then drops it is the
        // runtime's affair).
        let token = "channel: process.argv.some((a) => a.startsWith('tcp:')) ? 'loopback' : 'other', stale: process.env.RUTIS_CHANNEL_TOKEN === 'stale'";
        let read: Vec<String> = names
            .iter()
            .map(|n| format!("{n}: process.env.{n} ?? null"))
            .chain([token.to_owned()])
            .collect();
        std::fs::write(
            &file,
            format!(
                "import {{ writeFileSync }} from 'node:fs'\n\
                 export const name = '{name}'\n\
                 export function apply() {{ writeFileSync({:?}, JSON.stringify({{ {} }})) }}\n",
                out.display().to_string(),
                read.join(", ")
            ),
        )
        .unwrap();
        (file, out)
    };
    let (a, a_out) = writer("a");
    let (b, b_out) = writer("b");
    let mut host = PluginHost::builder()
        .node("one", plugins())
        .node("two", plugins())
        .inherit(Inherit::only(["PATH", "YOBUILD_KEPT_A"], ["YOBUILD_KEPT_"]))
        .withhold("YOBUILD_KEPT_B")
        .keep("two", "YOBUILD_GRANTED_*")
        .env("two", "YOBUILD_SET", "set")
        .start(&root)
        .await
        .expect("the runtimes start and connect");
    host.load([
        Row::new("a", a.display()).runtime("one"),
        Row::new("b", b.display()).runtime("two"),
    ])
    .await
    .unwrap();
    let path = serde_json::to_string(&std::env::var("PATH").unwrap()).unwrap();
    // Kept by name and by prefix; a withheld one gone though its prefix is
    // kept; the rest gone — a remote link's TLS file too, so a stray one no
    // longer fails the runtime.
    assert_eq!(
        written(&a_out).await,
        format!(
            r#"{{"PATH":{path},"YOBUILD_KEPT_A":"a","YOBUILD_KEPT_B":null,"YOBUILD_UNRELATED":null,"YOBUILD_GRANTED_X":null,"YOBUILD_SET":null,"RUTIS_CA":null,"RUTIS_LOCAL_HANDOVER":null,"channel":"loopback","stale":false}}"#
        )
    );
    // Its grant (a prefix) and its own `env` entry: that runtime only.
    assert_eq!(
        written(&b_out).await,
        format!(
            r#"{{"PATH":{path},"YOBUILD_KEPT_A":"a","YOBUILD_KEPT_B":null,"YOBUILD_UNRELATED":null,"YOBUILD_GRANTED_X":"x","YOBUILD_SET":"set","RUTIS_CA":null,"RUTIS_LOCAL_HANDOVER":null,"channel":"loopback","stale":false}}"#
        )
    );
    root.shutdown().await.unwrap();
}

/// The text of `path` once a plugin wrote it.
#[cfg(unix)]
async fn written(path: &Path) -> String {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if let Ok(text) = std::fs::read_to_string(path) {
                return text;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the plugin wrote")
}
