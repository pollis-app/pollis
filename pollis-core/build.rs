use std::process::Command;

fn main() {
    // Commit for `verify_own_build`; `POLLIS_GIT_COMMIT` overrides git, and neither leaves it unset.
    let commit = std::env::var("POLLIS_GIT_COMMIT")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(git_head_commit);
    if let Some(commit) = commit {
        println!("cargo:rustc-env=POLLIS_GIT_COMMIT={commit}");
    }

    println!("cargo:rerun-if-env-changed=POLLIS_GIT_COMMIT");
    println!("cargo:rerun-if-changed=../.git/HEAD");
}

/// `git rev-parse HEAD`, or `None` if git or the checkout is unavailable.
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
