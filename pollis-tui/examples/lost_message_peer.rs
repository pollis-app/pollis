//! Dev-DS peer for the mobile Maestro `lost-message` flow (#1312).
//!
//! Signs up on the dev DS (`POLLIS_DELIVERY_URL`), prints its handle, waits
//! for the phone to open a DM with it, then sends four messages: one ordinary,
//! two whose ciphertext is corrupted before it leaves
//! (`test_hooks::corrupt_next_send`), and one ordinary. The flow asserts one
//! collapsed placeholder sits between the other two.
//!
//! Examples build against pollis-tui's dev-dependencies, the only place
//! pollis-core carries `test-harness`.
//!
//!   POLLIS_DATA_DIR=$(mktemp -d) POLLIS_DELIVERY_URL=https://api-dev.pollis.com \
//!   POLLIS_DEV_ACCESS_KEY=… cargo run -p pollis-tui --example lost_message_peer -- <email>

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use pollis_core::commands::{dm, messages};
use pollis_core::config::Config;
use pollis_core::state::AppState;

/// The dev DS's fixed OTP (`DEV_OTP`); override with `POLLIS_DEV_OTP`.
const DEFAULT_OTP: &str = "000000";
/// How long to wait for the phone to open the DM.
const WAIT_FOR_DM: Duration = Duration::from_secs(300);

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let email = std::env::args().nth(1).context("usage: lost_message_peer <email>")?;
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
        if started.elapsed() > WAIT_FOR_DM {
            bail!("no DM request arrived within {WAIT_FOR_DM:?}");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    };
    // Join the DM's MLS group before sending into it.
    pollis_tui::data::dm_messages(&state, &me.id, &dm_id, None).await?;

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
    send("before the lost message").await?;
    // Two in a row: the phone collapses a run into one row.
    messages::test_hooks::corrupt_next_send();
    send("this message is lost in transit").await?;
    messages::test_hooks::corrupt_next_send();
    send("so is this one").await?;
    send("after the lost message").await?;
    println!("PEER_DONE dm={dm_id}");
    Ok(())
}
