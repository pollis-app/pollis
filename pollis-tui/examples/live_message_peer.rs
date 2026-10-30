//! Dev-DS peer for the mobile Maestro `live-message` flow (#1610).
//!
//! Signs up a fresh account on the DS named by `POLLIS_DELIVERY_URL` (the dev DS — it fixes the
//! OTP), prints `PEER_HANDLE=<handle>` and `PEER_PORT=<port>`, accepts the first DM request (the
//! phone under test opens one) and prints `PEER_READY`. From then on it sends into that DM when a
//! flow asks over loopback HTTP, so a Maestro step can send and then wait on the screen in one go:
//!
//!   GET http://127.0.0.1:<port>/send?text=<url-encoded text>   → 200 `<epoch ms when stored>`
//!
//! The response comes back once the DS has the message, so the flow's wait that follows is
//! the time from the DS having it to the phone showing it. `GET /quit` ends the peer.
//!
//! Not a shipping binary: examples build against pollis-tui's dev-dependencies.
//!
//!   POLLIS_DATA_DIR=$(mktemp -d) POLLIS_DELIVERY_URL=https://api-dev.pollis.com \
//!   POLLIS_DEV_ACCESS_KEY=… cargo run -p pollis-tui --example live_message_peer -- <email>

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use pollis_core::commands::{dm, messages};
use pollis_core::config::Config;
use pollis_core::state::AppState;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// The dev DS's fixed OTP (`DEV_OTP`); override with `POLLIS_DEV_OTP`.
const DEFAULT_OTP: &str = "000000";
/// How long to wait for the phone's DM request.
const WAIT: Duration = Duration::from_secs(300);
/// The peer exits on its own after this long without a request.
const IDLE: Duration = Duration::from_secs(1800);

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// `%XX` and `+` decoding for the one query parameter.
fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let email = std::env::args().nth(1).context("usage: live_message_peer <email>")?;
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
    let listener = TcpListener::bind("127.0.0.1:0").await.context("bind")?;
    println!("PEER_HANDLE={}", me.username);
    println!("PEER_PORT={}", listener.local_addr()?.port());

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
    println!("PEER_READY dm={dm_id}");

    loop {
        let (mut sock, _) = match tokio::time::timeout(IDLE, listener.accept()).await {
            Ok(accepted) => accepted?,
            Err(_) => bail!("idle for {IDLE:?}"),
        };
        let mut buf = vec![0u8; 8192];
        let n = sock.read(&mut buf).await.unwrap_or(0);
        let request = String::from_utf8_lossy(&buf[..n]).into_owned();
        let target = request.split_whitespace().nth(1).unwrap_or("/").to_string();
        let (status, body, quit) = if target == "/quit" {
            ("200 OK", "bye".to_string(), true)
        } else if let Some(query) = target.strip_prefix("/send?text=") {
            let text = url_decode(query.split('&').next().unwrap_or(""));
            match messages::send_message(
                dm_id.clone(),
                me.id.clone(),
                text.clone(),
                None,
                None,
                Some(me.username.clone()),
                &state,
            )
            .await
            {
                Ok(m) => {
                    let at = now_ms();
                    println!("SENT id={} at={at}", m.id);
                    ("200 OK", at.to_string(), false)
                }
                Err(e) => ("500 Internal Server Error", format!("send failed: {e}"), false),
            }
        } else {
            ("404 Not Found", "unknown".to_string(), false)
        };
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = sock.write_all(response.as_bytes()).await;
        let _ = sock.shutdown().await;
        if quit {
            return Ok(());
        }
    }
}
