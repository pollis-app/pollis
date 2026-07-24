//! Live end-to-end: prove the directory-backed overlay actually routes real
//! control-plane traffic through a real relay — not a mock, not a unit stub.
//!
//! Ignored by default (needs a dev backend + a local relay + a signed directory).
//! Run it against the setup in the PR notes:
//!   - a real `pollis-relay` bound on 127.0.0.1:9444 whose allowlist covers the
//!     dev hosts;
//!   - a signed directory served over HTTP pointing at that relay;
//!   - dev env (`.env.development`) + `DEV_EMAIL`/`DEV_OTP` for auto-enroll +
//!     `POLLIS_OVERLAY_DIRECTORY_URL` / `POLLIS_OVERLAY_DIRECTORY_KEY`.
//!
//!   set -a; . ./.env.development; set +a
//!   DEV_EMAIL=reltest@mail.com DEV_OTP=000000 \
//!   POLLIS_OVERLAY_DIRECTORY_URL=http://127.0.0.1:8799/directory.json \
//!   POLLIS_OVERLAY_DIRECTORY_KEY=<pubkey> POLLIS_DATA_DIR=/tmp/relaytest/tui-data \
//!   cargo test -p pollis-core --test overlay_live_relay -- --ignored --nocapture

use std::sync::Arc;

use pollis_core::commands::{auth, overlay, pin, user};
use pollis_core::config::{Config, OverlayMode};
use pollis_core::state::AppState;

#[tokio::test]
#[ignore]
async fn overlay_directory_routes_real_traffic_through_a_relay() {
    let config = Config::from_env().expect("Config::from_env");
    assert!(
        config.overlay_directory_configured(),
        "set POLLIS_OVERLAY_DIRECTORY_URL + _KEY to exercise the directory path"
    );
    let state = Arc::new(AppState::new(config).await.expect("AppState::new"));

    // 1. Log in first, with the overlay OFF — enrollment/OTP is a pre-cert
    //    bootstrap that must go direct (it can't authenticate to a relay yet).
    let profile = auth::get_session(&state)
        .await
        .expect("get_session")
        .expect("DEV_EMAIL should auto-log-in");
    eprintln!("[live] logged in as user {}", profile.id);

    // First-device tail: set a PIN (opens the encrypted local DB + persists the
    // account key) then initialize identity — the relay handshake needs both the
    // account key and the device signing key, which this makes loadable. Mirrors
    // the desktop/TUI first-device flow (set_pin -> initialize_identity).
    pin::set_pin(&state, None, "0000".to_string())
        .await
        .expect("set_pin (opens local DB, persists account key)");
    auth::initialize_identity(&state, profile.id.clone())
        .await
        .expect("initialize_identity");
    eprintln!("[live] device fully provisioned (PIN set, identity initialized)");

    // 2. Turn the overlay to STRICT. This fetches the signed directory, verifies
    //    it against the pinned key, builds the relay pool, and reconnects the
    //    remote DBs through the loopback shim. Strict = no silent direct fallback.
    overlay::apply_overlay_mode(&state, OverlayMode::Strict)
        .await
        .expect("apply Strict overlay (directory fetch + pool build)");
    let live = overlay::get_overlay_mode(&state).await.expect("get_overlay_mode");
    assert_eq!(live, "strict", "overlay must actually be strict now");
    eprintln!("[live] overlay is STRICT — remote DB is routed through the relay");

    // 3. A real remote read: SELECT from the users table on Turso. In Strict this
    //    can ONLY succeed by going through the relay — if the relay weren't
    //    forwarding, this would degrade/error instead of returning the row.
    let read = user::get_user_profile(profile.id.clone(), &state)
        .await
        .expect("remote profile read routed through the relay");
    assert!(
        read.is_some(),
        "the users row must read back — proving the relay forwarded the Turso query"
    );
    eprintln!(
        "[live] READ OK THROUGH THE RELAY: users row id={} — a real Turso query \
         crossed the relay under Strict mode.",
        read.unwrap().id
    );
}
