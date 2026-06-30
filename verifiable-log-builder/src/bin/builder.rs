//! `builder` — reads `mls_commit_log` and emits a signed monitor bundle.
//!
//! Subcommands:
//! * `build` — read the DB, append every commit, sign STHs, write the bundle.
//! * `keygen` — mint a throwaway Ed25519 keypair (hex) for dev.
//!
//! The emitted bundle is verified with `monitor verify <bundle.json>` from the
//! `verifiable-log` crate, unchanged.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use verifiable_log_builder::error::{BuilderError, Result};
use verifiable_log_builder::{build_account_bundle, build_bundle, keys, source};

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
    },
    /// Mint a fresh Ed25519 keypair (hex) for dev/throwaway use.
    Keygen,
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
        } => run_build(
            db,
            log_db,
            out,
            account_out,
            timestamp,
            account_timestamp.unwrap_or(timestamp),
            &signing_key_env,
            signing_key_file.as_deref(),
        ),
        Command::Keygen => {
            run_keygen();
            Ok(())
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn run_build(
    db: Option<String>,
    log_db: Option<String>,
    out: PathBuf,
    account_out: Option<PathBuf>,
    timestamp: u64,
    account_timestamp: u64,
    signing_key_env: &str,
    signing_key_file: Option<&std::path::Path>,
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
    let account_empty = account_rows.as_ref().map_or(true, |r| r.is_empty());

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
    let bundle = build_bundle(&rows, &signing_key, timestamp)?;
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
        let account_bundle = build_account_bundle(&account_rows, &signing_key, account_timestamp)?;
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

fn run_keygen() {
    let g = keys::generate();
    println!("# verifiable-log signing keypair (dev/throwaway — not for prod custody)");
    println!("VLOG_SIGNING_KEY={}", g.secret_hex);
    println!("public_key={}", g.public_hex);
}
