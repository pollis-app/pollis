//! Dev-DS peer for the mobile Maestro `shared-file` two-client flow (#2109).
//!
//! Two accounts send the SAME file. This peer signs up on the DS named by
//! `POLLIS_DELIVERY_URL` (the dev DS — it fixes the OTP), prints its handle,
//! accepts the DM the phone opens, then waits for the phone's photo. It
//! downloads and opens it (proving the phone's upload decrypts on another
//! client, in whichever format the DS's switch chose), and sends those exact
//! bytes back through the real outbox. It reports both references:
//!
//! - switch on (`v: 2`): the two messages must name the SAME stored object;
//! - switch off (`kv`, v1): two objects, each opening for the other side.
//!
//! `PEER_ECHO format=<v1|v2> same_object=<bool>`. The phone then draws the
//! echo. `mobile/scripts/maestro-shared-file.sh` drives both sides.
//!
//! Not a shipping binary: examples build against pollis-tui's dev-dependencies.
//!
//!   POLLIS_DATA_DIR=$(mktemp -d) POLLIS_DELIVERY_URL=https://api-dev.pollis.com \
//!   POLLIS_DEV_ACCESS_KEY=… R2_S3_ENDPOINT=… R2_PUBLIC_URL=… \
//!   cargo run -p pollis-tui --example shared_file_peer -- <email>

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use pollis_core::commands::r2::{download_media, AttachmentKeyScope};
use pollis_core::commands::{dm, messages};
use pollis_core::config::Config;
use pollis_core::state::AppState;

/// The dev DS's fixed OTP (`DEV_OTP`); override with `POLLIS_DEV_OTP`.
const DEFAULT_OTP: &str = "000000";
/// How long to wait for the phone at each step.
const WAIT: Duration = Duration::from_secs(600);

/// The newest `_att` entry with an uploaded image in `messages` from someone else.
fn image_att(page: &[pollis_core::commands::messages::ChannelMessage], me: &str) -> Option<serde_json::Value> {
    page.iter().filter(|m| m.sender_id != me).find_map(|m| {
        let env: serde_json::Value = serde_json::from_str(m.content.as_deref()?).ok()?;
        let att = env["_att"].as_array()?.first()?.clone();
        (att["ct"].as_str()?.starts_with("image/") && !att["key"].is_null()).then_some(att)
    })
}

fn scope_of(att: &serde_json::Value, dm_id: &str) -> Result<AttachmentKeyScope> {
    if att["v"].as_u64() == Some(2) {
        return Ok(AttachmentKeyScope::Content);
    }
    let kv = att["kv"].as_i64().context("a v1 reference without kv")?;
    Ok(AttachmentKeyScope::Conversation { conversation_id: dm_id.to_string(), key_version: kv })
}

async fn open(state: &Arc<AppState>, att: &serde_json::Value, dm_id: &str) -> Result<Vec<u8>> {
    Ok(download_media(
        att["key"].as_str().context("key")?.to_string(),
        att["hash"].as_str().context("hash")?.to_string(),
        att["sha"].as_str().context("sha")?.to_string(),
        scope_of(att, dm_id)?,
        state,
    )
    .await?)
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let email = std::env::args().nth(1).context("usage: shared_file_peer <email>")?;
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

    // The phone's photo: open it here.
    let started = Instant::now();
    let phone_att = loop {
        let page = pollis_tui::data::dm_messages(&state, &me.id, &dm_id, None).await?;
        if let Some(att) = image_att(&page.messages, &me.id) {
            break att;
        }
        if started.elapsed() > WAIT {
            bail!("the phone's photo never arrived");
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    };
    let bytes = open(&state, &phone_att, &dm_id).await.context("open the phone's photo")?;
    println!("PEER_OPENED bytes={} format={}", bytes.len(), if phone_att["v"].as_u64() == Some(2) { "v2" } else { "v1" });

    // Send those exact bytes back.
    let staged = pollis_core::commands::staging::stage_attachment(bytes.clone())?.id;
    let att: messages::OutgoingAttachment = serde_json::from_value(serde_json::json!({
        "stagedId": staged,
        "filename": phone_att["name"].as_str().unwrap_or("photo.png"),
        "contentType": phone_att["ct"].as_str().unwrap_or("image/png"),
    }))?;
    let sent = messages::send_message_with_id(
        None,
        dm_id.clone(),
        me.id.clone(),
        "echo of your photo".to_string(),
        vec![att],
        None,
        None,
        Some(me.username.clone()),
        &state,
    )
    .await
    .context("send the echo")?;
    let mine: serde_json::Value = serde_json::from_str(sent.content.as_deref().context("content")?)?;
    let my_att = mine["_att"][0].clone();
    // The echo opens too, from the other side's point of view of the same DM.
    let echoed = open(&state, &my_att, &dm_id).await.context("open the echo")?;
    if echoed != bytes {
        bail!("the echo does not open to the same bytes");
    }

    let v2 = phone_att["v"].as_u64() == Some(2) && my_att["v"].as_u64() == Some(2);
    let same_object = phone_att["hash"] == my_att["hash"];
    if v2 && !same_object {
        bail!("both v2, yet two stored objects: {} vs {}", phone_att["hash"], my_att["hash"]);
    }
    if !v2 && same_object {
        bail!("a v1 upload reused another's object id");
    }
    println!("PEER_ECHO format={} same_object={same_object}", if v2 { "v2" } else { "v1" });
    Ok(())
}
