//! Dev-DS peer for the mobile Maestro device-approval flows (#1360).
//!
//! Plays the DESKTOP half of device linking with the desktop's own commands:
//!
//! - `new <email>` — a new device of an existing account (`PEER_CODE`, `PEER_ENROLLED`).
//! - `approver <email>` — the FIRST device; approves the code read from
//!   `$POLLIS_PEER_CODE_FILE` (`PEER_APPROVED`).
//! - `offer -` (#1361) — a signed-out desktop showing its QR (`PEER_OFFER`).
//! - `show <email>` (#1207) — a signed-in desktop showing a v1 code (`PEER_LINK`).
//!
//! History transfer (#1735), flags after the email:
//! - `--seed-history N` (approver) — before waiting for the phone, create "History Test Group"
//!   and post `history-seed-01`…`history-seed-NN` in its #General (`PEER_SEEDED=N`).
//! - `--transfer-source` (approver) — after approving the phone, approve the first history
//!   request that arrives (with the peer's PIN when it is required) and send it to the end
//!   (`PEER_TRANSFER_APPROVED=<id>`, `PEER_TRANSFER_DONE=<state> rows=<n>`).
//! - `--transfer-receiver` (new, offer) — once enrolled, ask the device that approved this one for
//!   its history and receive it to the end (`PEER_TRANSFER_REQUESTED=<id>`, `PEER_TRANSFER_DONE=…`).
//! - `--transfer-via link|sibling` — the request's `via` (default `sibling`; `link` lets a device that
//!   just linked this one by QR approve with one tap).
//! - `--seed-pad BYTES` — pad each seeded message to about BYTES, so the copy spans many chunks.
//!
//! Built with `--no-default-features` so it never raises macOS Keychain prompts.
//!
//!   POLLIS_DATA_DIR=$(mktemp -d) POLLIS_DELIVERY_URL=https://api-dev.pollis.com \
//!   POLLIS_DEV_ACCESS_KEY=… cargo run -p pollis-core --no-default-features \
//!     --example enroll_peer -- new <email>

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use pollis_core::commands::{auth, device_enrollment, device_link, groups, history_transfer, messages, pin};
use pollis_core::config::Config;
use pollis_core::state::AppState;

/// The dev DS's fixed OTP (`DEV_OTP`); override with `POLLIS_DEV_OTP`.
const DEFAULT_OTP: &str = "000000";
/// How long to wait for the phone at each step.
const WAIT: Duration = Duration::from_secs(600);
/// The peer's own PIN; it never has to be typed anywhere.
const PEER_PIN: &str = "0000";

#[derive(Default)]
struct Opts {
    seed: u32,
    pad: usize,
    via: Option<String>,
    transfer_source: bool,
    transfer_receiver: bool,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().context("usage: enroll_peer <new|approver|offer|show> <email> [flags]")?;
    let email = args.next().context("usage: enroll_peer <new|approver|offer|show> <email> [flags]")?;
    let mut opts = Opts::default();
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--seed-history" => {
                opts.seed = args.next().context("--seed-history N")?.parse().context("--seed-history N")?;
            }
            "--transfer-source" => opts.transfer_source = true,
            "--transfer-receiver" => opts.transfer_receiver = true,
            "--transfer-via" => {
                let via = args.next().context("--transfer-via link|sibling")?;
                if via != "link" && via != "sibling" {
                    bail!("--transfer-via link|sibling");
                }
                opts.via = Some(via);
            }
            "--seed-pad" => {
                opts.pad = args.next().context("--seed-pad BYTES")?.parse().context("--seed-pad BYTES")?;
            }
            other => bail!("unknown flag {other:?}"),
        }
    }
    let otp = std::env::var("POLLIS_DEV_OTP").unwrap_or_else(|_| DEFAULT_OTP.to_string());
    let config = Config::from_env().context("POLLIS_DELIVERY_URL (the dev DS) is required")?;
    let ds = config.pollis_delivery_url.clone().unwrap_or_default();
    if !ds.contains("api-dev") && !ds.contains("localhost") {
        bail!("refusing to run against {ds:?} — dev DS only");
    }
    let state = Arc::new(AppState::new(config).await.context("AppState")?);

    // A new desktop at "Sign in with another device": no email at all.
    if mode == "offer" {
        offering_device(&state).await?;
        if opts.transfer_receiver {
            transfer_receiver(&state, &opts).await?;
        }
        return Ok(());
    }
    auth::request_otp(&state, email.clone()).await?;
    let me = auth::verify_otp(&state, email, otp).await?;
    match mode.as_str() {
        "new" => {
            new_device(&state, &me.id, me.enrollment_required).await?;
            if opts.transfer_receiver {
                transfer_receiver(&state, &opts).await?;
            }
            Ok(())
        }
        "approver" => approver(&state, &me.id, &me.username, me.new_secret_key.is_some(), &opts).await,
        "show" => showing_device(&state, &me.id, me.new_secret_key.is_some()).await,
        other => bail!("unknown mode {other:?}"),
    }
}

/// `offer -`: reprints `PEER_OFFER` with a fresh code whenever one lapses.
async fn offering_device(state: &Arc<AppState>) -> Result<()> {
    let started = Instant::now();
    let profile = loop {
        let handle = device_link::create_device_offer(state, Some("Pollis on macOS".into())).await?;
        println!("PEER_OFFER={}", handle.qr_payload);
        match device_link::await_device_offer(state, handle.link_id).await? {
            device_link::DeviceOfferWait::SignedIn { profile } => break profile,
            device_link::DeviceOfferWait::Expired => {}
        }
        if started.elapsed() > WAIT {
            bail!("no phone accepted a code within {WAIT:?}");
        }
    };
    println!("PEER_SIGNED_IN={}", profile.username);
    new_device(state, &profile.id, profile.enrollment_required).await
}

async fn showing_device(state: &Arc<AppState>, user_id: &str, first_device: bool) -> Result<()> {
    if !first_device {
        bail!("show mode signs up a fresh account; use a new email");
    }
    pin::set_pin(state, None, PEER_PIN.to_string()).await?;
    auth::initialize_identity(state, user_id.to_string()).await?;
    let started = Instant::now();
    loop {
        let handle = device_link::create_device_link(state, user_id.to_string(), PEER_PIN.to_string()).await?;
        println!("PEER_LINK={}", handle.qr_payload);
        let mut since = "open".to_string();
        loop {
            let status =
                device_link::await_device_link(state, user_id.to_string(), handle.link_id.clone(), since).await?;
            match status {
                device_link::DeviceLinkStatus::ReadyToApprove { .. } => {
                    device_link::approve_device_link(state, user_id.to_string(), handle.link_id).await?;
                    println!("PEER_APPROVED");
                    return Ok(());
                }
                device_link::DeviceLinkStatus::Tampered => bail!("the phone's request did not verify"),
                device_link::DeviceLinkStatus::Expired => break,
                device_link::DeviceLinkStatus::Open => since = "open".into(),
                device_link::DeviceLinkStatus::Claimed { .. } => since = "claimed".into(),
            }
        }
        if started.elapsed() > WAIT {
            bail!("no phone signed in within {WAIT:?}");
        }
    }
}

async fn new_device(state: &Arc<AppState>, user_id: &str, enrollment_required: bool) -> Result<()> {
    if !enrollment_required {
        bail!("the account has no device yet — sign it up on the phone first");
    }
    let handle = device_enrollment::start_device_enrollment(state, user_id.to_string()).await?;
    println!("PEER_CODE={}", handle.verification_code);
    println!("PEER_REQUEST={}", handle.request_id);
    match device_enrollment::await_enrollment_approval(state, handle.request_id).await? {
        device_enrollment::EnrollmentStatus::Approved { .. } => {}
        device_enrollment::EnrollmentStatus::Rejected => {
            println!("PEER_REJECTED");
            return Ok(());
        }
        other => {
            println!("PEER_EXPIRED ({other:?})");
            return Ok(());
        }
    }
    // The order the apps use: PIN (opens the local DB), then finalize.
    pin::set_pin(state, None, PEER_PIN.to_string()).await?;
    device_enrollment::finalize_device_enrollment(state, user_id.to_string()).await?;
    println!("PEER_ENROLLED");
    Ok(())
}

async fn approver(
    state: &Arc<AppState>,
    user_id: &str,
    username: &str,
    first_device: bool,
    opts: &Opts,
) -> Result<()> {
    if !first_device {
        bail!("approver mode signs up a fresh account; use a new email");
    }
    pin::set_pin(state, None, PEER_PIN.to_string()).await?;
    auth::initialize_identity(state, user_id.to_string()).await?;
    println!("PEER_SIGNED_UP");
    if opts.seed > 0 {
        seed_history(state, user_id, username, opts.seed, opts.pad).await?;
    }

    let code_file = std::env::var("POLLIS_PEER_CODE_FILE")
        .context("POLLIS_PEER_CODE_FILE (where the script writes the phone's code) is required")?;
    let started = Instant::now();
    let request = loop {
        let pending = device_enrollment::list_pending_enrollment_requests(state, user_id.to_string()).await?;
        if let Some(r) = pending.into_iter().next() {
            break r;
        }
        if started.elapsed() > WAIT {
            bail!("no enrollment request arrived within {WAIT:?}");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    };
    println!("PEER_SAW_REQUEST={}", request.request_id);
    let code = loop {
        if let Ok(text) = std::fs::read_to_string(&code_file) {
            let code = device_enrollment::normalize_enrollment_sas(&text);
            if code.len() == device_enrollment::ENROLLMENT_SAS_LEN {
                break code;
            }
        }
        if started.elapsed() > WAIT {
            bail!("the script never wrote the phone's code");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    device_enrollment::approve_device_enrollment(state, request.request_id, code).await?;
    println!("PEER_APPROVED");
    if opts.transfer_source {
        transfer_source(state).await?;
    }
    Ok(())
}

/// `--seed-history N`: a group whose messages predate the phone, so only a transfer can bring them.
async fn seed_history(state: &Arc<AppState>, user_id: &str, username: &str, n: u32, pad: usize) -> Result<()> {
    // Words, not one long token, so a padded row still wraps like a message.
    let filler: String = "lorem ipsum dolor sit amet ".chars().cycle().take(pad).collect();
    let group = groups::create_group(
        "History Test Group".into(),
        None,
        user_id.to_string(),
        Some(true),
        Some(false),
        state,
    )
    .await
    .context("create the seeded group")?;
    let listed = groups::list_user_groups_with_channels(user_id.to_string(), state).await?;
    let channel = listed
        .into_iter()
        .find(|g| g.id == group.id)
        .and_then(|g| g.channels.into_iter().next())
        .context("the seeded group has no #General")?;
    for i in 1..=n {
        messages::send_message(
            channel.id.clone(),
            user_id.to_string(),
            if pad > 0 { format!("history-seed-{i:02} {filler}") } else { format!("history-seed-{i:02}") },
            None,
            None,
            Some(username.to_string()),
            state,
        )
        .await
        .with_context(|| format!("seed message {i}"))?;
    }
    println!("PEER_SEEDED={n}");
    Ok(())
}

fn ended(state: &str) -> bool {
    matches!(state, "completed" | "declined" | "cancelled" | "expired" | "failed")
}

/// Drive one transfer to its end: a run that comes back paused (the other side idle) runs again.
async fn drive(state: &Arc<AppState>, transfer_id: String) -> Result<()> {
    let started = Instant::now();
    loop {
        let snap = history_transfer::run_history_transfer(
            state,
            transfer_id.clone(),
            Arc::new(pollis_core::sink::NoopSink),
        )
        .await?;
        if ended(&snap.state) {
            println!("PEER_TRANSFER_DONE={} rows={} skipped={}", snap.state, snap.rows_done, snap.rows_skipped);
            return Ok(());
        }
        if started.elapsed() > WAIT {
            bail!("transfer {transfer_id} did not end within {WAIT:?} (last state {})", snap.state);
        }
    }
}

/// `--transfer-source`: approve the first history request (the PIN only when asked) and send it.
async fn transfer_source(state: &Arc<AppState>) -> Result<()> {
    let started = Instant::now();
    let request = loop {
        let pending = history_transfer::list_pending_history_transfer_requests(state).await?;
        if let Some(r) = pending.into_iter().next() {
            break r;
        }
        if started.elapsed() > WAIT {
            bail!("no history request arrived within {WAIT:?}");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    };
    let pin = request.pin_required.then(|| PEER_PIN.to_string());
    history_transfer::approve_history_transfer(state, request.transfer_id.clone(), pin).await?;
    println!("PEER_TRANSFER_APPROVED={}", request.transfer_id);
    drive(state, request.transfer_id).await
}

/// `--transfer-receiver`: ask the device that approved this one (else the first listed) and receive.
async fn transfer_receiver(state: &Arc<AppState>, opts: &Opts) -> Result<()> {
    let sources = history_transfer::list_history_transfer_sources(state).await?;
    let source = sources
        .iter()
        .find(|s| s.approved_this_device)
        .or_else(|| sources.first())
        .context("no other device to copy history from")?;
    let transfer =
        history_transfer::request_history_transfer(
            state,
            source.device_id.clone(),
            opts.via.clone().unwrap_or_else(|| "sibling".into()),
        )
        .await?;
    println!("PEER_TRANSFER_REQUESTED={}", transfer.transfer_id);
    drive(state, transfer.transfer_id).await
}
