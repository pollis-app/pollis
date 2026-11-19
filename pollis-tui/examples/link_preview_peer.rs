//! Dev-DS peer for the mobile Maestro `link-previews` two-client flow (#1987).
//!
//! The other side of a cross-platform check: the same `pollis-core` the
//! desktop app runs. This peer signs up a fresh account on the DS named by
//! `POLLIS_DELIVERY_URL` (the dev DS, which fixes the OTP), prints its handle,
//! accepts the DM the phone under test opens, then builds a preview of
//! `https://github.com/` itself (a real fetch from this machine, exactly what
//! the desktop composer asks the core for), sends it and prints `PEER_SENT`.
//! It then waits for the phone's own message with a preview and prints
//! `PEER_SAW_DEVICE_PREVIEW title=…`.
//!
//! Then edit and delete, both ways, against the live DS. The peer edits its
//! message to another link with a fresh preview (`PEER_EDITED`); the edit
//! releases the replaced thumbnail through `/v1/attachments/release`, so its
//! download must start failing (`PEER_RELEASED_OLD_THUMB`). Once the file
//! named by `POLLIS_PEER_GO_DELETE` exists it deletes the message for everyone
//! (`PEER_DELETED`), which takes the new thumbnail too
//! (`PEER_RELEASED_NEW_THUMB`). Last it waits for the phone to edit its own
//! message to a new link (`PEER_SAW_DEVICE_EDIT`) and to delete it
//! (`PEER_SAW_DEVICE_DELETE`). `mobile/scripts/maestro-link-previews.sh`
//! drives both sides.
//!
//! Not a shipping binary: examples build against pollis-tui's dev-dependencies.
//!
//!   POLLIS_DATA_DIR=$(mktemp -d) POLLIS_DELIVERY_URL=https://api-dev.pollis.com \
//!   POLLIS_DEV_ACCESS_KEY=… cargo run -p pollis-tui --example link_preview_peer -- <email>

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use pollis_core::commands::{dm, link_preview, messages, r2};
use pollis_core::commands::messages::ChannelMessage;
use pollis_core::config::Config;
use pollis_core::state::AppState;

/// The dev DS's fixed OTP (`DEV_OTP`); override with `POLLIS_DEV_OTP`.
const DEFAULT_OTP: &str = "000000";
/// How long to wait for the phone at each step.
const WAIT: Duration = Duration::from_secs(600);
/// The link both sides preview.
const LINK: &str = "https://github.com/";
/// The link both sides edit their message to.
const EDIT_LINK: &str = "https://www.rust-lang.org/";
/// How long a released thumbnail may take to stop downloading.
const RECLAIM_WAIT: Duration = Duration::from_secs(120);

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
        // A dropped connection to the dev DS is retried on the next poll, not fatal.
        match dm::list_dm_requests(me.id.clone(), &state).await {
            Ok(reqs) => {
                if let Some(req) = reqs.into_iter().next() {
                    dm::accept_dm_request(req.id.clone(), me.id.clone(), &state).await?;
                    break req.id;
                }
            }
            Err(e) => eprintln!("list_dm_requests: {e}"),
        }
        if started.elapsed() > WAIT {
            bail!("no DM request arrived within {WAIT:?}");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    };
    if let Err(e) = pollis_tui::data::dm_messages(&state, &me.id, &dm_id, None).await {
        eprintln!("dm_messages: {e}");
    }

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

    let device_msg = wait_for(&state, &me.id, &dm_id, "the phone's message with a preview", |m| {
        m.sender_id != me.id
            && m.content.as_deref().is_some_and(|c| c.to_lowercase().contains("from the phone"))
            && m.link_preview.is_some()
    })
    .await?;
    let p = device_msg.link_preview.as_ref().expect("checked");
    println!("PEER_SAW_DEVICE_PREVIEW title={:?} image={}", p.title, p.image.is_some());

    // Edit: a new link and its own preview replace the old ones, and the old thumbnail is released.
    let mine = wait_for(&state, &me.id, &dm_id, "our own message", |m| {
        m.sender_id == me.id && m.content.as_deref().is_some_and(|c| c.contains("from the desktop core"))
    })
    .await?;
    let old_key = thumb_key(&mine).context("our sent message has no thumbnail")?;
    r2::download_file(old_key.clone(), &state)
        .await
        .context("the sent thumbnail does not download")?;
    println!("PEER_THUMB_LIVE");
    let edit_draft = link_preview::fetch_link_preview(EDIT_LINK.to_string(), &state)
        .await
        .context("fetch_link_preview (edit)")?
        .context("no preview of the edit link was built")?;
    messages::edit_message_with_preview(
        dm_id.clone(),
        mine.id.clone(),
        me.id.clone(),
        format!("edited by the desktop core {EDIT_LINK}"),
        Some(edit_draft),
        &state,
    )
    .await
    .context("edit with preview")?;
    let edited = wait_for(&state, &me.id, &dm_id, "our edit to settle", |m| {
        m.id == mine.id
            && m.edit_state.is_none()
            && m.link_preview.as_ref().is_some_and(|p| p.url == EDIT_LINK)
    })
    .await?;
    let new_key = thumb_key(&edited).context("the edited message has no thumbnail")?;
    println!(
        "PEER_EDITED title={:?} image={}",
        edited.link_preview.as_ref().map(|p| p.title.clone()),
        new_key != old_key
    );
    println!("PEER_RELEASED_OLD_THUMB {}", wait_gone(&state, &old_key).await?);

    // Delete for everyone, once the phone has seen the edit.
    let go = std::env::var("POLLIS_PEER_GO_DELETE").context("POLLIS_PEER_GO_DELETE is required")?;
    let started = Instant::now();
    while !std::path::Path::new(&go).exists() {
        if started.elapsed() > WAIT {
            bail!("never told to delete");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    messages::delete_message(mine.id.clone(), me.id.clone(), &state)
        .await
        .context("delete for everyone")?;
    println!("PEER_DELETED");
    println!("PEER_RELEASED_NEW_THUMB {}", wait_gone(&state, &new_key).await?);

    // The phone edits its message to the edit link, then deletes it.
    let device_edit = wait_for(&state, &me.id, &dm_id, "the phone's edit", |m| {
        m.id == device_msg.id
            && m.link_preview.as_ref().is_some_and(|p| p.url.to_lowercase().contains("rust-lang.org"))
    })
    .await?;
    let p = device_edit.link_preview.as_ref().expect("checked");
    println!("PEER_SAW_DEVICE_EDIT title={:?} image={}", p.title, p.image.is_some());
    let device_delete = wait_for(&state, &me.id, &dm_id, "the phone's delete", |m| {
        m.id == device_msg.id && (m.deleted_at.is_some() || m.content.is_none())
    })
    .await?;
    println!("PEER_SAW_DEVICE_DELETE preview={}", device_delete.link_preview.is_some());
    Ok(())
}

/// The R2 key of `m`'s preview thumbnail, if it has one.
fn thumb_key(m: &ChannelMessage) -> Option<String> {
    m.link_preview.as_ref()?.image.as_ref().map(|i| i.key.clone())
}

/// Poll the DM until a message matches `pred`; the dev DS's dropped connections are retried.
async fn wait_for(
    state: &Arc<AppState>,
    me: &str,
    dm_id: &str,
    what: &str,
    pred: impl Fn(&ChannelMessage) -> bool,
) -> Result<ChannelMessage> {
    let started = Instant::now();
    loop {
        match pollis_tui::data::dm_messages(state, me, dm_id, None).await {
            Ok(page) => {
                if let Some(m) = page.messages.into_iter().find(|m| pred(m)) {
                    return Ok(m);
                }
            }
            Err(e) => eprintln!("dm_messages: {e}"),
        }
        if started.elapsed() > WAIT {
            bail!("{what} never arrived");
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

/// Wait until `key` no longer downloads (released, then reclaimed by the DS); returns the error seen.
/// Two failures in a row, so one dropped connection to the dev DS is not taken for a reclaim.
async fn wait_gone(state: &Arc<AppState>, key: &str) -> Result<String> {
    let started = Instant::now();
    let mut failed_once = false;
    loop {
        match r2::download_file(key.to_string(), state).await {
            Err(e) if failed_once => return Ok(format!("after {:?}: {e}", started.elapsed())),
            Err(_) => failed_once = true,
            Ok(_) => failed_once = false,
        }
        if started.elapsed() > RECLAIM_WAIT {
            bail!("thumbnail {key} still downloads {RECLAIM_WAIT:?} after its release");
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}
