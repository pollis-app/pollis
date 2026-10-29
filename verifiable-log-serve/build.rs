//! Build script for `verifiable-log-serve`.
//!
//! Tells cargo that `pollis-verify --version` depends on `POLLIS_VERIFY_GIT_SHA` (#670), so a
//! cached build never names the wrong commit.

fn main() {
    println!("cargo:rerun-if-env-changed=POLLIS_VERIFY_GIT_SHA");
}
