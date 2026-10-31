//! Dev-DS peer for the mobile Maestro `typing` flow (#1635).
//!
//! A desktop-shaped typist: it joins the DM's LiveKit room through pollis-core's real realtime
//! path (the `media` feature, so the `typing-peer` feature of this crate), publishes `typing` the
//! way desktop's `publish_typing` does, and records the typing signals the phone sends.
//!
//! Signs up a fresh account on the dev DS, prints `PEER_HANDLE=<handle>` and
//! `PEER_PORT=<port>`, accepts the first DM request (the phone under test opens one), joins the
//! room and prints `PEER_READY`. Then, over loopback HTTP:
//!
//!   GET /typing?on=1   start typing: published now and re-sent every 3 s, as desktop does
//!   GET /typing?on=0   stop: one `is_typing: false`
//!   GET /seen          the phone's last typing state in this DM: `typing`, `stopped` or `none`
//!   GET /quit
//!
//! Not a shipping binary:
//!
//!   POLLIS_DATA_DIR=$(mktemp -d) POLLIS_DELIVERY_URL=https://api-dev.pollis.com \
//!   POLLIS_DEV_ACCESS_KEY=… cargo run -p pollis-tui --features typing-peer \
//!     --example typing_peer -- <email>

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use pollis_core::commands::{dm, livekit};
use pollis_core::config::Config;
use pollis_core::realtime::RealtimeEvent;
use pollis_core::sink::EventSink;
use pollis_core::state::AppState;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// The dev DS's fixed OTP (`DEV_OTP`); override with `POLLIS_DEV_OTP`.
const DEFAULT_OTP: &str = "000000";
/// How long to wait for the phone's DM request, and for the room to connect.
const WAIT: Duration = Duration::from_secs(300);
/// The peer exits on its own after this long without a request.
const IDLE: Duration = Duration::from_secs(1800);
/// Desktop's `TYPING_REFRESH_MS`.
const REFRESH: Duration = Duration::from_secs(3);

/// What the phone last said about typing in the DM.
struct Seen {
    me: String,
    dm_id: Mutex<Option<String>>,
    state: Mutex<&'static str>,
}

impl EventSink<RealtimeEvent> for Seen {
    fn send(&self, event: RealtimeEvent) -> Result<(), String> {
        if let RealtimeEvent::Typing { conversation_id, user_id, username, is_typing, .. } = event {
            let dm = self.dm_id.lock().unwrap().clone();
            if user_id != self.me && conversation_id.is_some() && conversation_id == dm {
                println!("SEEN typing={is_typing} from={}", username.unwrap_or(user_id));
                *self.state.lock().unwrap() = if is_typing { "typing" } else { "stopped" };
            }
        }
        Ok(())
    }
}

async fn publish(state: &Arc<AppState>, dm_id: &str, me: &str, on: bool) {
    let _ = livekit::publish_typing(
        dm_id.to_string(),
        None,
        Some(dm_id.to_string()),
        me.to_string(),
        None,
        on,
        state,
    )
    .await;
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let email = std::env::args().nth(1).context("usage: typing_peer <email>")?;
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

    let seen = Arc::new(Seen {
        me: me.id.clone(),
        dm_id: Mutex::new(None),
        state: Mutex::new("none"),
    });
    livekit::subscribe_realtime(seen.clone(), &state).await?;

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
    *seen.dm_id.lock().unwrap() = Some(dm_id.clone());
    pollis_tui::data::dm_messages(&state, &me.id, &dm_id, None).await?;

    livekit::connect_rooms(vec![dm_id.clone()], me.id.clone(), me.username.clone(), &state).await?;
    let started = Instant::now();
    while !state.livekit.lock().await.rooms.contains_key(&dm_id) {
        if started.elapsed() > WAIT {
            bail!("the DM's LiveKit room never connected");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    println!("PEER_READY dm={dm_id}");

    let mut typing: Option<JoinHandle<()>> = None;
    loop {
        let (mut sock, _) = match tokio::time::timeout(IDLE, listener.accept()).await {
            Ok(accepted) => accepted?,
            Err(_) => bail!("idle for {IDLE:?}"),
        };
        let mut buf = vec![0u8; 4096];
        let n = sock.read(&mut buf).await.unwrap_or(0);
        let request = String::from_utf8_lossy(&buf[..n]).into_owned();
        let target = request.split_whitespace().nth(1).unwrap_or("/").to_string();
        let (status, body, quit) = match target.as_str() {
            "/quit" => ("200 OK", "bye".to_string(), true),
            "/seen" => ("200 OK", seen.state.lock().unwrap().to_string(), false),
            "/typing?on=1" => {
                if let Some(t) = typing.take() {
                    t.abort();
                }
                let (st, dm, uid) = (state.clone(), dm_id.clone(), me.id.clone());
                typing = Some(tokio::spawn(async move {
                    loop {
                        publish(&st, &dm, &uid, true).await;
                        tokio::time::sleep(REFRESH).await;
                    }
                }));
                println!("TYPING on");
                ("200 OK", "on".to_string(), false)
            }
            "/typing?on=0" => {
                if let Some(t) = typing.take() {
                    t.abort();
                }
                publish(&state, &dm_id, &me.id, false).await;
                println!("TYPING off");
                ("200 OK", "off".to_string(), false)
            }
            _ => ("404 Not Found", "unknown".to_string(), false),
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
