//! `builder` — reads `mls_commit_log` and emits a signed monitor bundle.
//!
//! Subcommands:
//! * `build` — read the DB, append every commit, sign STHs, write the bundle.
//! * `build-binaries` — read a JSON file of `BinaryRecord`s and write a signed
//!   binaries-tree bundle (binary transparency, Phase 1). No DB.
//! * `keygen` — mint a throwaway Ed25519 keypair (hex) for dev.
//!
//! The emitted bundle is verified with `monitor verify <bundle.json>` from the
//! `verifiable-log` crate, unchanged.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use verifiable_log_builder::error::{BuilderError, Result};
use verifiable_log_builder::{
    build_account_bundle, build_binaries_bundle, build_bundle, keys, source, BinaryRecord,
    RetiredKey,
};

#[derive(Parser)]
#[command(
    name = "builder",
    about = "Build a signed verifiable-log monitor bundle from the MLS commit log."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Read `mls_commit_log`, append every commit, and write a signed bundle.
    Build {
        /// Main database source: a libSQL/Turso URL (uses `TURSO_AUTH_TOKEN`) or
        /// a local SQLite file path. Holds `account_key_log`. Falls back to
        /// `TURSO_DATABASE_URL` if omitted.
        #[arg(long)]
        db: Option<String>,

        /// Log database source for `mls_commit_log` (Goal A moves the commit log
        /// into its own DB with its own credentials). A libSQL/Turso URL (uses
        /// `LOG_DB_AUTH_TOKEN`, falling back to `TURSO_AUTH_TOKEN`) or a local
        /// SQLite file path. Resolution order: this flag, then `LOG_DB_URL`, then
        /// the main `--db` (single-DB / pre-cutover behaviour).
        #[arg(long)]
        log_db: Option<String>,

        /// Output path for the commit-log JSON bundle.
        #[arg(long)]
        out: PathBuf,

        /// Optional output path for the account-key JSON bundle. When given, the
        /// `account_key_log` table is also read and a SECOND, independent bundle
        /// (its own tree, its own domain-separated STH) is written here.
        #[arg(long)]
        account_out: Option<PathBuf>,

        /// STH timestamp, milliseconds since epoch. Supplied explicitly so the
        /// output is deterministic (never read from the system clock).
        #[arg(long)]
        timestamp: u64,

        /// STH timestamp for the account-key tree, milliseconds since epoch.
        /// Defaults to `--timestamp` when omitted. Supplied independently so an
        /// unchanged tree can be re-emitted byte-identically (reusing its already
        /// published, frozen timestamp) while the other tree advances — an STH
        /// for a given (size, root) must stay stable across republishes.
        #[arg(long)]
        account_timestamp: Option<u64>,

        /// Env var holding the 32-byte hex Ed25519 signing key.
        #[arg(long, default_value = "VLOG_SIGNING_KEY")]
        signing_key_env: String,

        /// Optional file holding the 32-byte hex signing key (used if the env
        /// var is unset).
        #[arg(long)]
        signing_key_file: Option<PathBuf>,

        /// A key inside its rotation overlap window, as
        /// `<public_key_hex>:<not_after_ms>`. Repeatable. These keys no longer
        /// sign, but are published alongside the active key so a client pinning
        /// one can still verify until `not_after` passes — which is what stops a
        /// rotation being a flag day. Omit outside a rotation.
        #[arg(long = "retired-key", value_name = "HEX:NOT_AFTER_MS")]
        retired_key: Vec<String>,
    },
    /// Read a JSON file of `BinaryRecord`s and write a signed binaries-tree
    /// bundle. Unlike `build`, the binaries tenant's source of truth is a JSON
    /// file (not the DB) in Phase 1, so this needs no database connection.
    BuildBinaries {
        /// Input path: a JSON array of `BinaryRecord`s in publish order (the
        /// records the `attest-and-log` release job emits). Its `layer`,
        /// `payload_sha256`, and tuple fields drive the `BinaryInvariant`.
        #[arg(long)]
        binaries_in: PathBuf,

        /// Output path for the `binaries-bundle.json` (STH signed under the
        /// binaries domain context).
        #[arg(long)]
        out: PathBuf,

        /// STH timestamp, milliseconds since epoch. Supplied explicitly (the tag
        /// commit timestamp) so the output is deterministic and an unchanged tree
        /// re-emits byte-identically — never read from the system clock.
        #[arg(long)]
        timestamp: u64,

        /// Env var holding the 32-byte hex Ed25519 signing key.
        #[arg(long, default_value = "VLOG_SIGNING_KEY")]
        signing_key_env: String,

        /// Optional file holding the 32-byte hex signing key (used if the env
        /// var is unset).
        #[arg(long)]
        signing_key_file: Option<PathBuf>,

        /// A key inside its rotation overlap window, as
        /// `<public_key_hex>:<not_after_ms>`. Repeatable. These keys no longer
        /// sign, but are published alongside the active key so a client pinning
        /// one can still verify until `not_after` passes — which is what stops a
        /// rotation being a flag day. Omit outside a rotation.
        #[arg(long = "retired-key", value_name = "HEX:NOT_AFTER_MS")]
        retired_key: Vec<String>,
    },
    /// Mint a fresh Ed25519 keypair (hex) for dev/throwaway use.
    Keygen,
    /// Sign a key-set statement with the OFFLINE root key (#754).
    ///
    /// This is the ceremony command. It is the only thing the root ever signs, and
    /// it is deliberately the whole job: mint the statement here, on the machine
    /// holding the root, and publish the resulting JSON. Nothing in CI needs the
    /// root, which is the entire point — a compromise of the build system cannot
    /// re-authorise a signer.
    KeySet {
        /// Env var holding the 32-byte hex ROOT signing seed.
        #[arg(long, default_value = "VLOG_ROOT_SIGNING_KEY")]
        root_key_env: String,

        /// File holding the 32-byte hex root seed (used if the env var is unset).
        #[arg(long)]
        root_key_file: Option<PathBuf>,

        /// Issue time, milliseconds since epoch. Explicit so the output is
        /// deterministic and re-mintable, never read from the system clock.
        #[arg(long)]
        issued_at: u64,

        /// When this statement stops being accepted (ms since epoch). Bounds how
        /// long a client cut off from the log keeps trusting a stale set.
        #[arg(long)]
        not_after: Option<u64>,

        /// A signer, as `<public_key_hex>:<tree,tree,...>[:<not_after_ms>]`.
        /// Repeatable. `trees` are the STH domain-separation contexts this key may
        /// sign under.
        #[arg(long = "signer", value_name = "HEX:TREES[:NOT_AFTER_MS]")]
        signer: Vec<String>,

        /// Where to write the signed statement.
        #[arg(long)]
        out: PathBuf,
    },
}

/// Parse `<public_key_hex>:<tree,tree>[:<not_after_ms>]` into a signer entry.
///
/// The tree contexts themselves contain colons (`pollis-verifiable-log:sth:v2`), so
/// this splits the key off the FRONT and the optional expiry off the BACK — and only
/// treats a trailing segment as an expiry when it is entirely digits. A naive
/// `split(':')` silently mangles every real context, which is how the first version
/// of this failed.
fn parse_signer(spec: &str) -> std::result::Result<verifiable_log::SignerEntry, String> {
    let (key_hex, rest) = spec
        .split_once(':')
        .ok_or_else(|| format!("signer must be <public_key_hex>:<trees>[:<not_after_ms>], got {spec:?}"))?;

    let key = verifiable_log::root_key_from_hex(key_hex.trim()).map_err(|e| e.to_string())?;

    let (trees_part, not_after_ms) = match rest.rsplit_once(':') {
        Some((head, tail)) if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) => (
            head,
            Some(
                tail.parse::<u64>()
                    .map_err(|_| format!("not_after_ms does not fit in u64: {tail:?}"))?,
            ),
        ),
        _ => (rest, None),
    };

    let trees: Vec<String> = trees_part
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect();
    if trees.is_empty() {
        return Err(format!("signer {spec:?} names no trees"));
    }

    Ok(verifiable_log::SignerEntry {
        key_id: verifiable_log::sth::key_id_for(&key),
        public_key: key_hex.trim().to_string(),
        trees,
        not_after_ms,
    })
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Build {
            db,
            log_db,
            out,
            account_out,
            timestamp,
            account_timestamp,
            signing_key_env,
            signing_key_file,
            retired_key,
        } => run_build(
            db,
            log_db,
            out,
            account_out,
            timestamp,
            account_timestamp.unwrap_or(timestamp),
            &signing_key_env,
            signing_key_file.as_deref(),
            &retired_key,
        ),
        Command::BuildBinaries {
            binaries_in,
            out,
            timestamp,
            signing_key_env,
            signing_key_file,
            retired_key,
        } => run_build_binaries(
            binaries_in,
            out,
            timestamp,
            &signing_key_env,
            signing_key_file.as_deref(),
            &retired_key,
        ),
        Command::Keygen => {
            run_keygen();
            Ok(())
        }
        Command::KeySet {
            root_key_env,
            root_key_file,
            issued_at,
            not_after,
            signer,
            out,
        } => run_key_set(
            &root_key_env,
            root_key_file.as_deref(),
            issued_at,
            not_after,
            &signer,
            &out,
        ),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Parse `--retired-key <public_key_hex>:<not_after_ms>` into bundle entries.
///
/// The key id is *derived* from the key rather than accepted from the operator,
/// so a typo cannot publish an id that names a different key than the bytes
/// beside it. A malformed entry is a hard error: silently dropping one would
/// publish a shorter overlap set than intended and strand clients pinning the
/// omitted key.
fn parse_retired_keys(specs: &[String]) -> Result<Vec<RetiredKey>> {
    specs
        .iter()
        .map(|spec| {
            let (hex_key, not_after) = spec.rsplit_once(':').ok_or_else(|| {
                BuilderError::SigningKey(format!(
                    "--retired-key must be <public_key_hex>:<not_after_ms>, got {spec:?}"
                ))
            })?;
            let not_after: u64 = not_after.trim().parse().map_err(|_| {
                BuilderError::SigningKey(format!("--retired-key not_after not an integer: {spec:?}"))
            })?;
            let vk = verifiable_log::verifying_key_from_hex(hex_key.trim()).map_err(|e| {
                BuilderError::SigningKey(format!("--retired-key is not a valid ML-DSA-44 key: {e}"))
            })?;
            Ok(RetiredKey {
                key_id: verifiable_log::key_id_for(&vk),
                algorithm: "ML-DSA-44".to_string(),
                public_key: hex_key.trim().to_ascii_lowercase(),
                not_after: Some(not_after),
            })
        })
        .collect()
}

#[tokio::main(flavor = "current_thread")]
// One parameter per CLI flag this subcommand accepts.
#[allow(clippy::too_many_arguments)]
async fn run_build(
    db: Option<String>,
    log_db: Option<String>,
    out: PathBuf,
    account_out: Option<PathBuf>,
    timestamp: u64,
    account_timestamp: u64,
    signing_key_env: &str,
    signing_key_file: Option<&std::path::Path>,
    retired_key: &[String],
) -> Result<()> {
    let db = db
        .or_else(|| std::env::var("TURSO_DATABASE_URL").ok())
        .ok_or(BuilderError::NoDbSource)?;

    // The commit log lives in its own DB after Goal A's cutover. Resolve it from
    // (1) --log-db, (2) LOG_DB_URL, (3) the main DB (single-DB / pre-cutover).
    let log_db = log_db
        .or_else(|| std::env::var("LOG_DB_URL").ok())
        .unwrap_or_else(|| db.clone());
    // The log DB has its own token; fall back to TURSO_AUTH_TOKEN so single-DB,
    // tests, and pre-cutover all behave exactly as before.
    let log_token = std::env::var("LOG_DB_AUTH_TOKEN")
        .or_else(|_| std::env::var("TURSO_AUTH_TOKEN"))
        .unwrap_or_default();

    // Resolve the signing key BEFORE touching the DB so a missing key fails fast.
    let signing_key = keys::load_signing_key(signing_key_env, signing_key_file)?;

    // The two tenants are INDEPENDENT: the commit log lives in the log DB and the
    // account-key directory in the main DB, on separate Merkle trees. A transient
    // empty commit log (e.g. right after Goal A's cutover) must NOT block the
    // account-key bundle, and vice versa. So we read both first, then decide.

    // Commit-log tenant (frozen contract): read from the LOG DB with its token.
    let log_conn = source::connect_with_token(&log_db, &log_token).await?;
    let rows = source::read_commit_log(&log_conn).await?;
    let commit_empty = rows.is_empty();

    // Account-key tenant (separate tree, domain-separated STH) — only when asked.
    // It stays in the MAIN DB (TURSO_AUTH_TOKEN), independent of the log DB.
    let account_rows = if account_out.is_some() {
        let conn = source::connect(&db).await?;
        Some(source::read_account_key_log(&conn).await?)
    } else {
        None
    };
    // "Absent" (not requested) counts the same as "empty" for the all-empty check.
    let account_empty = account_rows.as_ref().is_none_or(|r| r.is_empty());

    // Only hard-fail when there is nothing to publish from EITHER tenant. With at
    // least one non-empty source we go on to write a bundle for each — an empty
    // source yields a valid empty/zero-length bundle (see below), never an abort.
    if commit_empty && account_empty {
        // The DB connected fine; the commit log is just empty. Surface that
        // distinct condition (EmptyCommitLog), not the missing-source NoDbSource.
        source::ensure_non_empty(&rows)?;
    }

    // Commit-log bundle. `build_bundle` over zero rows is well-defined — it emits
    // a valid bundle with a single STH over tree size 0 and no entries — so we
    // ALWAYS write `out`. That keeps the downstream `serve generate --bundle
    // bundle.json` step working (the file always exists and parses) instead of
    // crashing on a missing file when the commit log happens to be empty.
    if commit_empty {
        eprintln!(
            "notice: commit log is empty (db connected OK) — writing an empty commit-log \
             bundle so `serve generate` still runs; the account-key tree is built independently"
        );
    }
    let retired = parse_retired_keys(retired_key)?;
    let mut bundle = build_bundle(&rows, &signing_key, timestamp)?;
    bundle.retired_keys.clone_from(&retired);
    let json = serde_json::to_string_pretty(&bundle)?;
    std::fs::write(&out, json)?;

    // Never print the raw commit_data or the auth token — only safe metadata.
    println!(
        "wrote commit-log bundle: {} commits, {} STH(s), {} inclusion proof(s) -> {}",
        rows.len(),
        bundle.sths.len(),
        bundle.inclusion.len(),
        out.display()
    );

    // Account-key bundle, written independently of the commit log's state. Like
    // the commit bundle, a zero-row account log yields a valid empty bundle, so
    // `serve generate --account-bundle ...` always has a file to read.
    if let (Some(account_out), Some(account_rows)) = (account_out, account_rows) {
        if account_rows.is_empty() {
            eprintln!(
                "notice: account-key log is empty (db connected OK) — writing an empty \
                 account-key bundle"
            );
        }
        let mut account_bundle =
            build_account_bundle(&account_rows, &signing_key, account_timestamp)?;
        account_bundle.retired_keys.clone_from(&retired);
        let account_json = serde_json::to_string_pretty(&account_bundle)?;
        std::fs::write(&account_out, account_json)?;

        println!(
            "wrote account-key bundle: {} keys, {} STH(s), {} inclusion proof(s) -> {}",
            account_rows.len(),
            account_bundle.sths.len(),
            account_bundle.inclusion.len(),
            account_out.display()
        );
    }

    println!("public_key: {}", bundle.public_key);
    Ok(())
}

/// Read `BinaryRecord`s from a JSON file and write a signed binaries bundle.
/// Synchronous — the binaries tenant reads a file, not a DB, so there is no
/// async runtime here (contrast [`run_build`]).
fn run_build_binaries(
    binaries_in: PathBuf,
    out: PathBuf,
    timestamp: u64,
    signing_key_env: &str,
    signing_key_file: Option<&std::path::Path>,
    retired_key: &[String],
) -> Result<()> {
    // Resolve the signing key BEFORE reading input so a missing key fails fast.
    let signing_key = keys::load_signing_key(signing_key_env, signing_key_file)?;

    let raw = std::fs::read_to_string(&binaries_in)?;
    let records: Vec<BinaryRecord> = serde_json::from_str(&raw)?;

    if records.is_empty() {
        eprintln!(
            "notice: binaries records file is empty — writing an empty binaries bundle so \
             `serve generate` still runs"
        );
    }

    let mut bundle = build_binaries_bundle(&records, &signing_key, timestamp)?;
    bundle.retired_keys = parse_retired_keys(retired_key)?;
    let json = serde_json::to_string_pretty(&bundle)?;
    std::fs::write(&out, json)?;

    println!(
        "wrote binaries bundle: {} artifact(s), {} STH(s), {} inclusion proof(s) -> {}",
        records.len(),
        bundle.sths.len(),
        bundle.inclusion.len(),
        out.display()
    );
    println!("public_key: {}", bundle.public_key);
    Ok(())
}

/// Sign a key-set statement with the offline root and write it out.
fn run_key_set(
    root_key_env: &str,
    root_key_file: Option<&std::path::Path>,
    issued_at: u64,
    not_after: Option<u64>,
    signer_specs: &[String],
    out: &std::path::Path,
) -> std::result::Result<(), BuilderError> {
    let root = keys::load_signing_key(root_key_env, root_key_file)?;

    let mut signers = Vec::with_capacity(signer_specs.len());
    for spec in signer_specs {
        signers.push(parse_signer(spec).map_err(BuilderError::SigningKey)?);
    }

    let statement = verifiable_log::KeySetStatement::create(&root, issued_at, not_after, signers)
        .map_err(|e| BuilderError::SigningKey(e.to_string()))?;

    // Verify what we just signed before writing it. A ceremony that emits an
    // unverifiable statement is worse than one that fails: it is discovered by
    // clients, at the moment they most need the key set to work.
    statement
        .verify(&ml_dsa::Keypair::verifying_key(&root))
        .map_err(|e| BuilderError::SigningKey(format!("refusing to write an unverifiable statement: {e}")))?;

    let json = serde_json::to_string_pretty(&statement)?;
    std::fs::write(out, format!("{json}\n"))?;

    eprintln!(
        "wrote {} — {} signer(s), issued {}, {}",
        out.display(),
        statement.signers.len(),
        issued_at,
        match not_after {
            Some(t) => format!("expires {t}"),
            None => "no expiry".to_string(),
        }
    );
    for s in &statement.signers {
        eprintln!("  {} -> {}", s.key_id, s.trees.join(", "));
    }
    Ok(())
}

fn run_keygen() {
    let g = keys::generate();
    println!("# verifiable-log signing keypair (dev/throwaway — not for prod custody)");
    println!("VLOG_SIGNING_KEY={}", g.secret_hex);
    println!("public_key={}", g.public_hex);
}

#[cfg(test)]
mod signer_spec_tests {
    use super::parse_signer;

    /// A real ML-DSA-44 public key, hex — generated here so the test needs no fixture.
    fn pubkey() -> String {
        use ml_dsa::{Keypair, MlDsa44, SigningKey};
        let sk = SigningKey::<MlDsa44>::from_seed(&[7u8; 32].into());
        hex::encode(sk.verifying_key().encode())
    }

    // The bug this pins: tree contexts contain colons, so splitting the spec on
    // every ':' mangles them. Caught live during the first ceremony run.
    #[test]
    fn a_tree_context_containing_colons_survives_parsing() {
        let s = parse_signer(&format!("{}:pollis-verifiable-log:sth:v2", pubkey())).expect("parse");
        assert_eq!(s.trees, vec!["pollis-verifiable-log:sth:v2"]);
        assert_eq!(s.not_after_ms, None);
    }

    #[test]
    fn several_colon_bearing_trees_parse() {
        let s = parse_signer(&format!(
            "{}:pollis-verifiable-log:sth:v2,pollis-verifiable-log:sth:v2:binaries",
            pubkey()
        ))
        .expect("parse");
        assert_eq!(s.trees.len(), 2);
        assert!(s.trees.contains(&"pollis-verifiable-log:sth:v2:binaries".to_string()));
    }

    #[test]
    fn a_trailing_number_is_read_as_the_expiry() {
        let s = parse_signer(&format!("{}:pollis-verifiable-log:sth:v2:1707776000000", pubkey()))
            .expect("parse");
        assert_eq!(s.trees, vec!["pollis-verifiable-log:sth:v2"]);
        assert_eq!(s.not_after_ms, Some(1_707_776_000_000));
    }

    // `:binaries` is a tree suffix, not an expiry — only an all-digit tail is.
    #[test]
    fn a_non_numeric_tail_stays_part_of_the_tree() {
        let s = parse_signer(&format!("{}:pollis-verifiable-log:sth:v2:binaries", pubkey()))
            .expect("parse");
        assert_eq!(s.trees, vec!["pollis-verifiable-log:sth:v2:binaries"]);
        assert_eq!(s.not_after_ms, None);
    }

    #[test]
    fn the_key_id_is_derived_from_the_key_not_supplied() {
        let s = parse_signer(&format!("{}:t", pubkey())).expect("parse");
        assert_eq!(s.key_id.len(), 16);
        assert_eq!(s.public_key, pubkey());
    }

    #[test]
    fn a_malformed_key_is_rejected() {
        assert!(parse_signer("not-hex:pollis-verifiable-log:sth:v2").is_err());
        assert!(parse_signer("deadbeef:pollis-verifiable-log:sth:v2").is_err());
    }

    #[test]
    fn a_spec_naming_no_trees_is_rejected() {
        assert!(parse_signer(&format!("{}:", pubkey())).is_err());
    }
}
