//! Dev-DS peer for the mobile Maestro `pins` two-client flow (#1315).
//!
//! Signs up on the dev DS (`POLLIS_DELIVERY_URL`), prints its handle, waits
//! for the phone to open a DM, sends a message and pins it (minting the DM's
//! pin key), prints `PEER_PINNED`, then waits for the phone's own pin to appear
//! (`PEER_SAW_DEVICE_PIN`) and for the phone to unpin the peer's
//! (`PEER_SAW_UNPIN`). `mobile/scripts/maestro-pins.sh` drives both sides.
//!
//!   POLLIS_DATA_DIR=$(mktemp -d) POLLIS_DELIVERY_URL=https://api-dev.pollis.com \
//!   POLLIS_DEV_ACCESS_KEY=… cargo run -p pollis-tui --example pins_peer -- <email>

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use pollis_core::commands::{dm, messages, pinned_messages};
use pollis_core::config::Config;
use pollis_core::state::AppState;

/// The dev DS's fixed OTP (`DEV_OTP`); override with `POLLIS_DEV_OTP`.
const DEFAULT_OTP: &str = "000000";
/// How long to wait for the phone at each step.
const WAIT: Duration = Duration::from_secs(600);

/// The text the peer pins; the flow looks for it on the phone.
const PEER_TEXT: &str = "peer pinned this";
/// The text the phone pins; the peer looks for it in its own list.
const DEVICE_TEXT: &str = "pin from the phone";

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let email = std::env::args().nth(1).context("usage: pins_peer <email>")?;
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

    let sent = messages::send_message(
        dm_id.clone(),
        me.id.clone(),
        PEER_TEXT.to_string(),
        None,
        None,
        Some(me.username.clone()),
        &state,
    )
    .await
    .context("send the message to pin")?;
    pinned_messages::pin_message(dm_id.clone(), sent.id.clone(), me.id.clone(), &state)
        .await
        .context("pin it")?;
    println!("PEER_PINNED dm={dm_id}");

    // The phone pins its own message, then unpins the peer's. Each is a DS
    // write the peer can only see by listing, so list until it shows.
    let started = Instant::now();
    let mut saw_device_pin = false;
    loop {
        // Ingest so the phone's messages are in the local user cache and the
        // pin key re-wrap backstop runs, as an open conversation would.
        let _ = pollis_tui::data::dm_messages(&state, &me.id, &dm_id, None).await;
        match pinned_messages::list_pinned_messages(dm_id.clone(), me.id.clone(), &state).await {
            Ok(pins) => {
                // Case-insensitive: the phone's keyboard capitalises the
                // first letter of what Maestro types.
                let device_pin = pins
                    .iter()
                    .any(|p| p.content.to_lowercase().contains(DEVICE_TEXT));
                let peer_pin = pins.iter().any(|p| p.message_id == sent.id);
                if device_pin && !saw_device_pin {
                    saw_device_pin = true;
                    println!("PEER_SAW_DEVICE_PIN");
                }
                if saw_device_pin && !peer_pin {
                    println!("PEER_SAW_UNPIN");
                    return Ok(());
                }
            }
            Err(e) => eprintln!("list_pinned_messages: {e}"),
        }
        if started.elapsed() > WAIT {
            bail!("the phone's pin / unpin never showed (saw pin: {saw_device_pin})");
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}
