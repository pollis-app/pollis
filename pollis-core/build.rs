use std::process::Command;

fn main() {
    // Bake the exact source revision of this build so `verify_own_build` can name
    // the commit alongside the version — matching the `commit` field the release
    // attest job (`scripts/attest-binaries.sh`) logs into each BinaryRecord leaf.
    //
    // Prefer an explicit override (release CI can pass the tag commit as
    // `POLLIS_GIT_COMMIT`), else fall back to `git rev-parse HEAD`. If neither is
    // available (e.g. a source tarball checked out without a `.git`), we simply
    // don't set the env — `option_env!("POLLIS_GIT_COMMIT")` is then `None` and
    // the command omits the commit gracefully. We never invent a fake value.
    let commit = std::env::var("POLLIS_GIT_COMMIT")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(git_head_commit);
    if let Some(commit) = commit {
        println!("cargo:rustc-env=POLLIS_GIT_COMMIT={commit}");
    }

    // Re-bake when the override changes or the checked-out HEAD moves.
    println!("cargo:rerun-if-env-changed=POLLIS_GIT_COMMIT");
    println!("cargo:rerun-if-changed=../.git/HEAD");
}

/// The current `git rev-parse HEAD` (40-hex), or `None` if git is unavailable or
/// this is not a checkout — best-effort, never fatal to the build.
fn git_head_commit() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if sha.is_empty() {
        None
    } else {
        Some(sha)
    }
}
