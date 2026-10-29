//! Dev-DS peer for the store-screenshot run (`mobile/scripts/maestro-store-shots.sh`).
//!
//! The store shots need a second person in the conversation, so the phone is
//! never talking to itself. This peer signs up a fresh account on the dev DS,
//! prints its handle, then waits for the phone to invite it to a group and to
//! open a DM with it. It accepts both and posts its half of the conversation:
//! two DM replies and a channel message (`PEER_SENT`). Once the phone has
//! answered in the channel it reacts to that answer, sends one more line and
//! exits (`PEER_DONE`).
//!
//! Not a shipping binary: examples build against pollis-tui's
//! dev-dependencies.
//!
//!   POLLIS_DATA_DIR=$(mktemp -d) POLLIS_DELIVERY_URL=https://api-dev.pollis.com \
//!   POLLIS_DEV_ACCESS_KEY=… cargo run -p pollis-tui --example store_shots_peer -- <email>

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use pollis_core::commands::{dm, groups, messages};
use pollis_core::config::Config;
use pollis_core::state::AppState;

/// The dev DS's fixed OTP (`DEV_OTP`); override with `POLLIS_DEV_OTP`.
const DEFAULT_OTP: &str = "000000";
/// How long to wait for the phone at each step.
const WAIT: Duration = Duration::from_secs(900);

const DM_LINES: [&str; 2] = [
    "Nice, this one's actually end-to-end encrypted",
    "The server only ever sees ciphertext. Not even Pollis can read it.",
];
const CHANNEL_LINE: &str = "Morning! Onboarding mockups are up, feedback welcome";
/// The phone's channel answer contains this; the peer reacts to it.
const PHONE_ANSWER: &str = "handoff build";
const CLOSING_LINE: &str = "Every message here is sealed before it leaves your phone";

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let email = std::env::args().nth(1).context("usage: store_shots_peer <email>")?;
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
    let mut dm_id = None;
    let mut joined = false;
    while dm_id.is_none() || !joined {
        if !joined {
            if let Some(inv) = groups::get_pending_invites(me.id.clone(), &state).await?.into_iter().next() {
                groups::accept_group_invite(inv.id, me.id.clone(), &state).await?;
                joined = true;
                println!("PEER_JOINED");
            }
        }
        if dm_id.is_none() {
            if let Some(req) = dm::list_dm_requests(me.id.clone(), &state).await?.into_iter().next() {
                dm::accept_dm_request(req.id.clone(), me.id.clone(), &state).await?;
                dm_id = Some(req.id);
                println!("PEER_ACCEPTED_DM");
            }
        }
        if started.elapsed() > WAIT {
            bail!("timed out waiting for the phone (joined: {joined}, dm: {})", dm_id.is_some());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let dm_id = dm_id.unwrap();

    pollis_tui::data::dm_messages(&state, &me.id, &dm_id, None).await?;
    for line in DM_LINES {
        send(&state, &me.id, &me.username, &dm_id, line).await?;
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // The welcome can trail the accept, so retry until the channel decrypts.
    let channel_id = loop {
        let gs = groups::list_user_groups_with_channels(me.id.clone(), &state).await?;
        if let Some(ch) = gs.iter().flat_map(|g| g.channels.iter()).find(|c| c.channel_type == "text") {
            break ch.id.clone();
        }
        if started.elapsed() > WAIT {
            bail!("joined group has no text channel");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    };
    let mut sent = false;
    while !sent {
        let _ = pollis_tui::data::channel_messages(&state, &me.id, &channel_id, None).await;
        match send(&state, &me.id, &me.username, &channel_id, CHANNEL_LINE).await {
            Ok(()) => sent = true,
            Err(e) => {
                eprintln!("channel send not ready yet: {e}");
                if started.elapsed() > WAIT {
                    bail!("could not post in the channel: {e}");
                }
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    }
    println!("PEER_SENT");

    let started = Instant::now();
    loop {
        let page = pollis_tui::data::channel_messages(&state, &me.id, &channel_id, None).await?;
        let answer = page.messages.iter().find(|m| {
            m.sender_id != me.id
                && m.content.as_deref().is_some_and(|c| c.to_lowercase().contains(PHONE_ANSWER))
        });
        if let Some(m) = answer {
            messages::add_reaction(m.id.clone(), me.id.clone(), "🎉".to_string(), &state)
                .await
                .context("react to the phone's answer")?;
            send(&state, &me.id, &me.username, &channel_id, CLOSING_LINE).await?;
            println!("PEER_DONE");
            return Ok(());
        }
        if started.elapsed() > WAIT {
            bail!("the phone's channel answer never arrived");
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

async fn send(state: &Arc<AppState>, me: &str, username: &str, conv: &str, text: &str) -> Result<()> {
    messages::send_message(
        conv.to_string(),
        me.to_string(),
        text.to_string(),
        None,
        None,
        Some(username.to_string()),
        state,
    )
    .await
    .with_context(|| format!("send {text:?}"))?;
    Ok(())
}
