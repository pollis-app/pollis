//! Build script for `verifiable-log-serve`.
//!
//! Its only job is to tell cargo that `pollis-verify`'s `--version` string
//! depends on `POLLIS_VERIFY_GIT_SHA` (#670). Without this, a cached build
//! directory — which the release workflow has, via `Swatinem/rust-cache` — would
//! happily reuse an object file stamped with a previous release's commit, and
//! ship a binary that names the wrong source.

fn main() {
    println!("cargo:rerun-if-env-changed=POLLIS_VERIFY_GIT_SHA");
}
