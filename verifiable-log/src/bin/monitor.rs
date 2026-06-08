//! `monitor` — the offline verification CLI ("the monitor", one-shot mode).
//!
//! Reads STHs, entries, and proofs from a local JSON fixture and verifies the
//! whole bundle with no network and no database: STH signatures, equivocation,
//! entry/STH-root agreement (replaying entries through tenant invariants),
//! inclusion proofs, and consistency between STHs. Exits non-zero with a clear
//! report if anything fails.
//!
//! A `gen-example` subcommand emits a known-good fixture so the verifier is
//! easy to try (and so the test-suite has a round-trip target).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};

use verifiable_log::{
    is_equivocation, proof, verifying_key_from_hex, ConsistencyProof, Entry, InclusionProof, Sth,
    UniqueDataInvariant, VerifiableLog,
};

/// Top-level fixture / wire bundle the monitor consumes and `gen-example`
/// produces. Every section except `public_key` is optional, so a fixture can
/// exercise just the checks it cares about.
#[derive(Debug, Serialize, Deserialize)]
struct Bundle {
    /// Ed25519 log public key, hex (32 bytes).
    public_key: String,
    /// Signed Tree Heads, oldest first.
    #[serde(default)]
    sths: Vec<Sth>,
    /// Full ordered log contents. When present, replayed to confirm each STH's
    /// root and to run tenant invariants.
    #[serde(default)]
    entries: Vec<Entry>,
    /// Tenants for which the example uniqueness invariant is enforced during
    /// replay.
    #[serde(default)]
    enforce_unique: Vec<String>,
    /// Inclusion proofs to verify.
    #[serde(default)]
    inclusion: Vec<InclusionCheck>,
    /// Consistency proofs to verify (indices reference `sths`).
    #[serde(default)]
    consistency: Vec<ConsistencyCheck>,
}

#[derive(Debug, Serialize, Deserialize)]
struct InclusionCheck {
    entry: Entry,
    proof: InclusionProof,
    /// Index into `sths` whose root the proof is checked against.
    sth_index: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct ConsistencyCheck {
    old_index: usize,
    new_index: usize,
    proof: ConsistencyProof,
}

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
    },
    /// Write a known-good example fixture to a path.
    GenExample {
        /// Output path for the generated fixture.
        out: PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Verify { fixture } => match run_verify(&fixture) {
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

/// Accumulates a human-readable pass/fail report and an overall verdict.
struct Report {
    ok: bool,
}

impl Report {
    fn new() -> Self {
        Self { ok: true }
    }

    fn check(&mut self, passed: bool, label: &str) {
        if passed {
            println!("PASS  {label}");
        } else {
            println!("FAIL  {label}");
            self.ok = false;
        }
    }
}

fn run_verify(path: &PathBuf) -> Result<bool, Box<dyn std::error::Error>> {
    let raw = std::fs::read_to_string(path)?;
    let bundle: Bundle = serde_json::from_str(&raw)?;
    let verifying_key = verifying_key_from_hex(&bundle.public_key)?;

    let mut report = Report::new();

    // 1. STH signatures.
    for (i, sth) in bundle.sths.iter().enumerate() {
        report.check(
            sth.verify(&verifying_key),
            &format!("STH[{i}] signature (tree_size={})", sth.tree_size),
        );
    }

    // 2. Equivocation: any two STHs at the same size with different roots.
    for i in 0..bundle.sths.len() {
        for j in (i + 1)..bundle.sths.len() {
            let equivocates = is_equivocation(&bundle.sths[i], &bundle.sths[j]);
            report.check(
                !equivocates,
                &format!(
                    "no equivocation between STH[{i}] and STH[{j}] (tree_size={})",
                    bundle.sths[i].tree_size
                ),
            );
        }
    }

    // 3. Replay entries: run tenant invariants and confirm every STH root.
    if !bundle.entries.is_empty() {
        let mut log = VerifiableLog::new();
        for tenant in &bundle.enforce_unique {
            log.register_invariant(tenant.clone(), Box::new(UniqueDataInvariant));
        }
        let mut replay_ok = true;
        for (i, entry) in bundle.entries.iter().enumerate() {
            if let Err(e) = log.append(entry.clone()) {
                println!("FAIL  entry[{i}] rejected by tenant invariant: {e}");
                replay_ok = false;
                report.ok = false;
            }
        }
        report.check(replay_ok, "all entries satisfy tenant invariants");

        if replay_ok {
            for (i, sth) in bundle.sths.iter().enumerate() {
                let size = sth.tree_size as usize;
                let matches = match log.root_at(size) {
                    Ok(root) => sth
                        .root_bytes()
                        .map(|r| r == root)
                        .unwrap_or(false),
                    Err(_) => false,
                };
                report.check(matches, &format!("STH[{i}] root matches replayed entries"));
            }
        }
    }

    // 4. Inclusion proofs.
    for (i, check) in bundle.inclusion.iter().enumerate() {
        let sth = bundle.sths.get(check.sth_index);
        let passed = sth
            .map(|s| proof::verify_inclusion_proof(&check.entry, &check.proof, s))
            .unwrap_or(false);
        report.check(
            passed,
            &format!(
                "inclusion[{i}] leaf {} in STH[{}]",
                check.proof.leaf_index, check.sth_index
            ),
        );
    }

    // 5. Consistency proofs.
    for (i, check) in bundle.consistency.iter().enumerate() {
        let old = bundle.sths.get(check.old_index);
        let new = bundle.sths.get(check.new_index);
        let passed = match (old, new) {
            (Some(o), Some(n)) => proof::verify_consistency_proof(o, n, &check.proof),
            _ => false,
        };
        report.check(
            passed,
            &format!(
                "consistency[{i}] STH[{}] -> STH[{}]",
                check.old_index, check.new_index
            ),
        );
    }

    if report.ok {
        println!("\nOK: all checks passed");
    } else {
        println!("\nFAILED: one or more checks did not pass");
    }
    Ok(report.ok)
}

/// Build a small multi-tenant log and serialize a known-good fixture.
fn gen_example(out: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    // Deterministic key so the fixture is reproducible.
    let signing_key = SigningKey::from_bytes(&[7u8; 32]);
    let public_key = hex::encode(signing_key.verifying_key().to_bytes());

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
        sths: vec![sth_mid, sth_full],
        entries,
        enforce_unique: vec!["commits".to_string()],
        inclusion,
        consistency,
    };

    std::fs::write(out, serde_json::to_string_pretty(&bundle)?)?;
    Ok(())
}
