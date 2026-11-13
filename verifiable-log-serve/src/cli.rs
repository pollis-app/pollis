//! The `pollis-verify` command line, in the library so tests can parse published invocations
//! (`tests/doc_invocations.rs`) against the exact definition the binary ships.

use clap::{Parser, Subcommand};

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
pub fn version_string() -> &'static str {
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
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
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
    /// the artifacts, the logged SBOM (if any) and hashes. Exits non-zero if invalid.
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
