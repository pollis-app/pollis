//! MLS commands — split into cohesive submodules. Public surface is
//! preserved via the `pub use` re-exports below so every external caller
//! (Tauri shims, sibling `commands::*` modules, integration tests) keeps
//! resolving names at `pollis_core::commands::mls::*`.

mod delivery;
mod device;
pub(crate) mod ds_client;
pub(crate) mod ds_reads;
pub(crate) mod generation;
mod group_state;
pub mod invariants;
mod key_packages;
mod migrate;
// `pub(crate)` because the media-only voice key export builds its own provider
// outside this module.
pub(crate) mod provider;
mod reconcile;
mod self_update;
mod sweep;
mod welcomes;

// ── Provider / credential helpers ────────────────────────────────────────────
pub use provider::{
    make_credential, parse_credential_device_id, parse_credential_user_id, PollisProvider,
};
// Out-of-module MLS crypto (voice key export). Only the `media` build has such
// a call site.
#[cfg(feature = "media")]
pub(crate) use provider::MlsProvider;
// The one seam the flows harness needs to exercise suite migration now that
// production ships a single suite (#669) — see `provider::current_suite`.
#[cfg(feature = "test-harness")]
pub use provider::set_current_suite_override;

// ── Per-device signing keys + cross-signing ──────────────────────────────────
pub use device::{
    ensure_device_cert, load_device_cert_pubs, load_device_pq_signing_key,
    load_or_create_device_signer, resign_stale_device_certs, stale_cert_candidates,
};

// ── Signed Delivery-Service write client (4 `X-Pollis-*` headers) ────────────
pub(crate) use ds_client::{
    current_user_id, decode_response, ds_claim_key_package, ds_livekit_send_data,
    ds_livekit_token, ds_post, ds_post_json, ds_post_ok, ds_post_plain, ds_post_session_ok,
    ds_post_signed_or_session, ds_post_signed_or_session_ok,
};
// Desktop-only (voice roster); mobile has no Rust-side participants path.
#[cfg(feature = "media")]
pub(crate) use ds_client::ds_livekit_participants;
// #836 identity resolution is media-only: the headless/mobile build drives
// LiveKit through the native SDK and never sees a Rust-side participant
// identity, so neither the resolver nor its cache exists there.
#[cfg(feature = "media")]
pub(crate) use ds_client::ds_livekit_identities;

// ── Key packages ─────────────────────────────────────────────────────────────
pub use key_packages::{ensure_mls_key_package, validate_key_package};

// ── Welcomes ─────────────────────────────────────────────────────────────────
pub use welcomes::{
    apply_welcome, poll_mls_welcomes, poll_mls_welcomes_inner, reset_welcome_delivery,
};

// ── Group lifecycle / encrypt / decrypt / commit processing ──────────────────
pub use group_state::{
    envelope_lineage, external_join_group, forget_local_mls_group, has_local_group, init_mls_group,
    process_pending_commits, process_pending_commits_inner, process_pending_commits_inner_with_hook,
    publish_group_info, try_mls_decrypt, try_mls_encrypt, MlsDecryptor,
};

// ── Cold-launch / post-reconnect sweep ──────────────────────────────────────
pub use sweep::catch_up_all_mls_groups;

// ── Own-leaf rotation (post-join merge + periodic PCS) ───────────────────────
pub use self_update::{self_update_group, self_update_if_due};

// ── Reconcile + self-repair ──────────────────────────────────────────────────
pub use reconcile::{
    reconcile_group_mls_core, reconcile_group_mls_core_staged, reconcile_group_mls_impl,
    ReconcileCommitData, ReconcileOutcome,
};

#[cfg(test)]
mod tests;
