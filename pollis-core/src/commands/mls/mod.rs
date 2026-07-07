//! MLS commands — split into cohesive submodules. Public surface is
//! preserved via the `pub use` re-exports below so every external caller
//! (Tauri shims, sibling `commands::*` modules, integration tests) keeps
//! resolving names at `pollis_core::commands::mls::*`.

mod delivery;
mod device;
mod ds_client;
mod group_state;
pub mod invariants;
mod key_packages;
mod provider;
mod reconcile;
mod sweep;
mod welcomes;

// ── Provider / credential helpers ────────────────────────────────────────────
pub use provider::{
    make_credential, parse_credential_device_id, parse_credential_user_id, PollisProvider,
};

// ── Per-device signing keys + cross-signing ──────────────────────────────────
pub use device::{ensure_device_cert, load_or_create_device_signer, resign_stale_device_certs};

// ── Signed Delivery-Service write client (4 `X-Pollis-*` headers) ────────────
pub(crate) use ds_client::{
    ds_claim_key_package, ds_post, ds_post_ok, ds_post_plain, ds_post_session_ok,
    ds_post_signed_or_session, ds_post_signed_or_session_ok,
};

// ── Key packages ─────────────────────────────────────────────────────────────
pub use key_packages::{ensure_mls_key_package, validate_key_package};

// ── Welcomes ─────────────────────────────────────────────────────────────────
pub use welcomes::{
    apply_welcome, poll_mls_welcomes, poll_mls_welcomes_inner, reset_welcome_delivery,
};

// ── Group lifecycle / encrypt / decrypt / commit processing ──────────────────
pub use group_state::{
    envelope_epoch, external_join_group, forget_local_mls_group, has_local_group, init_mls_group,
    process_pending_commits, process_pending_commits_inner, process_pending_commits_inner_with_hook,
    publish_group_info, try_mls_decrypt, try_mls_encrypt,
};

// ── Cold-launch / post-reconnect sweep ──────────────────────────────────────
pub use sweep::catch_up_all_mls_groups;

// ── Reconcile + self-repair ──────────────────────────────────────────────────
pub use reconcile::{
    reconcile_group_mls_core, reconcile_group_mls_core_staged, reconcile_group_mls_impl,
    ReconcileCommitData, ReconcileOutcome,
};

#[cfg(test)]
mod tests;
