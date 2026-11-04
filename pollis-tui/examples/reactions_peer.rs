//! Dev-DS peer for the mobile Maestro `reaction-breakdown` two-client flow (#1684).
//!
//! Two accounts react to one message, so the phone's "who reacted" sheet has
//! someone other than its own user to name. This peer signs up a fresh account
//! on the DS named by `POLLIS_DELIVERY_URL` (the dev DS — it fixes the OTP),
//! prints its handle, waits for the phone under test to open a DM with it,
//! sends a message and reacts to it with 👍 and 🎉, prints `PEER_REACTED`, then
//! waits for the phone's own 👍 on it (`PEER_SAW_DEVICE_REACTION`).
//! `mobile/scripts/maestro-reaction-breakdown.sh` drives both sides.
//!
//! Not a shipping binary: examples build against pollis-tui's
//! dev-dependencies.
//!
//!   POLLIS_DATA_DIR=$(mktemp -d) POLLIS_DELIVERY_URL=https://api-dev.pollis.com \
//!   POLLIS_DEV_ACCESS_KEY=… cargo run -p pollis-tui --example reactions_peer -- <email>

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use pollis_core::commands::{dm, messages};
use pollis_core::config::Config;
use pollis_core::state::AppState;

/// The dev DS's fixed OTP (`DEV_OTP`); override with `POLLIS_DEV_OTP`.
const DEFAULT_OTP: &str = "000000";
/// How long to wait for the phone at each step.
const WAIT: Duration = Duration::from_secs(600);

/// The text the peer reacts to; the flow looks for it on the phone.
const PEER_TEXT: &str = "react to this one";

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let email = std::env::args().nth(1).context("usage: reactions_peer <email>")?;
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
    .context("send the message to react to")?;
    for emoji in ["👍", "🎉"] {
        messages::add_reaction(sent.id.clone(), me.id.clone(), emoji.to_string(), &state)
            .await
            .with_context(|| format!("react with {emoji}"))?;
    }
    println!("PEER_REACTED dm={dm_id}");

    let started = Instant::now();
    loop {
        match messages::get_reactions(sent.id.clone(), &state).await {
            Ok(reactions) => {
                let thumbs = reactions.iter().find(|r| r.emoji == "👍");
                if thumbs.is_some_and(|r| r.user_ids.iter().any(|u| u != &me.id)) {
                    println!("PEER_SAW_DEVICE_REACTION");
                    return Ok(());
                }
            }
            Err(e) => eprintln!("get_reactions: {e}"),
        }
        if started.elapsed() > WAIT {
            bail!("the phone's 👍 never showed");
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}
