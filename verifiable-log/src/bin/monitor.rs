//! `monitor` — the offline verification CLI ("the monitor", one-shot mode).
//!
//! Reads STHs, entries, and proofs from a local JSON fixture and verifies the
//! whole bundle with no network and no database: STH signatures, equivocation,
//! entry/STH-root agreement (replaying entries through tenant invariants),
//! inclusion proofs, and consistency between STHs. Exits non-zero with a clear
//! report if anything fails.
//!
//! The bundle shape and the check loop both live in the library
//! ([`verifiable_log::bundle`], [`verifiable_log::monitor`]) so this binary is a
//! thin argument-parsing shell over the same code the builder's gate suites
//! verify against — the three used to be three separate transcriptions.
//!
//! A `gen-example` subcommand emits a known-good fixture so the verifier is
//! easy to try (and so the test-suite has a round-trip target).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use ml_dsa::Keypair;
use verifiable_log::SigningKey;

use verifiable_log::bundle::{Bundle, ConsistencyCheck, InclusionCheck};
use verifiable_log::monitor::{unique_invariants, verify_bundle, VerifyOptions};
use verifiable_log::{
    sth_context_for_tree, Entry, Sth, UniqueDataInvariant, VerifiableLog, STH_CONTEXTS,
};

#[derive(Parser)]
#[command(
    name = "monitor",
    about = "Offline verifier for the verifiable append-only log."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Verify a fixture bundle; exits non-zero if any check fails.
    Verify {
        /// Path to the JSON fixture.
        fixture: PathBuf,
        /// Which tree the bundle belongs to. Selects the STH domain-separation
        /// context: one key signs all three trees, and a head minted for one
        /// tree deliberately does not verify under another's context, so a
        /// bundle checked under the wrong tree reports FAIL for an honest log.
        /// There is no auto-detection — the bundle does not name its own tree,
        /// and guessing would turn "which tree?" into an answer the log gets to
        /// choose.
        #[arg(long, default_value = "commit-log", value_parser = tree_names())]
        tree: String,
        /// Wall-clock milliseconds used to expire rotation-overlap keys.
        /// Defaults to the system clock; pass it explicitly for a reproducible
        /// verdict (tests, or checking what a bundle looked like at a date).
        #[arg(long)]
        now_ms: Option<u64>,
    },
    /// Write a known-good example fixture to a path.
    GenExample {
        /// Output path for the generated fixture.
        out: PathBuf,
    },
}

/// The accepted `--tree` values, derived from the one context table so a fourth
/// tree cannot be added to the library and missed here.
fn tree_names() -> clap::builder::PossibleValuesParser {
    clap::builder::PossibleValuesParser::new(STH_CONTEXTS.iter().map(|(n, _)| *n))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Verify {
            fixture,
            tree,
            now_ms,
        } => match run_verify(&fixture, &tree, now_ms) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        Command::GenExample { out } => match gen_example(&out) {
            Ok(()) => {
                println!("wrote example fixture to {}", out.display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
    }
}

/// Wall-clock milliseconds. A verifier is allowed a clock; the log core is not.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn run_verify(
    path: &PathBuf,
    tree: &str,
    now_override: Option<u64>,
) -> Result<bool, Box<dyn std::error::Error>> {
    let raw = std::fs::read_to_string(path)?;
    let bundle: Bundle = serde_json::from_str(&raw)?;
    let context =
        sth_context_for_tree(tree).ok_or_else(|| format!("unknown tree `{tree}`"))?;

    let opts = VerifyOptions {
        context,
        now_ms: now_override.unwrap_or_else(now_ms),
    };
    println!("verifying against tree `{tree}`");

    let ok = verify_bundle(
        &bundle,
        &opts,
        unique_invariants(&bundle.enforce_unique),
        &mut |passed, label| {
            if passed {
                println!("PASS  {label}");
            } else {
                println!("FAIL  {label}");
            }
        },
    );

    if ok {
        println!("\nOK: all checks passed");
    } else {
        println!("\nFAILED: one or more checks did not pass");
    }
    Ok(ok)
}

/// Build a small multi-tenant log and serialize a known-good fixture.
fn gen_example(out: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    // Deterministic key so the fixture is reproducible.
    let signing_key = SigningKey::from_seed(&[7u8; 32].into());
    let public_key = hex::encode(signing_key.verifying_key().encode());

    let mut log = VerifiableLog::new();
    log.register_invariant("commits", Box::new(UniqueDataInvariant));

    let entries = vec![
        Entry::new("commits", b"group-a/epoch-0".to_vec()),
        Entry::new("accounts", b"alice/key-v1".to_vec()),
        Entry::new("commits", b"group-a/epoch-1".to_vec()),
        Entry::new("accounts", b"bob/key-v1".to_vec()),
        Entry::new("commits", b"group-b/epoch-0".to_vec()),
    ];
    for entry in &entries {
        log.append(entry.clone())?;
    }

    // An STH after the third append and one over the full log.
    let first_size = 3usize;
    let sth_mid = {
        // Re-derive the size-3 root via root_at, sign at a fixed timestamp.
        let root = log.root_at(first_size)?;
        Sth::create(&signing_key, first_size as u64, root, 1_700_000_000_000)
    };
    let sth_full = log.signed_tree_head(&signing_key, 1_700_000_500_000);

    let inclusion = vec![InclusionCheck {
        entry: entries[1].clone(),
        proof: log.inclusion_proof(1)?,
        sth_index: 1,
    }];

    let consistency = vec![ConsistencyCheck {
        old_index: 0,
        new_index: 1,
        proof: log.consistency_proof(first_size, log.size())?,
    }];

    let bundle = Bundle {
        public_key,
        retired_keys: Vec::new(),
        sths: vec![sth_mid, sth_full],
        entries,
        enforce_unique: vec!["commits".to_string()],
        inclusion,
        consistency,
    };

    std::fs::write(out, serde_json::to_string_pretty(&bundle)?)?;
    Ok(())
}
