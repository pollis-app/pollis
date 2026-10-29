//! Dev-DS peer for the mobile Maestro `membership` flow (#1506).
//!
//! Plays a group's creator against the phone, on the dev DS
//! (`POLLIS_DELIVERY_URL`), in one of two modes:
//!
//! * `signup <email>` — sign up and print `PEER_HANDLE=`, then exit. The
//!   script uses it for the account that stays a PENDING invitee.
//! * `admin <email> <phone-email> <pending-email>` — sign up, create a group
//!   named `$POLLIS_GROUP_NAME` with #General; invite the phone by email as
//!   soon as it has signed up (`PEER_INVITED_PHONE`); wait for it to accept
//!   (`PEER_SAW_PHONE_JOIN`); invite the pending account, wait until the
//!   group's invite list shows it, and make the phone an admin
//!   (`PEER_PROMOTED`); then wait for the phone to revoke that invite
//!   (`PEER_SAW_REVOKE`) and exit 0.
//!
//! `mobile/scripts/maestro-membership.sh` drives both sides.
//!
//!   POLLIS_DATA_DIR=$(mktemp -d) POLLIS_DELIVERY_URL=https://api-dev.pollis.com \
//!   POLLIS_DEV_ACCESS_KEY=… POLLIS_GROUP_NAME=… \
//!   cargo run -p pollis-tui --example membership_peer -- admin <email> <phone> <pending>

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use pollis_core::commands::groups;
use pollis_core::config::Config;
use pollis_core::state::AppState;

/// The dev DS's fixed OTP (`DEV_OTP`); override with `POLLIS_DEV_OTP`.
const DEFAULT_OTP: &str = "000000";
/// How long to wait for the phone at each step.
const WAIT: Duration = Duration::from_secs(600);

async fn sign_up(email: &str) -> Result<(Arc<AppState>, String, String)> {
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
    Ok((state, me.id, me.username))
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
    match args.first().map(String::as_str) {
        Some("signup") => {
            let email = args.get(1).context("usage: membership_peer signup <email>")?;
            sign_up(email).await?;
            Ok(())
        }
        Some("admin") => {
            let [_, email, phone, pending] = args.as_slice() else {
                bail!("usage: membership_peer admin <email> <phone-email> <pending-email>");
            };
            admin(email, phone, pending).await
        }
        _ => bail!("usage: membership_peer signup|admin …"),
    }
}

async fn admin(email: &str, phone: &str, pending: &str) -> Result<()> {
    let group_name = std::env::var("POLLIS_GROUP_NAME").context("POLLIS_GROUP_NAME is required")?;
    let (state, me, _) = sign_up(email).await?;

    let made = groups::create_group(group_name, None, me.clone(), Some(true), Some(false), &state)
        .await
        .context("create the group")?;
    let group_id = made.id.clone();

    // The phone signs up while this runs; until it has, the invite answers
    // "not found".
    wait_for("invite the phone", || async {
        Ok(groups::send_group_invite(group_id.clone(), me.clone(), phone.to_string(), &state)
            .await
            .ok())
    })
    .await?;
    println!("PEER_INVITED_PHONE");

    let phone_id = wait_for("the phone accepts", || async {
        let members = groups::get_group_members(group_id.clone(), me.clone(), &state).await?;
        Ok(members.into_iter().find(|m| m.user_id != me).map(|m| m.user_id))
    })
    .await?;
    println!("PEER_SAW_PHONE_JOIN");

    groups::send_group_invite(group_id.clone(), me.clone(), pending.to_string(), &state)
        .await
        .context("invite the pending account")?;
    // The list must show it before "gone" can mean anything: a DS without
    // the invite list answers an empty one, which would pass the wait below
    // without the phone doing a thing.
    wait_for("the pending invite is listed", || async {
        let invites = groups::list_group_invites(group_id.clone(), me.clone(), &state).await?;
        Ok((!invites.is_empty()).then_some(()))
    })
    .await?;
    groups::set_member_role(group_id.clone(), phone_id, "admin".to_string(), me.clone(), &state)
        .await
        .context("make the phone an admin")?;
    println!("PEER_PROMOTED");

    wait_for("the phone revokes", || async {
        let invites = groups::list_group_invites(group_id.clone(), me.clone(), &state).await?;
        Ok(invites.is_empty().then_some(()))
    })
    .await?;
    println!("PEER_SAW_REVOKE");

    // Wake-ups are sent on spawned tasks; let them leave before the runtime
    // goes away with this process.
    tokio::time::sleep(Duration::from_secs(5)).await;
    Ok(())
}
