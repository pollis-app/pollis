//! Dev-DS peer for the mobile Maestro `composing` two-client flow (#1507).
//!
//! Signs up on the dev DS (`POLLIS_DELIVERY_URL`), prints its handle, accepts
//! the phone's group invite, posts more than a screen of lines and prints
//! `PEER_READY`. Then each time the script touches `go-<n>` in
//! `COMPOSING_PEER_SIGNAL_DIR` (n = 1, 2), it sends three lines a second apart
//! and prints `PEER_SENT_<n>`. `mobile/scripts/maestro-composing.sh` drives
//! both sides.
//!
//!   POLLIS_DATA_DIR=$(mktemp -d) POLLIS_DELIVERY_URL=https://api-dev.pollis.com \
//!   COMPOSING_PEER_SIGNAL_DIR=$(mktemp -d) POLLIS_DEV_ACCESS_KEY=… \
//!   cargo run -p pollis-tui --example composing_peer -- <email>

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use pollis_core::commands::{groups, messages};
use pollis_core::config::Config;
use pollis_core::state::AppState;

/// The dev DS's fixed OTP (`DEV_OTP`); override with `POLLIS_DEV_OTP`.
const DEFAULT_OTP: &str = "000000";
/// How long to wait for the phone, or the script, at each step.
const WAIT: Duration = Duration::from_secs(600);
/// How many batches the script releases.
const BATCHES: usize = 2;
/// Lines posted before the test, more than one screen holds.
const HISTORY: usize = 15;

async fn wait_for_signal(dir: &Path, name: &str) -> Result<()> {
    let started = Instant::now();
    let path = dir.join(name);
    while !path.exists() {
        if started.elapsed() > WAIT {
            bail!("no {name} signal within {WAIT:?}");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Ok(())
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let email = std::env::args().nth(1).context("usage: composing_peer <email>")?;
    let signals: PathBuf = std::env::var("COMPOSING_PEER_SIGNAL_DIR")
        .context("COMPOSING_PEER_SIGNAL_DIR is required")?
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
    loop {
        if let Some(inv) = groups::get_pending_invites(me.id.clone(), &state).await?.into_iter().next() {
            groups::accept_group_invite(inv.id, me.id.clone(), &state).await?;
            println!("PEER_JOINED");
            break;
        }
        if started.elapsed() > WAIT {
            bail!("no group invite arrived within {WAIT:?}");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // The group's first text channel. The welcome can trail the accept, so
    // retry the first send until the channel is ready.
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
    loop {
        let _ = pollis_tui::data::channel_messages(&state, &me.id, &channel_id, None).await;
        match send(&state, &me.id, &me.username, &channel_id, "peer is here").await {
            Ok(()) => break,
            Err(e) => {
                eprintln!("channel send not ready yet: {e}");
                if started.elapsed() > WAIT {
                    bail!("could not post in the channel: {e}");
                }
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    }
    // More than a screen, so the list scrolls.
    for n in 1..=HISTORY {
        send(&state, &me.id, &me.username, &channel_id, &format!("peer history {n}")).await?;
    }
    println!("PEER_READY");

    for batch in 1..=BATCHES {
        wait_for_signal(&signals, &format!("go-{batch}")).await?;
        for line in 1..=3 {
            send(&state, &me.id, &me.username, &channel_id, &format!("peer batch {batch} line {line}")).await?;
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        println!("PEER_SENT_{batch}");
    }
    Ok(())
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
