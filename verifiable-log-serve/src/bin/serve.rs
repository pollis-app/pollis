//! `serve` — the serve-layer CLI for the verifiable log.
//!
//! Subcommands:
//! * `generate` — turn a signed bundle into the immutable `/v1/...` static tree.
//! * `serve` — run a local dev HTTP server over a generated tree (testing/demo
//!   only; the real deployment is to drop the directory on a static host).
//! * `live` — serve a live, lazily-refreshed view read straight from Turso: the
//!   same `/v1` surface and per-group endpoint as `serve`, but rebuilt in memory
//!   on demand so new commits appear within the TTL with no idle DB load.
//! * `verify-remote` — fetch the static API over HTTP and verify the log,
//!   trusting only the published public key.
//! * `verify-group` — fetch the static API and verify ONE conversation's commit
//!   chain. This calls the exact same `verify_group` the backend HTTP endpoint
//!   does, so the CLI and the server can never report different verdicts.

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
        /// Path to the bundle JSON (output of `builder build`).
        #[arg(long)]
        bundle: PathBuf,
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
        /// Database source: a libSQL/Turso URL (uses `TURSO_AUTH_TOKEN`) or a
        /// local SQLite file path. Falls back to `TURSO_DATABASE_URL` if omitted.
        #[arg(long)]
        db: Option<String>,
        /// Port to bind on `127.0.0.1` (0 picks an ephemeral port).
        #[arg(long, default_value_t = 8787)]
        port: u16,
        /// Cache TTL in seconds: at most one DB pull per this window. `0` means
        /// rebuild on every request (useful for tests).
        #[arg(long, default_value_t = 60)]
        ttl_secs: u64,
        /// Env var holding the 32-byte hex Ed25519 signing key.
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
        Command::Generate { bundle, out } => match run_generate(&bundle, &out) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(e),
        },
        Command::Serve { dir, port } => match run_serve(dir, port) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(e),
        },
        Command::Live {
            db,
            port,
            ttl_secs,
            signing_key_env,
            signing_key_file,
        } => match run_live(db, port, ttl_secs, &signing_key_env, signing_key_file.as_deref()) {
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

fn run_generate(bundle_path: &PathBuf, out: &PathBuf) -> Result<()> {
    let bundle = layout::load_bundle(bundle_path)?;
    let manifest = layout::generate(&bundle, out)?;
    println!(
        "generated static tree: {} entries, {} STH(s), {} inclusion + {} consistency proof(s), {} group report(s) -> {}",
        manifest.entry_count,
        manifest.sth_sizes.len(),
        manifest.inclusion.len(),
        manifest.consistency.len(),
        manifest.conversations.len(),
        out.join(layout::API_VERSION).display()
    );
    println!("public_key: {}", manifest.public_key);
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
    port: u16,
    ttl_secs: u64,
    signing_key_env: &str,
    signing_key_file: Option<&std::path::Path>,
) -> Result<()> {
    let db = db
        .or_else(|| std::env::var("TURSO_DATABASE_URL").ok())
        .ok_or_else(|| {
            ServeError::Config("no database: pass --db <url-or-path> or set TURSO_DATABASE_URL".into())
        })?;

    // Resolve the signing key BEFORE binding so a missing key fails fast — the
    // live server refuses to start without one (same loader as the builder).
    let signing_key = keys::load_signing_key(signing_key_env, signing_key_file)?;

    let server = LiveServer::spawn(db, port, Duration::from_secs(ttl_secs), signing_key)?;
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
