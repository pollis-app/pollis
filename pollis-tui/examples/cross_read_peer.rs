//! Dev-DS peers for the mobile Maestro `cross-device-unread` flow (#1628).
//!
//! The phone under test is the account's SECOND device; this example plays the account's other
//! device and the person writing to it. Two roles, two processes (each its own data dir):
//!
//! * `owner <email>` — the account's FIRST device. Signs up, prints `PEER_RECOVERY_KEY=` (the
//!   phone enrolls with it), `PEER_USER_ID=`, then `PEER_SAW_PHONE=<device id>` once the phone
//!   has enrolled. Accepts the sender's DM request (`PEER_ACCEPTED dm=<id>`). On the script's
//!   `ingest-<n>` signal it fetches the DM — storing the sender's messages, which emits the
//!   delivered receipts that wake the phone (`PEER_INGESTED_<n>`); on `read-<n>` it marks the DM
//!   read, as a person opening it would, and waits out the debounced cursor push and push dismiss
//!   (`PEER_READ_<n>`).
//! * `sender <email> <owner-user-id>` — opens a DM with the owner and prints `PEER_SENDER_READY`.
//!   On `go-1` it sends two messages, on `go-2` one, printing `PEER_SENT_<n>`.
//!
//! Signals are files in `CROSS_READ_SIGNAL_DIR`. Not a shipping binary: examples build against
//! pollis-tui's dev-dependencies.
//!
//!   POLLIS_DATA_DIR=$(mktemp -d) POLLIS_DELIVERY_URL=https://api-dev.pollis.com \
//!   CROSS_READ_SIGNAL_DIR=$(mktemp -d) POLLIS_DEV_ACCESS_KEY=… \
//!   cargo run -p pollis-tui --example cross_read_peer -- owner <email>

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use pollis_core::commands::{auth, dm, messages};
use pollis_core::config::Config;
use pollis_core::state::AppState;

/// The dev DS's fixed OTP (`DEV_OTP`); override with `POLLIS_DEV_OTP`.
const DEFAULT_OTP: &str = "000000";
/// How long to wait for the phone, the other peer or the script at each step.
const WAIT: Duration = Duration::from_secs(600);
/// Past the core's 5 s read-cursor push debounce and 2 s dismiss debounce.
const AFTER_READ: Duration = Duration::from_secs(9);

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

fn signals() -> Result<PathBuf> {
    Ok(std::env::var("CROSS_READ_SIGNAL_DIR")
        .context("CROSS_READ_SIGNAL_DIR is required")?
        .into())
}

/// The first of `names` the script has created, waiting for one.
async fn next_signal(dir: &Path, names: &[String]) -> Result<String> {
    wait_for("the script's next signal", || async {
        Ok(names.iter().find(|n| dir.join(n.as_str()).exists()).cloned())
    })
    .await
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [role, email] if role == "owner" => owner(email).await,
        [role, email, owner_id] if role == "sender" => sender(email, owner_id).await,
        _ => bail!("usage: cross_read_peer owner <email> | sender <email> <owner-user-id>"),
    }
}

async fn owner(email: &str) -> Result<()> {
    let dir = signals()?;
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

    let dm_id = wait_for("the sender's DM request", || async {
        Ok(dm::list_dm_requests(me.clone(), &state).await?.into_iter().next().map(|r| r.id))
    })
    .await?;
    dm::accept_dm_request(dm_id.clone(), me.clone(), &state)
        .await
        .context("accept the DM")?;
    pollis_tui::data::dm_messages(&state, &me, &dm_id, None).await?;
    println!("PEER_ACCEPTED dm={dm_id}");

    let mut done: Vec<String> = Vec::new();
    loop {
        let wanted: Vec<String> = (1..=2)
            .flat_map(|n| [format!("ingest-{n}"), format!("read-{n}")])
            .filter(|s| !done.contains(s))
            .collect();
        if wanted.is_empty() {
            return Ok(());
        }
        let signal = next_signal(&dir, &wanted).await?;
        let (what, n) = signal.split_once('-').context("signal name")?;
        // Storing the sender's messages emits the delivered receipts that wake the phone.
        let page = pollis_tui::data::dm_messages(&state, &me, &dm_id, None).await?;
        if what == "read" {
            // As the desktop does with the messages on screen: a read receipt, which wakes the
            // phone with the same `new_message` wake a message does (the #1628 repro).
            let theirs: Vec<String> = page
                .messages
                .iter()
                .filter(|m| m.sender_id != me)
                .map(|m| m.id.clone())
                .collect();
            messages::mark_messages_read(dm_id.clone(), me.clone(), theirs, &state).await?;
            let moved = messages::mark_conversation_read(dm_id.clone(), me.clone(), &state).await?;
            tokio::time::sleep(AFTER_READ).await;
            println!("PEER_READ_{n} moved={moved}");
        } else {
            println!("PEER_INGESTED_{n} rows={}", page.messages.len());
        }
        done.push(signal);
    }
}

async fn sender(email: &str, owner_id: &str) -> Result<()> {
    let dir = signals()?;
    let (state, me, handle, _) = sign_up(email).await?;
    println!("PEER_SENDER_HANDLE={handle}");
    let made = dm::create_dm_channel(me.clone(), vec![me.clone(), owner_id.to_string()], &state)
        .await
        .context("open the DM")?;
    pollis_tui::data::dm_messages(&state, &me, &made.id, None).await?;
    // The script releases `go-1` only after the owner prints `PEER_ACCEPTED`.
    println!("PEER_SENDER_READY dm={}", made.id);

    let send = |text: &'static str| {
        let state = state.clone();
        let (me, handle, dm_id) = (me.clone(), handle.clone(), made.id.clone());
        async move {
            messages::send_message(dm_id, me, text.to_string(), None, None, Some(handle), &state)
                .await
                .with_context(|| format!("send {text:?}"))
        }
    };
    next_signal(&dir, &["go-1".to_string()]).await?;
    send("are you around?").await?;
    send("ping me when you see this").await?;
    println!("PEER_SENT_1");
    next_signal(&dir, &["go-2".to_string()]).await?;
    send("one more while your phone sleeps").await?;
    println!("PEER_SENT_2");
    Ok(())
}
