//! Dev-DS peer for the mobile Maestro `key-change` flow (#1605).
//!
//! Signs up a fresh account on the DS named by `POLLIS_DELIVERY_URL` (the dev
//! DS, which fixes the OTP), prints its handle, waits for the phone to open a
//! DM with it, accepts, sends one message and prints `PEER_READY dm=<id>`.
//! When the script touches `go` in `KEY_CHANGE_PEER_SIGNAL_DIR` (after the
//! phone verified this peer's safety number), it rotates its account identity
//! in place (`reset_identity`: a new account key under the same user id, the
//! DM membership kept), which is exactly a safety-number change for everyone
//! who pinned the old key, and prints `PEER_RESET`. The phone's next ingest of
//! the DM re-pins the key and must show the change as a line in the timeline.
//!
//! Not a shipping binary: examples build against pollis-tui's dev-dependencies.
//!
//!   POLLIS_DATA_DIR=$(mktemp -d) POLLIS_DELIVERY_URL=https://api-dev.pollis.com \
//!   KEY_CHANGE_PEER_SIGNAL_DIR=$(mktemp -d) POLLIS_DEV_ACCESS_KEY=… \
//!   cargo run -p pollis-tui --example key_change_peer -- <email>

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use pollis_core::commands::{account_identity, dm, messages};
use pollis_core::config::Config;
use pollis_core::state::AppState;

/// The dev DS's fixed OTP (`DEV_OTP`); override with `POLLIS_DEV_OTP`.
const DEFAULT_OTP: &str = "000000";
/// How long to wait for the phone, or the script, at each step.
const WAIT: Duration = Duration::from_secs(300);

async fn wait_for_signal(dir: &Path, name: &str) -> Result<()> {
    let started = Instant::now();
    let path = dir.join(name);
    while !path.exists() {
        if started.elapsed() > WAIT {
            bail!("no {name} signal within {WAIT:?}");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Ok(())
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let email = std::env::args().nth(1).context("usage: key_change_peer <email>")?;
    let signals: PathBuf = std::env::var("KEY_CHANGE_PEER_SIGNAL_DIR")
        .context("KEY_CHANGE_PEER_SIGNAL_DIR is required")?
        .into();
    let otp = std::env::var("POLLIS_DEV_OTP").unwrap_or_else(|_| DEFAULT_OTP.to_string());
    let config = Config::from_env().context("POLLIS_DELIVERY_URL (the dev DS) is required")?;
    let ds = config.pollis_delivery_url.clone().unwrap_or_default();
    if !ds.contains("api-dev") && !ds.contains("localhost") {
        bail!("refusing to run against {ds:?} — dev DS only");
    }
    let state = Arc::new(AppState::new(config).await.context("AppState")?);

    pollis_tui::auth::request_otp(&state, &email).await?;
    let me = pollis_tui::auth::verify_otp(&state, &email, &otp).await?;
    pollis_tui::auth::set_pin_and_init(&state, &me.id, "0000").await?;
    println!("PEER_HANDLE={}", me.username);

    let started = Instant::now();
    let dm_id = loop {
        if let Some(req) = dm::list_dm_requests(me.id.clone(), &state).await?.into_iter().next() {
            dm::accept_dm_request(req.id.clone(), me.id.clone(), &state).await?;
            break req.id;
        }
        if started.elapsed() > WAIT {
            bail!("no DM request arrived within {WAIT:?}");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    };
    pollis_tui::data::dm_messages(&state, &me.id, &dm_id, None).await?;
    messages::send_message(
        dm_id.clone(),
        me.id.clone(),
        "before the change".to_string(),
        None,
        None,
        Some(me.username.clone()),
        &state,
    )
    .await
    .context("send before the change")?;
    println!("PEER_READY dm={dm_id}");

    wait_for_signal(&signals, "go").await?;
    account_identity::reset_identity(&state, &me.id)
        .await
        .context("rotate the account identity")?;
    println!("PEER_RESET");
    // Wake-ups are sent on spawned tasks; let them leave before exiting.
    tokio::time::sleep(Duration::from_secs(5)).await;
    Ok(())
}
