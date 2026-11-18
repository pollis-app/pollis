//! Built only by tests/dev_grants_release.rs, which expects this to compile in
//! a debug build and to FAIL to compile in a release build, because
//! `dev_grants` must not exist there.

fn main() {
    let _ = pollis_app_manifest::dev_grants::load(std::path::Path::new("grants.json"));
}
