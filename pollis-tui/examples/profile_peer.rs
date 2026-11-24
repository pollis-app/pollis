//! Dev-DS peer for the mobile Maestro `profile-change` flows (#2084).
//!
//! Plays a group's creator who renames itself while the phone is looking, against the DS named
//! by `POLLIS_DELIVERY_URL` (the dev DS — it fixes the OTP):
//!
//! `admin <email> <phone-email>` — sign up (`PEER_HANDLE=`, `PEER_ID=`), create a group named
//! `$POLLIS_GROUP_NAME` with #General, invite the phone by email once it has signed up, wait for
//! it to accept (`PEER_SAW_PHONE_JOIN`), post `hello before the rename` (`PEER_READY`); then wait
//! for the file `$POLLIS_RENAME_TRIGGER` to exist, change its handle to `$POLLIS_NEW_HANDLE` and
//! its display name to `$POLLIS_NEW_NAME` (`PEER_RENAMED`), and exit 0 once the DS's wake-ups
//! have had time to leave.
//!
//! Also drives `maestro-display-names.sh` (#2085): `POLLIS_PEER_NAME` sets a display name right
//! after sign-up, `POLLIS_DM_PHONE=1` opens a DM to the phone once it has joined (a message
//! request on the phone) and writes `hello in the dm` there, and with no `POLLIS_RENAME_TRIGGER`
//! the peer exits after `PEER_READY` plus a short wait instead of renaming.
//!
//! `mobile/scripts/maestro-profile-change.sh` drives it. Not a shipping binary: examples build
//! against pollis-tui's dev-dependencies.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use pollis_core::commands::{dm, groups, messages, user};
use pollis_core::config::Config;
use pollis_core::state::AppState;

/// The dev DS's fixed OTP (`DEV_OTP`); override with `POLLIS_DEV_OTP`.
const DEFAULT_OTP: &str = "000000";
/// How long to wait for the phone at each step.
const WAIT: Duration = Duration::from_secs(600);

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
    println!("PEER_ID={}", me.id);
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
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [mode, email, phone] = args.as_slice() else {
        bail!("usage: profile_peer admin <email> <phone-email>");
    };
    if mode != "admin" {
        bail!("usage: profile_peer admin <email> <phone-email>");
    }
    admin(email, phone).await
}

async fn admin(email: &str, phone: &str) -> Result<()> {
    let group_name = std::env::var("POLLIS_GROUP_NAME").context("POLLIS_GROUP_NAME is required")?;
    let trigger = std::env::var("POLLIS_RENAME_TRIGGER").ok();
    let (state, me) = sign_up(email).await?;
    if let Ok(name) = std::env::var("POLLIS_PEER_NAME") {
        user::update_user_profile(me.clone(), None, Some(name), None, None, &state)
            .await
            .context("set the display name")?;
    }

    let made = groups::create_group(group_name, None, me.clone(), Some(true), Some(false), &state)
        .await
        .context("create the group")?;
    let group_id = made.id.clone();

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
    if std::env::var("POLLIS_DM_PHONE").as_deref() == Ok("1") {
        let opened = dm::create_dm_channel(me.clone(), vec![phone_id.clone()], &state)
            .await
            .context("open a DM to the phone")?;
        messages::send_message(opened.id.clone(), me.clone(), "hello in the dm".to_string(), None, None, None, &state)
            .await
            .context("write in the DM")?;
        println!("PEER_OPENED_DM");
    }

    let general = groups::list_group_channels(group_id.clone(), me.clone(), &state)
        .await?
        .into_iter()
        .next()
        .context("the group has a channel")?;
    messages::send_message(
        general.id.clone(),
        me.clone(),
        "hello before the rename".to_string(),
        None,
        None,
        None,
        &state,
    )
    .await
    .context("post the message")?;
    println!("PEER_READY");

    let Some(trigger) = trigger else {
        // Nothing to rename: stay a member long enough for the phone's reads, then leave.
        tokio::time::sleep(Duration::from_secs(5)).await;
        return Ok(());
    };
    let new_handle = std::env::var("POLLIS_NEW_HANDLE").context("POLLIS_NEW_HANDLE is required")?;
    let new_name = std::env::var("POLLIS_NEW_NAME").context("POLLIS_NEW_NAME is required")?;
    wait_for("the rename trigger", || async {
        Ok(std::path::Path::new(&trigger).exists().then_some(()))
    })
    .await?;
    user::update_user_profile(me.clone(), Some(new_handle), Some(new_name), None, None, &state)
        .await
        .context("rename")?;
    println!("PEER_RENAMED");

    // The DS sends its wake-ups on spawned tasks; give them time to leave.
    tokio::time::sleep(Duration::from_secs(5)).await;
    Ok(())
}
