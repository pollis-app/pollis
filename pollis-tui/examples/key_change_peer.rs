//! Dev-DS peer for the mobile Maestro `key-change` flow (#1605).
//!
//! Two invocations, each in its own `POLLIS_DATA_DIR` (a separate device):
//!
//! * `first <email>` signs up a fresh account on the DS named by
//!   `POLLIS_DELIVERY_URL` (the dev DS, which fixes the OTP), prints its handle,
//!   waits for the phone to open a DM with it, accepts, sends one message and
//!   prints `PEER_READY dm=<id>`.
//! * `reset <email> <phone-email>` signs in to the SAME account from a new
//!   device and takes the soft-recovery path (`reset_identity_and_recover`):
//!   a new account key under the same user id, which is exactly a safety-number
//!   change for everyone who pinned the old one. Recovery drops the account's
//!   memberships, so it then opens a new DM with the phone and prints
//!   `PEER_RESET dm=<id>`. The phone's ingest of that DM re-pins the key and
//!   must show the change as a line in the timeline.
//!
//! Not a shipping binary: examples build against pollis-tui's dev-dependencies.
//!
//!   POLLIS_DATA_DIR=$(mktemp -d) POLLIS_DELIVERY_URL=https://api-dev.pollis.com \
//!   POLLIS_DEV_ACCESS_KEY=… cargo run -p pollis-tui --example key_change_peer -- first <email>

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use pollis_core::commands::{dm, messages, user};
use pollis_core::config::Config;
use pollis_core::state::AppState;

/// The dev DS's fixed OTP (`DEV_OTP`); override with `POLLIS_DEV_OTP`.
const DEFAULT_OTP: &str = "000000";
/// How long to wait for the phone at each step.
const WAIT: Duration = Duration::from_secs(300);

async fn state() -> Result<Arc<AppState>> {
    let config = Config::from_env().context("POLLIS_DELIVERY_URL (the dev DS) is required")?;
    let ds = config.pollis_delivery_url.clone().unwrap_or_default();
    if !ds.contains("api-dev") && !ds.contains("localhost") {
        bail!("refusing to run against {ds:?} — dev DS only");
    }
    Ok(Arc::new(AppState::new(config).await.context("AppState")?))
}

fn otp() -> String {
    std::env::var("POLLIS_DEV_OTP").unwrap_or_else(|_| DEFAULT_OTP.to_string())
}

async fn first(email: &str) -> Result<()> {
    let state = state().await?;
    pollis_tui::auth::request_otp(&state, email).await?;
    let me = pollis_tui::auth::verify_otp(&state, email, &otp()).await?;
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
    // Wake-ups are sent on spawned tasks; let them leave before exiting.
    tokio::time::sleep(Duration::from_secs(5)).await;
    Ok(())
}

async fn reset(email: &str, phone: &str) -> Result<()> {
    let state = state().await?;
    pollis_tui::auth::request_otp(&state, email).await?;
    let me = pollis_tui::auth::verify_otp(&state, email, &otp()).await?;
    pollis_tui::enroll::reset_and_recover(&state, me.id.clone(), email.to_string())
        .await
        .context("reset_identity_and_recover")?;
    pollis_tui::enroll::set_pin_and_finalize(&state, &me.id, "0000")
        .await
        .context("set_pin_and_finalize after the reset")?;

    let phone_id = user::search_user_by_username(phone.to_string(), &state)
        .await?
        .map(|p| p.id)
        .context("the phone's account")?;
    let made = dm::create_dm_channel(me.id.clone(), vec![phone_id], &state)
        .await
        .context("open a DM with the phone")?;
    println!("PEER_RESET dm={}", made.id);
    tokio::time::sleep(Duration::from_secs(5)).await;
    Ok(())
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [mode, email] if mode == "first" => first(email).await,
        [mode, email, phone] if mode == "reset" => reset(email, phone).await,
        _ => bail!("usage: key_change_peer first <email> | reset <email> <phone-email>"),
    }
}
