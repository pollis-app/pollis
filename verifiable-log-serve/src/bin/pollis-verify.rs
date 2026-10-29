//! `pollis-verify` — a verification-only auditor CLI for the Pollis public
//! transparency log, trusting only the pinned log key. Every subcommand exits
//! non-zero if verification fails.

use clap::{Parser, Subcommand};
use std::process::ExitCode;
use verifiable_log_serve::{account, group, release, remote, ServeError};

/// What `--version` reports (#670): the crate version (1:1 with the
/// `pollis-verify-v*` tag) plus the commit the release workflow stamps, or
/// "source build" when unstamped rather than a commit it cannot vouch for.
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

/// Stable exit codes: `0` passed, `1` verification failed or transport/parse
/// error, `2` version skew ("upgrade your verifier", not a tampering finding;
/// #668).
const EXIT_VERSION_SKEW: u8 = 2;

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(ServeError::VersionSkew { served, supported }) => {
            eprintln!(
                "your pollis-verify is too old for this log — upgrade it.\n\
                 \n\
                 The log is published in wire format v{served}; this binary only \
                 understands up to v{supported}. This is a version mismatch, NOT a \
                 verification failure: nothing here says the log was tampered with. \
                 Download a newer pollis-verify (a `pollis-verify-v*` release) and \
                 re-run.\n\
                 \n\
                 (Note: a log's format only ever moves forward at a full republish, \
                 e.g. the #672 ML-DSA-44 key rotation. The website explorer runs the \
                 same check in the browser and ships with the site, so it moves with \
                 the log.)"
            );
            ExitCode::from(EXIT_VERSION_SKEW)
        }
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

    /// #670: guards against going back to `version.workspace = true`;
    /// `verifier-release.yml` checks the number against the tag.
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
