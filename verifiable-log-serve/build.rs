//! Rebuild when `POLLIS_VERIFY_GIT_SHA` changes, so a cached build never ships a
//! `--version` naming a previous release's commit (#670).

fn main() {
    println!("cargo:rerun-if-env-changed=POLLIS_VERIFY_GIT_SHA");
}
