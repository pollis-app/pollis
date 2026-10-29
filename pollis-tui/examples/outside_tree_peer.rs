//! Dev-DS peers for the mobile Maestro `device-outside-tree` flow (#1571).
//!
//! The incident: a user's second device (a phone) was enrolled, then a peer
//! opened a DM with the user, the user accepted on their FIRST device and the
//! two exchanged messages — all while the phone never ran. When the phone
//! finally opened the DM it was empty. This drives exactly that, with the
//! phone's KeyPackage pool spent first, so the only thing that can add it is
//! its last-resort package.
//!
//! Two roles, two processes (each its own data dir):
//!
//! * `owner <email>` — the user's FIRST device. Signs up, prints
//!   `PEER_RECOVERY_KEY=` (the phone enrolls with it) and `PEER_USER_ID=`, then
//!   `PEER_SAW_PHONE=<device id>` once the phone has enrolled. Accepts the
//!   creator's DM request and replies once to its first message
//!   (`PEER_REPLIED`).
//! * `creator <email> <owner-user-id> <owner-email>` — the
//!   peer who opens the DM. Waits for `$POLLIS_OUTSIDE_TREE_GO` (the script
//!   creates it once the phone's app is closed), spends the phone's regular
//!   pool by inviting the owner to five groups (each invite adds every one of
//!   the owner's devices, claiming a package from each), then opens the DM,
//!   sends, waits for the owner's reply, sends again and prints `PEER_DONE`.
//!
//! Not a shipping binary: examples build against pollis-tui's dev-dependencies.
//!
//!   POLLIS_DATA_DIR=$(mktemp -d) POLLIS_DELIVERY_URL=https://api-dev.pollis.com \
//!   POLLIS_DEV_ACCESS_KEY=… cargo run -p pollis-tui --example outside_tree_peer -- owner <email>

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use pollis_core::commands::{auth, dm, groups, messages};
use pollis_core::config::Config;
use pollis_core::state::AppState;

/// The dev DS's fixed OTP (`DEV_OTP`); override with `POLLIS_DEV_OTP`.
const DEFAULT_OTP: &str = "000000";
/// How long to wait for the phone, the other peer or the script at each step.
const WAIT: Duration = Duration::from_secs(600);
/// The phone's regular pool (`key_packages::POOL_TARGET`): this many adds
/// spend it.
const POOL: usize = 5;

pub const FIRST: &str = "sent while your phone was off";
pub const REPLY: &str = "replied from my other device";
pub const SECOND: &str = "and one more before it wakes";

async fn sign_up(email: &str) -> Result<(Arc<AppState>, String, String, Option<String>)> {
    let otp = std::env::var("POLLIS_DEV_OTP").unwrap_or_else(|_| DEFAULT_OTP.to_string());
    let config = Config::from_env().context("POLLIS_DELIVERY_URL (the dev DS) is required")?;
    let ds = config.pollis_delivery_url.clone().unwrap_or_default();
    if !ds.contains("api-dev") && !ds.contains("localhost") && !ds.contains("127.0.0.1") {
        bail!("refusing to run against {ds:?} — dev DS only");
    }
    let state = Arc::new(AppState::new(config).await.context("AppState")?);
    pollis_tui::auth::request_otp(&state, email).await?;
    let me = pollis_tui::auth::verify_otp(&state, email, &otp).await?;
    let recovery = me.new_secret_key.clone();
    pollis_tui::auth::set_pin_and_init(&state, &me.id, "0000").await?;
    Ok((state, me.id, me.username, recovery))
}

/// Retry `f` every couple of seconds until it answers `Some`, or fail after `WAIT`.
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

async fn dm_has(state: &Arc<AppState>, me: &str, dm_id: &str, text: &str) -> Result<Option<()>> {
    let page = pollis_tui::data::dm_messages(state, me, dm_id, None).await?;
    Ok(page
        .messages
        .iter()
        .any(|m| m.content.as_deref() == Some(text))
        .then_some(()))
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("owner") => {
            let email = args.get(1).context("usage: outside_tree_peer owner <email>")?;
            owner(email).await
        }
        Some("creator") => {
            let [_, email, owner_id, owner_email] = args.as_slice() else {
                bail!("usage: outside_tree_peer creator <email> <owner-user-id> <owner-email>");
            };
            creator(email, owner_id, owner_email).await
        }
        _ => bail!("usage: outside_tree_peer owner|creator …"),
    }
}

async fn owner(email: &str) -> Result<()> {
    let (state, me, _, recovery) = sign_up(email).await?;
    let recovery = recovery.context("a first sign-up returns the recovery key")?;
    let mine = state.device_id.lock().await.clone().context("device id")?;
    println!("PEER_RECOVERY_KEY={recovery}");
    println!("PEER_USER_ID={me}");

    let phone = wait_for("the phone enrolls", || async {
        let devices = auth::list_user_devices(&state, me.clone()).await?;
        Ok(devices
            .iter()
            .filter_map(|d| d.get("device_id").and_then(|v| v.as_str()).map(str::to_string))
            .find(|id| *id != mine))
    })
    .await?;
    println!("PEER_SAW_PHONE={phone}");

    let dm_id = wait_for("the creator's DM request", || async {
        Ok(dm::list_dm_requests(me.clone(), &state).await?.into_iter().next().map(|r| r.id))
    })
    .await?;
    dm::accept_dm_request(dm_id.clone(), me.clone(), &state)
        .await
        .context("accept the DM")?;
    println!("PEER_ACCEPTED");

    wait_for("the creator's first message", || dm_has(&state, &me, &dm_id, FIRST)).await?;
    messages::send_message(dm_id.clone(), me.clone(), REPLY.to_string(), None, None, None, &state)
        .await
        .context("reply")?;
    println!("PEER_REPLIED");
    Ok(())
}

async fn creator(email: &str, owner_id: &str, owner_email: &str) -> Result<()> {
    let go = std::env::var("POLLIS_OUTSIDE_TREE_GO")
        .context("POLLIS_OUTSIDE_TREE_GO (a file the script creates) is required")?;
    let (state, me, handle, _) = sign_up(email).await?;
    println!("PEER_CREATOR_HANDLE={handle}");

    let started = Instant::now();
    while !std::path::Path::new(&go).exists() {
        if started.elapsed() > WAIT {
            bail!("the script never said go");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    // Spend the phone's regular pool the way it is spent in life: every
    // conversation the owner is invited to adds each of the owner's devices at
    // invite time, claiming one package from each, and the phone is not running
    // to refill. After `POOL` invites its regular pool is empty.
    for i in 0..POOL {
        let made = groups::create_group(format!("Outside the tree {i}"), None, me.clone(), Some(true), Some(false), &state)
            .await
            .context("create a group")?;
        groups::send_group_invite(made.id.clone(), me.clone(), owner_email.to_string(), &state)
            .await
            .context("invite the owner")?;
    }
    println!("PEER_POOL_SPENT");

    let made = dm::create_dm_channel(me.clone(), vec![me.clone(), owner_id.to_string()], &state)
        .await
        .context("open the DM")?;
    pollis_tui::data::dm_messages(&state, &me, &made.id, None).await?;
    messages::send_message(made.id.clone(), me.clone(), FIRST.to_string(), None, None, None, &state)
        .await
        .context("first message")?;
    wait_for("the owner's reply", || dm_has(&state, &me, &made.id, REPLY)).await?;
    messages::send_message(made.id.clone(), me.clone(), SECOND.to_string(), None, None, None, &state)
        .await
        .context("second message")?;
    println!("PEER_DONE dm={}", made.id);
    Ok(())
}
