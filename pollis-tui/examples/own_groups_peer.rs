//! Dev-DS peer for the mobile Maestro `own-groups` two-device flow.
//!
//! A group one of a user's devices creates must reach the user's OTHER devices
//! without a refresh, in both directions. This peer is the account's FIRST
//! device (standing in for the user's desktop): it signs up on the DS named by
//! `POLLIS_DELIVERY_URL` (the dev DS — it fixes the OTP) and prints the
//! account's recovery key, which the phone under test uses to enroll as the
//! second device. Then:
//!
//! 1. The phone creates a group with #General and posts in it. The peer must
//!    list that group and read the post — so the phone's create added this
//!    device to the MLS group (`PEER_READ_PHONE_MESSAGE`).
//! 2. Once the driving script says go (`touch $POLLIS_OWN_GROUPS_GO`), the peer
//!    creates a group named `$POLLIS_PEER_GROUP_NAME` and prints its id
//!    (`PEER_CREATED_GROUP=`). The phone, which never leaves the Groups tab,
//!    must show it on its own — the inbox wake-up `create_group` sends is the
//!    only thing that can tell it.
//!
//! `mobile/scripts/maestro-own-groups.sh` drives both sides. Not a shipping
//! binary: examples build against pollis-tui's dev-dependencies.
//!
//!   POLLIS_DATA_DIR=$(mktemp -d) POLLIS_DELIVERY_URL=https://api-dev.pollis.com \
//!   POLLIS_DEV_ACCESS_KEY=… POLLIS_OWN_GROUPS_GO=/tmp/go POLLIS_PEER_GROUP_NAME=… \
//!   cargo run -p pollis-tui --example own_groups_peer -- <email>

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use pollis_core::commands::{auth, groups};
use pollis_core::config::Config;
use pollis_core::state::AppState;

/// The dev DS's fixed OTP (`DEV_OTP`); override with `POLLIS_DEV_OTP`.
const DEFAULT_OTP: &str = "000000";
/// What the phone posts in its new group (lower-cased: the phone's keyboard
/// may capitalise the first letter).
const PHONE_TEXT: &str = "hello from the phone";
/// How long to wait for the phone at each step.
const WAIT: Duration = Duration::from_secs(600);

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let email = std::env::args().nth(1).context("usage: own_groups_peer <email>")?;
    let go = std::env::var("POLLIS_OWN_GROUPS_GO")
        .context("POLLIS_OWN_GROUPS_GO (a file the script creates) is required")?;
    let peer_group_name =
        std::env::var("POLLIS_PEER_GROUP_NAME").context("POLLIS_PEER_GROUP_NAME is required")?;
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
    loop {
        let devices = auth::list_user_devices(&state, me.id.clone()).await?;
        let phone = devices
            .iter()
            .filter_map(|d| d.get("device_id").and_then(|v| v.as_str()))
            .any(|id| id != mine);
        if phone {
            break;
        }
        if started.elapsed() > WAIT {
            bail!("the phone never enrolled within {WAIT:?}");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    println!("PEER_SAW_PHONE");

    // 1. The phone's group, and the post in its #General.
    let started = Instant::now();
    let channel_id = loop {
        let listed = groups::list_user_groups_with_channels(me.id.clone(), &state).await?;
        if let Some(c) = listed.iter().flat_map(|g| g.channels.iter()).next() {
            break c.id.clone();
        }
        if started.elapsed() > WAIT {
            bail!("the phone's group never appeared within {WAIT:?}");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    };
    println!("PEER_SAW_PHONE_GROUP");
    let started = Instant::now();
    loop {
        // Ingest-then-read, as opening the channel does: this is what applies
        // the Welcome the phone's create queued for this device.
        match pollis_tui::data::channel_messages(&state, &me.id, &channel_id, None).await {
            Ok(page) => {
                let read = page.messages.iter().any(|m| {
                    m.content.as_deref().is_some_and(|c| c.to_lowercase().contains(PHONE_TEXT))
                });
                if read {
                    break;
                }
            }
            Err(e) => eprintln!("channel_messages: {e}"),
        }
        if started.elapsed() > WAIT {
            bail!("the phone's post never became readable within {WAIT:?}");
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    println!("PEER_READ_PHONE_MESSAGE");

    // 2. The other direction, on the script's word.
    let started = Instant::now();
    while !std::path::Path::new(&go).exists() {
        if started.elapsed() > WAIT {
            bail!("the script never said go");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let made = groups::create_group(
        peer_group_name,
        None,
        me.id.clone(),
        Some(true),
        Some(false),
        &state,
    )
    .await
    .context("create the peer's group")?;
    println!("PEER_CREATED_GROUP={}", made.id);

    // The wake-up is sent on a spawned task; give it time to leave before the
    // runtime goes away with this process.
    tokio::time::sleep(Duration::from_secs(10)).await;
    Ok(())
}
