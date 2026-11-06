//! Dev-DS peer for the mobile Maestro `dm-request-push` flow (#1734).
//!
//! The stranger who sends the phone a message request. Signs up a fresh account on the DS named
//! by `POLLIS_DELIVERY_URL` (the dev DS — it fixes the OTP), prints its handle, waits until the
//! phone's account exists, then waits for the script's `go-request` signal (a file in
//! `DM_REQUEST_PEER_SIGNAL_DIR`) before opening a DM with it, which the phone sees as a request,
//! and prints `PEER_REQUESTED`. `go-message-1` and `go-message-2` each release one message into
//! the still-pending request (`PEER_SENT_1`, `PEER_SENT_2`), so the script can check the phone is
//! pushed for the request once, its first message once more, and the second message not at all.
//! With `DM_REQUEST_PEER_MESSAGES=0` it sends nothing and exits after the request.
//!
//! `mobile/scripts/maestro-dm-request-push.sh` drives it. Not a shipping binary: examples build
//! against pollis-tui's dev-dependencies.
//!
//!   POLLIS_DATA_DIR=$(mktemp -d) POLLIS_DELIVERY_URL=https://api-dev.pollis.com \
//!   DM_REQUEST_PEER_SIGNAL_DIR=$(mktemp -d) POLLIS_DEV_ACCESS_KEY=… \
//!   cargo run -p pollis-tui --example dm_request_peer -- <email> <phone-email>

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use pollis_core::commands::{dm, messages, user};
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
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [email, phone] = args.as_slice() else {
        bail!("usage: dm_request_peer <email> <phone-email>");
    };
    let signals: PathBuf = std::env::var("DM_REQUEST_PEER_SIGNAL_DIR")
        .context("DM_REQUEST_PEER_SIGNAL_DIR is required")?
        .into();
    let otp = std::env::var("POLLIS_DEV_OTP").unwrap_or_else(|_| DEFAULT_OTP.to_string());
    let config = Config::from_env().context("POLLIS_DELIVERY_URL (the dev DS) is required")?;
    let ds = config.pollis_delivery_url.clone().unwrap_or_default();
    if !ds.contains("api-dev") && !ds.contains("localhost") {
        bail!("refusing to run against {ds:?} — dev DS only");
    }
    let state = Arc::new(AppState::new(config).await.context("AppState")?);
    pollis_tui::auth::request_otp(&state, email).await?;
    let me = pollis_tui::auth::verify_otp(&state, email, &otp).await?;
    pollis_tui::auth::set_pin_and_init(&state, &me.id, "0000").await?;
    println!("PEER_HANDLE={}", me.username);

    let started = Instant::now();
    let phone_id = loop {
        match user::search_user_by_username(phone.to_string(), &state).await {
            Ok(Some(p)) => break p.id,
            Ok(None) => {}
            Err(e) => eprintln!("the phone's account: {e}"),
        }
        if started.elapsed() > WAIT {
            bail!("the phone's account did not appear within {WAIT:?}");
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    };
    println!("PEER_FOUND_PHONE");

    wait_for_signal(&signals, "go-request").await?;
    let dm = dm::create_dm_channel(me.id.clone(), vec![phone_id], &state)
        .await
        .context("open a DM with the phone")?;
    let dm_id = dm.id.clone();
    println!("PEER_REQUESTED dm={dm_id}");

    // `DM_REQUEST_PEER_MESSAGES=0`: a requester who never writes (the script's accept check).
    let messages = std::env::var("DM_REQUEST_PEER_MESSAGES").map_or(2, |v| v.parse().unwrap_or(2));
    for (n, text) in [(1, "hello from a stranger"), (2, "are you there?")].into_iter().take(messages) {
        wait_for_signal(&signals, &format!("go-message-{n}")).await?;
        messages::send_message(
            dm_id.clone(),
            me.id.clone(),
            text.to_string(),
            None,
            None,
            Some(me.username.clone()),
            &state,
        )
        .await
        .with_context(|| format!("send message {n}"))?;
        println!("PEER_SENT_{n}");
    }
    // Pushes are sent on spawned tasks on the DS; let our own wake-ups leave before exiting.
    tokio::time::sleep(Duration::from_secs(5)).await;
    Ok(())
}
