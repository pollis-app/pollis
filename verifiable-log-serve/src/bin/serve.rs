//! `serve` — the serve-layer CLI for the verifiable log.
//!
//! Subcommands:
//! * `generate` — turn a signed bundle into the immutable `/v1/...` static tree.
//! * `serve` — run a local dev HTTP server over a generated tree (testing/demo
//!   only; the real deployment is to drop the directory on a static host).
//! * `verify-remote` — fetch the static API over HTTP and verify the log,
//!   trusting only the published public key.
//! * `verify-group` — fetch the static API and verify ONE conversation's commit
//!   chain. This calls the exact same `verify_group` the backend HTTP endpoint
//!   does, so the CLI and the server can never report different verdicts.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use verifiable_log_serve::error::Result;
use verifiable_log_serve::{group, layout, remote, DevServer};

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
        "generated static tree: {} entries, {} STH(s), {} inclusion + {} consistency proof(s) -> {}",
        manifest.entry_count,
        manifest.sth_sizes.len(),
        manifest.inclusion.len(),
        manifest.consistency.len(),
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
