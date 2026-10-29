//! Dev-DS peer for the mobile Maestro `device-revoked` two-device flow (#1338).
//!
//! The account's first device: signs up on the dev DS (`POLLIS_DELIVERY_URL`)
//! and prints the recovery key the phone uses to enroll as the second device.
//! Once that device appears and the script has closed the app on the phone
//! (`touch $POLLIS_REVOKE_GO`), it revokes the phone and prints
//! `PEER_REVOKED`. `mobile/scripts/maestro-device-revoked.sh` drives both sides.
//!
//!   POLLIS_DATA_DIR=$(mktemp -d) POLLIS_DELIVERY_URL=https://api-dev.pollis.com \
//!   POLLIS_DEV_ACCESS_KEY=… POLLIS_REVOKE_GO=/tmp/go \
//!   cargo run -p pollis-tui --example revoke_peer -- <email>

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use pollis_core::commands::auth;
use pollis_core::config::Config;
use pollis_core::state::AppState;

/// The dev DS's fixed OTP (`DEV_OTP`); override with `POLLIS_DEV_OTP`.
const DEFAULT_OTP: &str = "000000";
/// How long to wait for the phone at each step.
const WAIT: Duration = Duration::from_secs(600);

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let email = std::env::args().nth(1).context("usage: revoke_peer <email>")?;
    let go = std::env::var("POLLIS_REVOKE_GO").context("POLLIS_REVOKE_GO (a file the script creates) is required")?;
    let otp = std::env::var("POLLIS_DEV_OTP").unwrap_or_else(|_| DEFAULT_OTP.to_string());
    let config = Config::from_env().context("POLLIS_DELIVERY_URL (the dev DS) is required")?;
    let ds = config.pollis_delivery_url.clone().unwrap_or_default();
    if !ds.contains("api-dev") && !ds.contains("localhost") {
        bail!("refusing to run against {ds:?} — dev DS only");
    }
    let state = Arc::new(AppState::new(config).await.context("AppState")?);

    pollis_tui::auth::request_otp(&state, &email).await?;
    let me = pollis_tui::auth::verify_otp(&state, &email, &otp).await?;
    let recovery = me.new_secret_key.clone().context("a first sign-up returns the recovery key")?;
    pollis_tui::auth::set_pin_and_init(&state, &me.id, "0000").await?;
    let mine = state.device_id.lock().await.clone().context("device id")?;
    println!("PEER_RECOVERY_KEY={recovery}");

    // The phone enrolls with the recovery key: a second live device appears.
    let started = Instant::now();
    let phone = loop {
        let devices = auth::list_user_devices(&state, me.id.clone()).await?;
        let other = devices
            .iter()
            .filter_map(|d| d.get("device_id").and_then(|v| v.as_str()).map(str::to_string))
            .find(|id| *id != mine);
        if let Some(id) = other {
            break id;
        }
        if started.elapsed() > WAIT {
            bail!("the phone never enrolled within {WAIT:?}");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    };
    println!("PEER_SAW_PHONE");

    // Revoke only once the phone's app is closed, so no live nudge can reach it.
    let started = Instant::now();
    while !std::path::Path::new(&go).exists() {
        if started.elapsed() > WAIT {
            bail!("the script never said go");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    auth::revoke_device(&state, me.id.clone(), phone).await.context("revoke the phone")?;
    println!("PEER_REVOKED");
    Ok(())
}
