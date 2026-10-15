//! Dev-DS peer that sends one of every message shape the other clients
//! produce (#1337), for a hands-on check of the terminal client against the
//! dev Delivery Service.
//!
//! Signs up a fresh account, prints its handle, waits for a DM request (start
//! one from the client under test with `d` + the handle), accepts it, then
//! sends: a multi-line message, a long one, a captioned photo (the `_att`
//! envelope desktop and mobile write), a mention of the requester, a thread
//! reply and a quote reply to the first message, and a reaction on it. Prints
//! `PEER_SENT` when done and exits.
//!
//! Not a shipping binary: examples build against pollis-tui's
//! dev-dependencies.
//!
//!   POLLIS_DATA_DIR=$(mktemp -d) POLLIS_DELIVERY_URL=https://api-dev.pollis.com \
//!   POLLIS_DEV_ACCESS_KEY=… cargo run -p pollis-tui --example message_shapes_peer -- <email>

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use pollis_core::commands::{dm, messages};
use pollis_core::config::Config;
use pollis_core::state::AppState;

/// The dev DS's fixed OTP (`DEV_OTP`); override with `POLLIS_DEV_OTP`.
const DEFAULT_OTP: &str = "000000";
/// How long to wait for the DM request.
const WAIT: Duration = Duration::from_secs(600);

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let email = std::env::args()
        .nth(1)
        .context("usage: message_shapes_peer <email>")?;
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
    let request = loop {
        if let Some(req) = dm::list_dm_requests(me.id.clone(), &state)
            .await?
            .into_iter()
            .next()
        {
            break req;
        }
        if started.elapsed() > WAIT {
            bail!("no DM request arrived within {WAIT:?}");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    };
    let dm_id = request.id.clone();
    let requester = request
        .members
        .iter()
        .find(|m| m.user_id != me.id)
        .and_then(|m| m.username.clone())
        .unwrap_or_default();
    pollis_tui::send::accept_dm(&state, &me.id, &dm_id).await?;
    // Join the DM's MLS group before sending into it.
    pollis_tui::data::dm_messages(&state, &me.id, &dm_id, None).await?;

    let send = |text: String, reply_to: Option<String>, thread: Option<String>| {
        let state = state.clone();
        let me_id = me.id.clone();
        let me_name = me.username.clone();
        let dm_id = dm_id.clone();
        async move {
            pollis_tui::send::send_text_with(
                &state,
                &me_id,
                Some(me_name),
                &dm_id,
                &text,
                reply_to,
                thread,
            )
            .await
            .map(|m| m.id)
        }
    };
    let root = send("multi\nline\nmessage".into(), None, None).await?;
    send(
        format!("long: {}— the end", "lorem ipsum dolor sit amet ".repeat(10)),
        None,
        None,
    )
    .await?;
    send(
        r#"{"_att":[{"key":"media/0000","hash":"0000","name":"photo.png","ct":"image/png","size":2048,"w":1,"h":1}],"_txt":"a captioned photo"}"#.into(),
        None,
        None,
    )
    .await?;
    send(format!("hey @{requester}, this mentions you"), None, None).await?;
    send("a reply inside the thread".into(), None, Some(root.clone())).await?;
    send("a quote reply to the first message".into(), Some(root.clone()), None).await?;
    messages::add_reaction(root.clone(), me.id.clone(), "🎉".into(), &state).await?;
    println!("PEER_SENT dm={dm_id}");
    Ok(())
}
