//! `builder` — reads `mls_commit_log` and emits a signed monitor bundle.
//!
//! Subcommands:
//! * `append-commit-log` — append the DS rows above the last persisted `seq` to
//!   the commit log's persisted leaves, its log of record (#1579). Fails closed.
//! * `build` — sign STHs over the persisted commit-log leaves (`--commit-leaves`)
//!   or, without that flag, over every DS row; write the bundle.
//! * `build-binaries` — read a JSON file of `BinaryRecord`s and write a signed
//!   binaries-tree bundle (binary transparency, Phase 1). No DB.
//! * `keygen` — mint a throwaway ML-DSA-44 keypair (hex) for dev.
//!
//! The emitted bundle is verified with `monitor verify <bundle.json>` from the
//! `verifiable-log` crate, unchanged.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use verifiable_log_builder::error::{BuilderError, Result};
use verifiable_log_builder::{
    build_account_bundle, build_binaries_bundle, build_bundle, build_bundle_from_leaves,
    build_commit_log_2_bundle, commit_leaves, commit_log_reset, keys, source, BinaryRecord,
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

        /// Build the commit-log tree from this persisted leaves file (the
        /// output of `append-commit-log`) instead of reading `mls_commit_log`.
        /// This is how CI publishes (#1579): the DS prunes its table, so the
        /// table is never the log of record. Without it the commit log is read
        /// from the DB, which is only right for a local or test database.
        #[arg(long)]
        commit_leaves: Option<PathBuf>,

        /// Which commit-log tree to sign the commit-log bundle for. `commit-log`
        /// is the retired first tree (its STH context, for local and test use);
        /// `commit-log-2` is the live one (#1579), which CI publishes. The
        /// successor is only ever built from persisted leaves, so it requires
        /// `--commit-leaves`.
        #[arg(
            long,
            default_value = "commit-log",
            value_parser = ["commit-log", "commit-log-2"],
            requires_if("commit-log-2", "commit_leaves")
        )]
        commit_tree: String,

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
    /// Append the DS rows above the last persisted `seq` to the commit log's
    /// persisted leaves (#1579) and write the result. Refuses, never repairs, a
    /// missing, shrunk or rewritten leaves file, a file that does not reproduce
    /// `--published-sth`, and a live row below the head that was never logged
    /// or has changed.
    AppendCommitLog {
        /// Log database: a libSQL/Turso URL (uses `LOG_DB_AUTH_TOKEN`, falling
        /// back to `TURSO_AUTH_TOKEN`) or a local SQLite path. Falls back to
        /// `LOG_DB_URL`, then `TURSO_DATABASE_URL`.
        #[arg(long)]
        log_db: Option<String>,

        /// The persisted leaves file (a JSON array of entries, the shape of
        /// `/v1/entries.json`). Must exist unless a `--seed-*` flag is given.
        #[arg(long)]
        leaves: PathBuf,

        /// Where to write the appended leaves. May equal `--leaves`.
        #[arg(long)]
        out: PathBuf,

        /// The latest published commit-log STH (`/v1/commit-log-2/sth/latest.json`). The
        /// persisted leaves must reproduce it. Omit only when no commit-log
        /// tree has ever been published.
        #[arg(long)]
        published_sth: Option<PathBuf>,

        /// One-shot: `--leaves` does not exist yet, so start the log of record
        /// from the live table. Refused if `--leaves` exists.
        #[arg(long, conflicts_with = "seed_from")]
        seed_from_live: bool,

        /// One-shot: `--leaves` does not exist yet, so start the log of record
        /// from this entries file (for example leaves recovered from an earlier
        /// publish), then append the live rows above it. Refused if `--leaves`
        /// exists.
        #[arg(long)]
        seed_from: Option<PathBuf>,
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
    /// Mint the signed reset statement that retires the first commit-log tree
    /// and names its successor, `commit-log-2` (#1579). Refuses unless the
    /// committed retirement facts match the served retired heads exactly, and
    /// unless the successor has at least one persisted leaf (its genesis).
    CommitLogReset {
        /// The committed retirement facts (`commit-log-retirement.json`).
        #[arg(long)]
        retirement: PathBuf,

        /// Directory holding the retired tree's served per-size heads as
        /// `<tree_size>.json`, one for every size the facts list.
        #[arg(long)]
        retired_sths: PathBuf,

        /// The successor's persisted leaves (`internal/commit-log-2-leaves.json`).
        #[arg(long)]
        leaves: PathBuf,

        /// Issue time, milliseconds since epoch. Explicit, never the clock.
        #[arg(long)]
        issued_at: u64,

        /// Where to write the signed statement.
        #[arg(long)]
        out: PathBuf,

        /// Env var holding the 32-byte hex signing key.
        #[arg(long, default_value = "VLOG_SIGNING_KEY")]
        signing_key_env: String,

        /// Optional file holding the 32-byte hex signing key.
        #[arg(long)]
        signing_key_file: Option<PathBuf>,
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
        Command::AppendCommitLog {
            log_db,
            leaves,
            out,
            published_sth,
            seed_from_live,
            seed_from,
        } => run_append_commit_log(
            log_db,
            &leaves,
            &out,
            published_sth.as_deref(),
            seed_from_live,
            seed_from.as_deref(),
        ),
        Command::CommitLogReset {
            retirement,
            retired_sths,
            leaves,
            issued_at,
            out,
            signing_key_env,
            signing_key_file,
        } => run_commit_log_reset(
            &retirement,
            &retired_sths,
            &leaves,
            issued_at,
            &out,
            &signing_key_env,
            signing_key_file.as_deref(),
        ),
        Command::Build {
            db,
            log_db,
            commit_leaves,
            commit_tree,
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
            commit_leaves,
            commit_tree == "commit-log-2",
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
    commit_leaves: Option<PathBuf>,
    commit_log_2: bool,
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

    // Commit-log tenant (frozen contract). From the persisted log of record when
    // given (#1579); otherwise from the LOG DB with its token.
    let commit_source = match &commit_leaves {
        Some(path) => CommitSource::Leaves(commit_leaves::load(path)?.ok_or_else(|| {
            BuilderError::PersistedLeaves(format!(
                "--commit-leaves {} does not exist",
                path.display()
            ))
        })?),
        None => {
            let log_conn = source::connect_with_token(&log_db, &log_token).await?;
            CommitSource::Rows(source::read_commit_log(&log_conn).await?)
        }
    };
    let commit_count = match &commit_source {
        CommitSource::Leaves(leaves) => leaves.len(),
        CommitSource::Rows(rows) => rows.len(),
    };
    let commit_empty = commit_count == 0;

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
        return Err(BuilderError::EmptyCommitLog);
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
    let mut bundle = match &commit_source {
        CommitSource::Leaves(leaves) if commit_log_2 => {
            build_commit_log_2_bundle(leaves, &signing_key, timestamp)?
        }
        CommitSource::Leaves(leaves) => build_bundle_from_leaves(leaves, &signing_key, timestamp)?,
        CommitSource::Rows(rows) => build_bundle(rows, &signing_key, timestamp)?,
    };
    bundle.retired_keys.clone_from(&retired);
    let json = serde_json::to_string_pretty(&bundle)?;
    std::fs::write(&out, json)?;

    // Never print the raw commit_data or the auth token — only safe metadata.
    println!(
        "wrote commit-log bundle: {} commits, {} STH(s), {} inclusion proof(s) -> {}",
        commit_count,
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

/// Where `build` takes the commit-log leaves from.
enum CommitSource {
    /// The persisted log of record (#1579), which is what CI publishes.
    Leaves(Vec<verifiable_log::Entry>),
    /// Every DS row. Right only for a local or test database.
    Rows(Vec<source::CommitRow>),
}

/// Append the live rows above the persisted head and write the new leaves.
#[tokio::main(flavor = "current_thread")]
async fn run_append_commit_log(
    log_db: Option<String>,
    leaves_path: &std::path::Path,
    out: &std::path::Path,
    published_sth: Option<&std::path::Path>,
    seed_from_live: bool,
    seed_from: Option<&std::path::Path>,
) -> Result<()> {
    let seeding = seed_from_live || seed_from.is_some();
    let persisted = match (commit_leaves::load(leaves_path)?, seeding) {
        (Some(_), true) => {
            return Err(BuilderError::PersistedLeaves(format!(
                "{} already exists: a seed flag only starts a log of record, it never replaces one",
                leaves_path.display()
            )));
        }
        (Some(leaves), false) => leaves,
        (None, false) => {
            return Err(BuilderError::PersistedLeaves(format!(
                "{} does not exist. It is the commit log's log of record, so its absence is never \
                 read as empty; seed it once with --seed-from-live or --seed-from <entries.json>",
                leaves_path.display()
            )));
        }
        (None, true) => match seed_from {
            Some(path) => commit_leaves::load(path)?.ok_or_else(|| {
                BuilderError::PersistedLeaves(format!(
                    "--seed-from {} does not exist",
                    path.display()
                ))
            })?,
            None => Vec::new(),
        },
    };

    let published = match published_sth {
        Some(path) => {
            let raw = std::fs::read_to_string(path)?;
            Some(serde_json::from_str::<verifiable_log::Sth>(&raw)?)
        }
        None => None,
    };

    let log_db = log_db
        .or_else(|| std::env::var("LOG_DB_URL").ok())
        .or_else(|| std::env::var("TURSO_DATABASE_URL").ok())
        .ok_or(BuilderError::NoDbSource)?;
    let log_token = std::env::var("LOG_DB_AUTH_TOKEN")
        .or_else(|_| std::env::var("TURSO_AUTH_TOKEN"))
        .unwrap_or_default();
    let conn = source::connect_with_token(&log_db, &log_token).await?;
    let live = source::read_commit_log(&conn).await?;

    let outcome = commit_leaves::append_rows(&persisted, &live, published.as_ref())?;
    std::fs::write(out, serde_json::to_string(&outcome.leaves)?)?;

    // Counts only: never a row, an id or the token.
    println!(
        "commit-log leaves: {} persisted, {} live row(s), {} appended, {} total -> {}",
        persisted.len(),
        live.len(),
        outcome.appended,
        outcome.leaves.len(),
        out.display()
    );
    Ok(())
}

/// Check the committed retirement facts against the served retired heads, then
/// sign the reset statement (#1579).
fn run_commit_log_reset(
    retirement: &std::path::Path,
    retired_sths: &std::path::Path,
    leaves_path: &std::path::Path,
    issued_at: u64,
    out: &std::path::Path,
    signing_key_env: &str,
    signing_key_file: Option<&std::path::Path>,
) -> Result<()> {
    let signing_key = keys::load_signing_key(signing_key_env, signing_key_file)?;
    let facts: commit_log_reset::RetirementFacts =
        serde_json::from_str(&std::fs::read_to_string(retirement)?)?;

    let r = &facts.retired;
    let mut sizes: Vec<u64> = r.chain_sizes.clone();
    sizes.extend(r.forks.iter().chain(&r.superseded).map(|h| h.tree_size));
    sizes.push(r.last_consistent.tree_size);
    sizes.sort_unstable();
    sizes.dedup();
    let mut served = std::collections::BTreeMap::new();
    for size in sizes {
        let path = retired_sths.join(format!("{size}.json"));
        let raw = std::fs::read_to_string(&path).map_err(|e| {
            BuilderError::Reset(format!("read the served retired head {}: {e}", path.display()))
        })?;
        served.insert(size, serde_json::from_str::<verifiable_log::Sth>(&raw)?);
    }

    // The retired heads were signed by this same key; it is the only key the
    // statement can describe them under.
    let vk = ml_dsa::Keypair::verifying_key(&signing_key);
    let candidates = vec![(verifiable_log::key_id_for(&vk), vk)];
    commit_log_reset::check_against_served(&facts, &served, &candidates)?;

    let leaves = commit_leaves::load(leaves_path)?.ok_or_else(|| {
        BuilderError::Reset(format!("{} does not exist", leaves_path.display()))
    })?;
    let body = commit_log_reset::build_body(&facts, &leaves, issued_at)?;
    let statement = verifiable_log::ResetStatement::create(&signing_key, body)?;
    statement.verify_any(&candidates)?;
    std::fs::write(out, format!("{}\n", serde_json::to_string_pretty(&statement)?))?;
    println!(
        "wrote commit-log reset statement: retired chain to size {}, {} fork(s), {} superseded head(s), successor genesis {} -> {}",
        r.last_consistent.tree_size,
        r.forks.len(),
        r.superseded.len(),
        statement.statement.successor.genesis_root_hash,
        out.display()
    );
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
