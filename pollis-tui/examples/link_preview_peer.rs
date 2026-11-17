//! Dev-DS peer for the mobile Maestro `link-previews` two-client flow (#1987).
//!
//! The other side of a cross-platform check: the same `pollis-core` the
//! desktop app runs. This peer signs up a fresh account on the DS named by
//! `POLLIS_DELIVERY_URL` (the dev DS, which fixes the OTP), prints its handle,
//! accepts the DM the phone under test opens, then builds a preview of
//! `https://github.com/` itself (a real fetch from this machine, exactly what
//! the desktop composer asks the core for), sends it and prints `PEER_SENT`.
//! It then waits for the phone's own message with a preview and prints
//! `PEER_SAW_DEVICE_PREVIEW title=…`. `mobile/scripts/maestro-link-previews.sh`
//! drives both sides.
//!
//! Not a shipping binary: examples build against pollis-tui's dev-dependencies.
//!
//!   POLLIS_DATA_DIR=$(mktemp -d) POLLIS_DELIVERY_URL=https://api-dev.pollis.com \
//!   POLLIS_DEV_ACCESS_KEY=… cargo run -p pollis-tui --example link_preview_peer -- <email>

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use pollis_core::commands::{dm, link_preview, messages};
use pollis_core::config::Config;
use pollis_core::state::AppState;

/// The dev DS's fixed OTP (`DEV_OTP`); override with `POLLIS_DEV_OTP`.
const DEFAULT_OTP: &str = "000000";
/// How long to wait for the phone at each step.
const WAIT: Duration = Duration::from_secs(600);
/// The link both sides preview.
const LINK: &str = "https://github.com/";

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let email = std::env::args().nth(1).context("usage: link_preview_peer <email>")?;
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

    let draft = link_preview::fetch_link_preview(LINK.to_string(), &state)
        .await
        .context("fetch_link_preview")?
        .context("no preview of github.com was built")?;
    println!(
        "PEER_BUILT title={:?} image={}",
        draft.title,
        draft.image.is_some()
    );
    let content = format!("from the desktop core {LINK}");
    messages::send_message_with_preview(
        None,
        dm_id.clone(),
        me.id.clone(),
        content,
        Vec::new(),
        Some(draft),
        None,
        None,
        Some(me.username.clone()),
        &state,
    )
    .await
    .context("send with preview")?;
    println!("PEER_SENT dm={dm_id}");

    let started = Instant::now();
    loop {
        if let Ok(page) = pollis_tui::data::dm_messages(&state, &me.id, &dm_id, None).await {
            let seen = page.messages.iter().find(|m| {
                m.sender_id != me.id
                    && m.content.as_deref().is_some_and(|c| c.to_lowercase().contains("from the phone"))
                    && m.link_preview.is_some()
            });
            if let Some(m) = seen {
                let p = m.link_preview.as_ref().expect("checked");
                println!(
                    "PEER_SAW_DEVICE_PREVIEW title={:?} image={}",
                    p.title,
                    p.image.is_some()
                );
                return Ok(());
            }
        }
        if started.elapsed() > WAIT {
            bail!("the phone's message with a preview never arrived");
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}
