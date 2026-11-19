//! Dev-DS peer for the mobile Maestro `group-aliases` flow (#2003).
//!
//! Plays a group's creator with a per-group alias against the phone under test,
//! against the DS named by `POLLIS_DELIVERY_URL` (the dev DS — it fixes the OTP):
//!
//! `admin <email> <phone-email>` — sign up (`PEER_HANDLE=`, `PEER_ID=`), create
//! a group named `$POLLIS_GROUP_NAME` with #General, set its own alias there to
//! `$POLLIS_PEER_ALIAS`, invite the phone by email once it has signed up, wait
//! for it to accept (`PEER_SAW_PHONE_JOIN`), post a message that mentions the
//! peer's own `@handle` and make the phone an admin (`PEER_READY`); then wait
//! for the phone, as an admin, to reset the peer's alias (`PEER_SAW_RESET`) and
//! exit 0.
//!
//! `mobile/scripts/maestro-group-aliases.sh` drives it. Not a shipping binary:
//! examples build against pollis-tui's dev-dependencies.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use pollis_core::commands::{groups, messages};
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
    println!("PEER_ID={}", me.id);
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
    let [mode, email, phone] = args.as_slice() else {
        bail!("usage: alias_peer admin <email> <phone-email>");
    };
    if mode != "admin" {
        bail!("usage: alias_peer admin <email> <phone-email>");
    }
    admin(email, phone).await
}

async fn admin(email: &str, phone: &str) -> Result<()> {
    let group_name = std::env::var("POLLIS_GROUP_NAME").context("POLLIS_GROUP_NAME is required")?;
    let alias = std::env::var("POLLIS_PEER_ALIAS").context("POLLIS_PEER_ALIAS is required")?;
    let (state, me, handle) = sign_up(email).await?;

    let made = groups::create_group(group_name, None, me.clone(), Some(true), Some(false), &state)
        .await
        .context("create the group")?;
    let group_id = made.id.clone();
    groups::set_group_alias(me.clone(), Some(group_id.clone()), Some(alias), None, &state)
        .await
        .context("set the peer's alias")?;

    // Until the phone has signed up, the invite answers "not found".
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

    let general = groups::list_group_channels(group_id.clone(), me.clone(), &state)
        .await?
        .into_iter()
        .next()
        .context("the group has a channel")?;
    // The mention is written as the handle; the phone renders the alias.
    messages::send_message(
        general.id.clone(),
        me.clone(),
        format!("roll call @{handle}"),
        None,
        None,
        Some(handle.clone()),
        &state,
    )
    .await
    .context("post the message")?;
    groups::set_member_role(group_id.clone(), phone_id, "admin".to_string(), me.clone(), &state)
        .await
        .context("make the phone an admin")?;
    println!("PEER_READY");

    wait_for("the phone resets the alias", || async {
        let aliases = groups::list_my_aliases(me.clone(), &state).await?;
        Ok((!aliases.iter().any(|a| a.group_id == group_id)).then_some(()))
    })
    .await?;
    println!("PEER_SAW_RESET");

    // Wake-ups are sent on spawned tasks; let them leave before exiting.
    tokio::time::sleep(Duration::from_secs(5)).await;
    Ok(())
}
