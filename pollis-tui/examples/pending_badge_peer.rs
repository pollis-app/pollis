//! Dev-DS peer for the mobile Maestro `pending-badges` flow (#1561).
//!
//! The stranger who sends the phone message requests and group invites, in one of two modes:
//!
//! * `request <email> <phone-email>` — sign up, wait until the phone's account
//!   exists, open a DM with it (the phone sees a message request) and print
//!   `PEER_REQUESTED`.
//! * `invite <email> <phone-email>` — sign up, create the groups named by
//!   `$POLLIS_GROUP_NAMES` (comma separated, each with #General), invite the
//!   phone by email to every one and print `PEER_INVITED`.
//!
//! `mobile/scripts/maestro-pending-badges.sh` drives it. Not a shipping
//! binary: examples build against pollis-tui's dev-dependencies.
//!
//!   POLLIS_DATA_DIR=$(mktemp -d) POLLIS_DELIVERY_URL=https://api-dev.pollis.com \
//!   POLLIS_DEV_ACCESS_KEY=… \
//!   cargo run -p pollis-tui --example pending_badge_peer -- request <email> <phone>

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use pollis_core::commands::{dm, groups, user};
use pollis_core::config::Config;
use pollis_core::state::AppState;

/// The dev DS's fixed OTP (`DEV_OTP`); override with `POLLIS_DEV_OTP`.
const DEFAULT_OTP: &str = "000000";
/// How long to wait for the phone's account to exist.
const WAIT: Duration = Duration::from_secs(300);

async fn sign_up(email: &str) -> Result<(Arc<AppState>, String)> {
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
    Ok((state, me.id))
}

/// Retry `f` every few seconds until it answers `Some`, or fail after `WAIT`.
async fn wait_for<T, F, Fut>(what: &str, mut f: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Option<T>>>,
{
    let started = Instant::now();
    loop {
        match f().await {
            Ok(Some(v)) => return Ok(v),
            Ok(None) => {}
            Err(e) => eprintln!("{what}: {e}"),
        }
        if started.elapsed() > WAIT {
            bail!("{what}: not within {WAIT:?}");
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [mode, email, phone] = args.as_slice() else {
        bail!("usage: pending_badge_peer request|invite <email> <phone-email>");
    };
    let (state, me) = sign_up(email).await?;
    match mode.as_str() {
        "request" => {
            let phone_id = wait_for("the phone's account", || async {
                Ok(user::search_user_by_username(phone.to_string(), &state)
                    .await?
                    .map(|p| p.id))
            })
            .await?;
            dm::create_dm_channel(me.clone(), vec![phone_id], &state)
                .await
                .context("open a DM with the phone")?;
            println!("PEER_REQUESTED");
        }
        "invite" => {
            let names = std::env::var("POLLIS_GROUP_NAMES").context("POLLIS_GROUP_NAMES is required")?;
            for name in names.split(',').map(str::trim).filter(|n| !n.is_empty()) {
                let made = groups::create_group(name.to_string(), None, me.clone(), Some(true), Some(false), &state)
                    .await
                    .with_context(|| format!("create {name:?}"))?;
                let group_id = made.id.clone();
                wait_for("invite the phone", || async {
                    Ok(groups::send_group_invite(group_id.clone(), me.clone(), phone.to_string(), &state)
                        .await
                        .ok())
                })
                .await?;
            }
            println!("PEER_INVITED");
        }
        other => bail!("unknown mode {other:?}: request|invite"),
    }
    // Wake-ups are sent on spawned tasks; let them leave before exiting.
    tokio::time::sleep(Duration::from_secs(5)).await;
    Ok(())
}
