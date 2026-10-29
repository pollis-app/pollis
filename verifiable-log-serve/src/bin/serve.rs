//! `serve` — the serve-layer CLI for the verifiable log: generate the static
//! `/v1` tree, serve it (dev or live from Turso), and verify it remotely.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};

use verifiable_log_builder::keys;
use verifiable_log_serve::error::{Result, ServeError};
use verifiable_log_serve::{group, layout, remote, DevServer, LiveServer};

#[derive(Parser)]
#[command(
    name = "serve",
    about = "Generate, serve, and remotely verify the verifiable log's static read API."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate the immutable static artifact tree from a signed bundle.
    Generate {
        /// Path to a bundle for the FIRST commit-log tree, written directly under
        /// `/v1/...`. That tree is retired in production (#1579) and CI no longer
        /// passes this; it remains for local and self-hosted logs.
        #[arg(long, required_unless_present = "commit_log_2_bundle")]
        bundle: Option<PathBuf>,
        /// Path to the commit-log-2 bundle JSON (`builder build --commit-tree
        /// commit-log-2`), emitted under `/v1/commit-log-2/...`.
        #[arg(long)]
        commit_log_2_bundle: Option<PathBuf>,
        /// Optional signed reset statement (`builder commit-log-reset`), published
        /// at `/v1/commit-log-reset.json` after it verifies under the
        /// commit-log-2 bundle's key.
        #[arg(long, requires = "commit_log_2_bundle")]
        reset_statement: Option<PathBuf>,
        /// Optional path to the account-key bundle JSON (output of
        /// `builder build --account-out`). When given, the account-key tree is
        /// emitted under `/v1/account-keys/...` alongside the commit-log tree.
        #[arg(long)]
        account_bundle: Option<PathBuf>,
        /// Optional path to the binaries bundle JSON (output of
        /// `builder build-binaries`). When given, the binaries tree (binary
        /// transparency) is emitted under `/v1/binaries/...` alongside the
        /// commit-log and account-key trees.
        #[arg(long)]
        binaries_bundle: Option<PathBuf>,
        /// Optional path to the root-signed key-set statement (`builder key-set`).
        /// When given it is verified against `--root-key` and published at
        /// `/v1/key-set.json` (#754). Publishing one that does not verify would be
        /// worse than publishing none, so a bad statement fails the generate.
        #[arg(long, requires = "root_key")]
        key_set: Option<PathBuf>,
        /// Hex ML-DSA-44 public key of the offline root, to verify `--key-set`.
        #[arg(long)]
        root_key: Option<String>,
        /// Output directory root; the `/v1/...` tree is written under it.
        #[arg(long)]
        out: PathBuf,
    },
    /// Serve a generated directory over a local dev HTTP server.
    Serve {
        /// Directory root containing the generated `/v1/...` tree.
        #[arg(long)]
        dir: PathBuf,
        /// Port to bind on `127.0.0.1` (0 picks an ephemeral port).
        #[arg(long, default_value_t = 8787)]
        port: u16,
    },
    /// Serve a live, lazily-refreshed view read directly from Turso/libSQL. The
    /// same `/v1` artifacts and `/verify/group/<id>` endpoint as `serve`, but
    /// rebuilt in memory on demand (single-flight, at most one DB pull per TTL).
    Live {
        /// Main database source: a libSQL/Turso URL (uses `TURSO_AUTH_TOKEN`) or
        /// a local SQLite file path. Falls back to `TURSO_DATABASE_URL` if
        /// omitted. Used as the fallback log DB when `--log-db`/`LOG_DB_URL` are
        /// unset.
        #[arg(long)]
        db: Option<String>,
        /// Log database source for `mls_commit_log` (Goal A moves the commit log
        /// into its own DB). A libSQL/Turso URL (uses `LOG_DB_AUTH_TOKEN`,
        /// falling back to `TURSO_AUTH_TOKEN`) or a local SQLite file path.
        /// Resolution order: this flag, then `LOG_DB_URL`, then `--db`. The live
        /// server reads ONLY `mls_commit_log`, so it reads from the log DB.
        #[arg(long)]
        log_db: Option<String>,
        /// Port to bind on `127.0.0.1` (0 picks an ephemeral port).
        #[arg(long, default_value_t = 8787)]
        port: u16,
        /// Cache TTL in seconds: at most one DB pull per this window. `0` means
        /// rebuild on every request (useful for tests).
        #[arg(long, default_value_t = 60)]
        ttl_secs: u64,
        /// Env var holding the 32-byte hex ML-DSA-44 signing-key seed.
        #[arg(long, default_value = "VLOG_SIGNING_KEY")]
        signing_key_env: String,
        /// Optional file holding the 32-byte hex signing key (used if the env
        /// var is unset).
        #[arg(long)]
        signing_key_file: Option<PathBuf>,
    },
    /// Fetch the static API over HTTP and verify it, trusting only the pubkey.
    VerifyRemote {
        /// Base URL the static API is served at, e.g. http://127.0.0.1:8787
        base_url: String,
    },
    /// Verify a single conversation's commit chain over HTTP. Exits non-zero if
    /// the chain is not valid. Calls the same function the backend endpoint does.
    VerifyGroup {
        /// Base URL the static API is served at, e.g. http://127.0.0.1:8787
        #[arg(long)]
        base: String,
        /// Conversation / group id to verify.
        #[arg(long)]
        group: String,
        /// Print the GroupReport as JSON instead of a human report.
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Generate {
            bundle,
            commit_log_2_bundle,
            reset_statement,
            account_bundle,
            binaries_bundle,
            key_set,
            root_key,
            out,
        } => match run_generate(
            bundle.as_deref(),
            commit_log_2_bundle.as_deref(),
            reset_statement.as_deref(),
            account_bundle.as_deref(),
            binaries_bundle.as_deref(),
            key_set.as_deref(),
            root_key.as_deref(),
            &out,
        ) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(e),
        },
        Command::Serve { dir, port } => match run_serve(dir, port) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(e),
        },
        Command::Live {
            db,
            log_db,
            port,
            ttl_secs,
            signing_key_env,
            signing_key_file,
        } => match run_live(
            db,
            log_db,
            port,
            ttl_secs,
            &signing_key_env,
            signing_key_file.as_deref(),
        ) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(e),
        },
        Command::VerifyRemote { base_url } => match run_verify_remote(&base_url) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(e) => fail(e),
        },
        Command::VerifyGroup { base, group, json } => match run_verify_group(&base, &group, json) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(e) => fail(e),
        },
    }
}

// One parameter per CLI flag `generate` accepts.
#[allow(clippy::too_many_arguments)]
fn run_generate(
    bundle_path: Option<&std::path::Path>,
    commit_log_2_bundle_path: Option<&std::path::Path>,
    reset_statement_path: Option<&std::path::Path>,
    account_bundle_path: Option<&std::path::Path>,
    binaries_bundle_path: Option<&std::path::Path>,
    key_set_path: Option<&std::path::Path>,
    root_key_hex: Option<&str>,
    out: &std::path::Path,
) -> Result<()> {
    // The first commit-log tree, directly under /v1. Retired in production
    // (#1579): CI omits --bundle so none of its frozen files is rewritten.
    if let Some(bundle_path) = bundle_path {
        let bundle = layout::load_bundle(bundle_path)?;
        let manifest = layout::generate(&bundle, out)?;
        println!(
            "generated static tree: {} entries, {} STH(s), {} inclusion + {} consistency proof(s) -> {}",
            manifest.entry_count,
            manifest.sth_sizes.len(),
            manifest.inclusion.len(),
            manifest.consistency.len(),
            out.join(layout::API_VERSION).display()
        );
        println!("public_key: {}", manifest.public_key);
    }

    if let Some(path) = commit_log_2_bundle_path {
        let bundle = layout::load_bundle(path)?;
        let manifest = layout::generate_commit_log_2(&bundle, out)?;
        println!(
            "generated commit-log-2 tree: {} entries, {} STH(s), {} inclusion + {} consistency proof(s) -> {}",
            manifest.entry_count,
            manifest.sth_sizes.len(),
            manifest.inclusion.len(),
            manifest.consistency.len(),
            out.join(layout::COMMIT_LOG_2_API_PREFIX).display()
        );
        if let Some(reset_path) = reset_statement_path {
            let raw = std::fs::read_to_string(reset_path)?;
            let statement: verifiable_log::ResetStatement = serde_json::from_str(&raw)
                .map_err(|e| ServeError::BadBundle(format!("reset statement is not readable: {e}")))?;
            let mut one = std::collections::BTreeMap::new();
            layout::insert_reset_statement(
                &mut one,
                &statement,
                &bundle.key_candidates(verifiable_log_serve::bundle::now_ms()),
            )?;
            for (rel, bytes) in one {
                let dest = out.join(&rel);
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&dest, bytes)?;
                println!("published reset statement -> {}", dest.display());
            }
        }
    }

    // A key set that does not verify would make every client trust no signer at
    // all, so it fails the generate instead of being published (#754).
    if let (Some(path), Some(root_hex)) = (key_set_path, root_key_hex) {
        let raw = std::fs::read_to_string(path)?;
        let statement: verifiable_log_serve::KeySetStatement = serde_json::from_str(&raw)
            .map_err(|e| ServeError::BadBundle(format!("key set is not readable: {e}")))?;
        let mut one = std::collections::BTreeMap::new();
        layout::insert_key_set(&mut one, &statement, root_hex)?;
        for (rel, bytes) in one {
            let dest = out.join(&rel);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&dest, bytes)?;
            println!(
                "published key set -> {} ({} signer(s))",
                dest.display(),
                statement.signers.len()
            );
        }
    }
    if let Some(account_path) = account_bundle_path {
        let account_bundle = layout::load_bundle(account_path)?;
        let account_manifest = layout::generate_account(&account_bundle, out)?;
        println!(
            "generated account-keys tree: {} entries, {} STH(s), {} inclusion + {} consistency proof(s), {} account report(s) -> {}",
            account_manifest.entry_count,
            account_manifest.sth_sizes.len(),
            account_manifest.inclusion.len(),
            account_manifest.consistency.len(),
            account_manifest.users.len(),
            out.join(layout::ACCOUNT_API_PREFIX).display()
        );
    }

    if let Some(binaries_path) = binaries_bundle_path {
        let binaries_bundle = layout::load_bundle(binaries_path)?;
        let binaries_manifest = layout::generate_binaries(&binaries_bundle, out)?;
        println!(
            "generated binaries tree: {} entries, {} STH(s), {} inclusion + {} consistency proof(s), {} release report(s) -> {}",
            binaries_manifest.entry_count,
            binaries_manifest.sth_sizes.len(),
            binaries_manifest.inclusion.len(),
            binaries_manifest.consistency.len(),
            binaries_manifest.tags.len(),
            out.join(layout::BINARIES_API_PREFIX).display()
        );
    }

    Ok(())
}

fn run_serve(dir: PathBuf, port: u16) -> Result<()> {
    let server = DevServer::spawn(dir, port)?;
    println!("serving static read API at {}/v1/", server.base_url());
    println!("(dev/demo only — production is a static host serving the directory)");
    println!("press Ctrl-C to stop");
    server.block_forever();
    Ok(())
}

fn run_live(
    db: Option<String>,
    log_db: Option<String>,
    port: u16,
    ttl_secs: u64,
    signing_key_env: &str,
    signing_key_file: Option<&std::path::Path>,
) -> Result<()> {
    let db = db.or_else(|| std::env::var("TURSO_DATABASE_URL").ok());

    // The live server reads only `mls_commit_log`, so it resolves the log DB:
    // --log-db, then LOG_DB_URL, then the main --db / TURSO_DATABASE_URL.
    let log_db = log_db
        .or_else(|| std::env::var("LOG_DB_URL").ok())
        .or_else(|| db.clone())
        .ok_or_else(|| {
            ServeError::Config(
                "no database: pass --log-db/--db <url-or-path> or set LOG_DB_URL/TURSO_DATABASE_URL"
                    .into(),
            )
        })?;
    // A local file path ignores the token.
    let log_token = std::env::var("LOG_DB_AUTH_TOKEN")
        .or_else(|_| std::env::var("TURSO_AUTH_TOKEN"))
        .unwrap_or_default();

    // Load the key before binding so a missing key fails fast.
    let signing_key = keys::load_signing_key(signing_key_env, signing_key_file)?;

    let server = LiveServer::spawn(
        log_db,
        log_token,
        port,
        Duration::from_secs(ttl_secs),
        signing_key,
    )?;
    println!(
        "serving LIVE read API at {}/v1/  (lazy refresh, ttl {ttl_secs}s)",
        server.base_url()
    );
    println!("(reads from Turso on demand — at most one DB pull per TTL, no idle load)");
    println!("press Ctrl-C to stop");
    server.block_forever();
    Ok(())
}

fn run_verify_remote(base_url: &str) -> Result<bool> {
    let report = remote::verify_remote(base_url)?;
    report.print();
    Ok(report.ok)
}

fn run_verify_group(base: &str, group_id: &str, json: bool) -> Result<bool> {
    let report = group::verify_group(base, group_id)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        report.print();
    }
    Ok(report.chain_valid)
}

fn fail(e: verifiable_log_serve::ServeError) -> ExitCode {
    eprintln!("error: {e}");
    ExitCode::FAILURE
}
