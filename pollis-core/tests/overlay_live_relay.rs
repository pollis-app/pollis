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

    // Log in with the overlay OFF: pre-cert OTP can't authenticate to a relay.
    let profile = auth::get_session(&state)
        .await
        .expect("get_session")
        .expect("DEV_EMAIL should auto-log-in");
    eprintln!("[live] logged in as user {}", profile.id);

    // The relay handshake needs the account key and device signing key.
    pin::set_pin(&state, None, "0000".to_string())
        .await
        .expect("set_pin (opens local DB, persists account key)");
    auth::initialize_identity(&state, profile.id.clone())
        .await
        .expect("initialize_identity");
    eprintln!("[live] device fully provisioned (PIN set, identity initialized)");

    // Strict = no silent direct fallback.
    overlay::apply_overlay_mode(&state, OverlayMode::Strict)
        .await
        .expect("apply Strict overlay (directory fetch + pool build)");
    let live = overlay::get_overlay_mode(&state).await.expect("get_overlay_mode");
    assert_eq!(live, "strict", "overlay must actually be strict now");
    eprintln!("[live] overlay is STRICT — remote DB is routed through the relay");

    // In Strict this can only succeed through the relay.
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
