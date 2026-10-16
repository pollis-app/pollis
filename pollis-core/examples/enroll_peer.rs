//! Dev-DS peer for the mobile Maestro device-approval flows (#1360).
//!
//! It plays the DESKTOP half of device linking with the very `pollis-core`
//! commands the desktop app calls, so a phone under Maestro can be checked
//! against it in both directions:
//!
//! - `new <email>` — a new device of an existing account: email sign-in, then
//!   `start_device_enrollment` (the desktop's "Approve from another device").
//!   Prints `PEER_CODE=<code>` and `PEER_REQUEST=<id>`, waits for a signed-in
//!   device to approve, then sets its PIN and finalizes (`PEER_ENROLLED`), or
//!   prints `PEER_REJECTED` / `PEER_EXPIRED`.
//! - `approver <email>` — the account's FIRST device: signs up
//!   (`PEER_SIGNED_UP`), waits for another device's pending request and for
//!   the code that device shows to be written to `$POLLIS_PEER_CODE_FILE`
//!   (Maestro reads it off the phone's screen), then approves it with
//!   `approve_device_enrollment`, the call desktop's approval prompt makes
//!   (`PEER_APPROVED`).
//!
//! An example of `pollis-core` rather than `pollis-tui` so it can be built
//! with `--no-default-features`: no `os-keystore`, so the peer keeps its keys
//! in the encrypted file under `POLLIS_DATA_DIR` and never touches the macOS
//! Keychain (whose prompts a run over SSH must never raise).
//!
//!   POLLIS_DATA_DIR=$(mktemp -d) POLLIS_DELIVERY_URL=https://api-dev.pollis.com \
//!   POLLIS_DEV_ACCESS_KEY=… cargo run -p pollis-core --no-default-features \
//!     --example enroll_peer -- new <email>

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use pollis_core::commands::{auth, device_enrollment, pin};
use pollis_core::config::Config;
use pollis_core::state::AppState;

/// The dev DS's fixed OTP (`DEV_OTP`); override with `POLLIS_DEV_OTP`.
const DEFAULT_OTP: &str = "000000";
/// How long to wait for the phone at each step.
const WAIT: Duration = Duration::from_secs(600);
/// The peer's own PIN; it never has to be typed anywhere.
const PEER_PIN: &str = "0000";

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().context("usage: enroll_peer <new|approver> <email>")?;
    let email = args.next().context("usage: enroll_peer <new|approver> <email>")?;
    let otp = std::env::var("POLLIS_DEV_OTP").unwrap_or_else(|_| DEFAULT_OTP.to_string());
    let config = Config::from_env().context("POLLIS_DELIVERY_URL (the dev DS) is required")?;
    let ds = config.pollis_delivery_url.clone().unwrap_or_default();
    if !ds.contains("api-dev") && !ds.contains("localhost") {
        bail!("refusing to run against {ds:?} — dev DS only");
    }
    let state = Arc::new(AppState::new(config).await.context("AppState")?);

    auth::request_otp(&state, email.clone()).await?;
    let me = auth::verify_otp(&state, email, otp).await?;
    match mode.as_str() {
        "new" => new_device(&state, &me.id, me.enrollment_required).await,
        "approver" => approver(&state, &me.id, me.new_secret_key.is_some()).await,
        other => bail!("unknown mode {other:?}"),
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
        device_enrollment::EnrollmentStatus::Approved => {}
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

async fn approver(state: &Arc<AppState>, user_id: &str, first_device: bool) -> Result<()> {
    if !first_device {
        bail!("approver mode signs up a fresh account; use a new email");
    }
    pin::set_pin(state, None, PEER_PIN.to_string()).await?;
    auth::initialize_identity(state, user_id.to_string()).await?;
    println!("PEER_SIGNED_UP");

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
    Ok(())
}
