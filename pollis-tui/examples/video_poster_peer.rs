//! Dev-DS peer for the mobile Maestro `video-posters` two-client flow (#1837).
//!
//! Shows a video's poster crossing clients both ways, and an older sender's
//! posterless video still getting a frame. This peer signs up a fresh account
//! on the DS named by `POLLIS_DELIVERY_URL` (the dev DS — it fixes the OTP),
//! prints its handle, waits for the phone under test to open a DM with it,
//! then sends two videos through the real outbox (upload to R2 included):
//! one with a poster frame, as a current client sends it, and one without,
//! as a build from before #1837 did. It prints `PEER_SENT`, then waits for
//! the phone to send a video of its own and checks that envelope carries a
//! `pv` poster that is a JPEG (`PEER_SAW_POSTER`).
//! `mobile/scripts/maestro-video-posters.sh` drives both sides.
//!
//! Not a shipping binary: examples build against pollis-tui's
//! dev-dependencies.
//!
//!   POLLIS_DATA_DIR=$(mktemp -d) POLLIS_DELIVERY_URL=https://api-dev.pollis.com \
//!   POLLIS_DEV_ACCESS_KEY=… R2_S3_ENDPOINT=… R2_PUBLIC_URL=… \
//!   cargo run -p pollis-tui --example video_poster_peer -- <email> <video.mp4> <frame.jpg>

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use base64::Engine as _;
use pollis_core::commands::{dm, messages};
use pollis_core::config::Config;
use pollis_core::state::AppState;

/// The dev DS's fixed OTP (`DEV_OTP`); override with `POLLIS_DEV_OTP`.
const DEFAULT_OTP: &str = "000000";
/// How long to wait for the phone at each step.
const WAIT: Duration = Duration::from_secs(600);

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: video_poster_peer <email> <video.mp4> <frame.jpg>";
    let email = args.next().context(usage)?;
    let video = args.next().context(usage)?;
    let frame = std::fs::read(args.next().context(usage)?).context("read the frame")?;
    let otp = std::env::var("POLLIS_DEV_OTP").unwrap_or_else(|_| DEFAULT_OTP.to_string());
    let config = Config::from_env().context("POLLIS_DELIVERY_URL (the dev DS) and the R2 origins are required")?;
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
    let seen = pollis_tui::data::dm_messages(&state, &me.id, &dm_id, None).await?;
    let before: Vec<String> = seen.messages.iter().map(|m| m.id.clone()).collect();

    let clip = |name: &str, poster: Option<String>| -> Result<messages::OutgoingAttachment> {
        Ok(serde_json::from_value(serde_json::json!({
            "path": video,
            "filename": name,
            "contentType": "video/mp4",
            "poster": poster,
        }))?)
    };
    let frame_b64 = base64::engine::general_purpose::STANDARD.encode(&frame);
    for (caption, att) in [
        ("video with poster", clip("with-poster.mp4", Some(frame_b64))?),
        // As a client from before #1837 sent it: no frame, so no `pv`.
        ("video from an older sender", clip("old-sender.mp4", None)?),
    ] {
        messages::send_message_with_id(
            None,
            dm_id.clone(),
            me.id.clone(),
            caption.to_string(),
            vec![att],
            None,
            None,
            Some(me.username.clone()),
            &state,
        )
        .await
        .with_context(|| format!("send {caption:?}"))?;
    }
    println!("PEER_SENT dm={dm_id}");

    // The phone's own video: its envelope must carry a JPEG `pv`.
    let started = Instant::now();
    loop {
        let page = pollis_tui::data::dm_messages(&state, &me.id, &dm_id, None).await?;
        for m in page.messages.iter().filter(|m| m.sender_id != me.id && !before.contains(&m.id)) {
            let Some(content) = m.content.as_deref() else {
                continue;
            };
            let Ok(env) = serde_json::from_str::<serde_json::Value>(content) else {
                continue;
            };
            let Some(att) = env["_att"].as_array().and_then(|a| a.first()) else {
                continue;
            };
            if !att["ct"].as_str().unwrap_or_default().starts_with("video/") || att["key"].is_null() {
                continue;
            }
            let pv = att["pv"].as_str().context("the phone's video has no `pv` poster")?;
            let jpeg = base64::engine::general_purpose::STANDARD.decode(pv).context("pv is not base64")?;
            if !(jpeg.starts_with(&[0xFF, 0xD8]) && jpeg.ends_with(&[0xFF, 0xD9])) {
                bail!("pv is not a JPEG");
            }
            println!(
                "PEER_SAW_POSTER bytes={} bh={} w={} h={}",
                jpeg.len(),
                att["bh"].as_str().unwrap_or("-"),
                att["w"],
                att["h"]
            );
            return Ok(());
        }
        if started.elapsed() > WAIT {
            bail!("the phone's video never arrived");
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}
