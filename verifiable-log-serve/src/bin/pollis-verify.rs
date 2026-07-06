//! `pollis-verify` — independently verify the Pollis public transparency log.
//!
//! A verification-only CLI for auditors: it fetches the published artifacts over
//! HTTP(S) and checks them, trusting only the pinned Ed25519 public key. It does
//! not run, serve, or build anything — that's what the operator `serve` binary
//! is for. Both subcommands exit non-zero if verification fails, so they slot
//! into CI and scripts.

use clap::{Parser, Subcommand};
use std::process::ExitCode;
use verifiable_log_serve::{account, group, release, remote, ServeError};

#[derive(Parser)]
#[command(
    name = "pollis-verify",
    about = "Independently verify the Pollis public transparency log.",
    version
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
