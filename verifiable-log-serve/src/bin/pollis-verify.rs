//! `pollis-verify` — independently verify the Pollis public transparency log.
//!
//! A verification-only CLI for auditors: it fetches the published artifacts over
//! HTTP(S) and checks them, trusting only the pinned log public key (ML-DSA-44
//! since #668; while that rotation is in flight no key is pinned and the
//! verifier reports *unverified* rather than trusting the served one). It does
//! not run, serve, or build anything — that's what the operator `serve` binary
//! is for. Every subcommand exits non-zero if verification fails, so they slot
//! into CI and scripts.

use clap::{Parser, Subcommand};
use std::process::ExitCode;
use verifiable_log_serve::{account, group, release, remote, ServeError};

/// What `--version` reports (#670).
///
/// The number comes from the crate's own concrete `version` field, which maps
/// 1:1 onto the `pollis-verify-v*` release tag — so a downloaded binary can
/// state which release it is. The commit is an annotation on top: the release
/// and rebuild-verify workflows pass `POLLIS_VERIFY_GIT_SHA`, since the SHA is
/// what the recipe in the binaries tree pins. A plain `cargo build` has no tag
/// behind it and says so rather than naming a commit it cannot vouch for — an
/// auditor comparing a local build against a release should see that
/// difference, not have it papered over.
fn version_string() -> &'static str {
    static VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    VERSION
        .get_or_init(|| {
            let version = env!("CARGO_PKG_VERSION");
            match option_env!("POLLIS_VERIFY_GIT_SHA") {
                Some(sha) => format!("{version} ({sha})"),
                None => format!("{version} (source build)"),
            }
        })
        .as_str()
}

#[derive(Parser)]
#[command(
    name = "pollis-verify",
    about = "Independently verify the Pollis public transparency log.",
    version = version_string()
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Verify the whole log: every STH signature, all inclusion proofs, and
    /// consistency between tree sizes — for BOTH the commit-log tree and the
    /// account-key tree. Trusts only the pinned public key. If the account-key
    /// tree is absent it prints a warning and still verifies the commit log.
    Remote {
        /// Base URL of the transparency log, e.g. https://verify.pollis.com
        base_url: String,
    },
    /// Verify a single conversation's commit chain. Use a conversation id (an
    /// MLS conversation id), not a workspace name. Exits non-zero if invalid.
    Group {
        /// Base URL of the transparency log, e.g. https://verify.pollis.com
        base_url: String,
        /// Conversation id to verify.
        conversation_id: String,
        /// Print the report as JSON instead of a human-readable summary.
        #[arg(long)]
        json: bool,
    },
    /// Verify a single user's account-key history chain: that every published
    /// identity-key version is provably included in the signed account tree and
    /// the versions are strictly increasing. Exits non-zero if invalid.
    Account {
        /// Base URL of the transparency log, e.g. https://verify.pollis.com
        base_url: String,
        /// User id to verify.
        user_id: String,
        /// Print the report as JSON instead of a human-readable summary.
        #[arg(long)]
        json: bool,
    },
    /// Verify a released version's binaries: that every published artifact for a
    /// tag is provably included in the signed binaries tree (under the binaries
    /// domain-separated STH), the tree satisfies the binary invariant (no forked
    /// re-issue, payload/signed pairing), and the recorded hashes match. Prints
    /// the artifacts + hashes. Exits non-zero if invalid.
    Release {
        /// Base URL of the transparency log, e.g. https://verify.pollis.com
        base_url: String,
        /// Release tag to verify, e.g. v1.3.0.
        release_tag: String,
        /// Print the report as JSON instead of a human-readable summary.
        #[arg(long)]
        json: bool,
    },
}

fn run() -> Result<bool, ServeError> {
    match Cli::parse().command {
        Command::Remote { base_url } => {
            let report = remote::verify_remote(&base_url)?;
            report.print();
            Ok(report.ok)
        }
        Command::Group {
            base_url,
            conversation_id,
            json,
        } => {
            let report = group::verify_group(&base_url, &conversation_id)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                report.print();
            }
            Ok(report.chain_valid)
        }
        Command::Account {
            base_url,
            user_id,
            json,
        } => {
            let report = account::verify_account(&base_url, &user_id)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                report.print();
            }
            Ok(report.chain_valid)
        }
        Command::Release {
            base_url,
            release_tag,
            json,
        } => {
            let report = release::verify_release(&base_url, &release_tag)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                report.print();
            }
            Ok(report.chain_valid)
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::version_string;

    /// Reads `version` out of the root manifest's `[workspace.package]` table.
    fn workspace_placeholder_version() -> String {
        let root = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../Cargo.toml"))
            .expect("read the workspace manifest");
        let table = root
            .split("[workspace.package]")
            .nth(1)
            .expect("workspace manifest has a [workspace.package] table");
        for line in table.lines() {
            if let Some(value) = line.trim().strip_prefix("version") {
                return value
                    .trim_start_matches([' ', '='])
                    .trim()
                    .trim_matches('"')
                    .to_string();
            }
        }
        panic!("[workspace.package] has no version");
    }

    /// #670: the binary released as `pollis-verify-v0.4.0` reported `1.1.0` —
    /// the frozen workspace placeholder, which is not a version of anything —
    /// so an auditor holding a download could not correlate it with the release
    /// it came from. This is the auditor-facing tool for the transparency log,
    /// so being unable to state its own identity is load-bearing, not cosmetic.
    ///
    /// Guards the specific regression: `verifiable-log-serve` going back to
    /// `version.workspace = true`. `verifier-release.yml` covers the other half
    /// (the number drifting away from the release tag), which can only be
    /// checked where the tag exists.
    #[test]
    fn the_reported_version_is_this_crates_own_and_not_the_workspace_placeholder() {
        let version = env!("CARGO_PKG_VERSION");
        assert_ne!(
            version,
            workspace_placeholder_version(),
            "verifiable-log-serve is inheriting the workspace placeholder again — \
             give it a concrete `version` matching its pollis-verify-v* tag (#670)"
        );
        assert!(
            version_string().starts_with(version),
            "--version must lead with the release number, got {:?}",
            version_string()
        );
    }

    /// A build with no release tag behind it has to say so. Naming a commit it
    /// cannot vouch for would be worse than the placeholder it replaced: an
    /// auditor comparing a local build against a download should see the
    /// difference. `cargo test` is exactly such an unstamped build, unless the
    /// caller stamped one, in which case it must be a full commit hash.
    #[test]
    fn the_build_provenance_suffix_is_either_a_commit_or_an_honest_disclaimer() {
        let reported = version_string();
        let suffix = reported
            .split_once(" (")
            .and_then(|(_, rest)| rest.strip_suffix(')'))
            .unwrap_or_else(|| panic!("--version must carry a provenance suffix, got {reported:?}"));
        match option_env!("POLLIS_VERIFY_GIT_SHA") {
            Some(sha) => assert_eq!(suffix, sha),
            None => assert_eq!(suffix, "source build"),
        }
    }
}
