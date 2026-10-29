//! Dev-DS peer for the mobile Maestro `unread` flow (#1422).
//!
//! Signs up on the dev DS (`POLLIS_DELIVERY_URL`), prints its handle, waits
//! for the phone to open a DM with it, accepts and prints `PEER_READY`. It then
//! sends in two batches, each released by the script touching a file in
//! `UNREAD_PEER_SIGNAL_DIR`: `go-1` while the phone's app is backgrounded,
//! `go-2` while it is closed.
//!
//!   POLLIS_DATA_DIR=$(mktemp -d) POLLIS_DELIVERY_URL=https://api-dev.pollis.com \
//!   UNREAD_PEER_SIGNAL_DIR=$(mktemp -d) POLLIS_DEV_ACCESS_KEY=… \
//!   cargo run -p pollis-tui --example unread_peer -- <email>

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use pollis_core::commands::{dm, messages};
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
    let email = std::env::args().nth(1).context("usage: unread_peer <email>")?;
    let signals: PathBuf = std::env::var("UNREAD_PEER_SIGNAL_DIR")
        .context("UNREAD_PEER_SIGNAL_DIR is required")?
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
    // Join the DM's MLS group before sending into it.
    pollis_tui::data::dm_messages(&state, &me.id, &dm_id, None).await?;
    println!("PEER_READY dm={dm_id}");

    let send = |text: &'static str| {
        let state = state.clone();
        let (id, name) = (me.id.clone(), me.username.clone());
        let dm_id = dm_id.clone();
        async move {
            messages::send_message(dm_id, id, text.to_string(), None, None, Some(name), &state)
                .await
                .with_context(|| format!("send {text:?}"))
        }
    };

    // The phone's app is in the background.
    wait_for_signal(&signals, "go-1").await?;
    send("sent while you were away").await?;
    send("and another one").await?;
    println!("PEER_SENT_1");

    // The phone's app is closed.
    wait_for_signal(&signals, "go-2").await?;
    send("sent while the app was closed").await?;
    println!("PEER_SENT_2");
    Ok(())
}
