//! Bots (#1182): a bot is a user scoped to one group; its hosts are its devices.
//! Design: epic #872 and issue #1182; DS side `pollis-delivery/src/bots.rs`;
//! docs `.codesight/wiki/bots.md`.
//!
//! # API
//!
//! Every host-side call takes the installation's own [`AppState`](crate::state::AppState):
//! one state per installation, built with its own keystore and its own data dir
//! (`AppState::local_dir`), so installations in one process share nothing.
//!
//! **Pairing a new bot (host).**
//! 1. [`start_new_bot_pairing`] generates the bot's account key and this host's
//!    device id locally, files the request and returns a [`PairingHandle`];
//!    print [`PairingHandle::code`].
//! 2. [`await_pairing`] waits (one call, backing off, bounded by the TTL) for the
//!    admin, then installs the host: store, device cert, key packages, and the
//!    keystore slot for unattended starts. → [`PairingOutcome::Paired`].
//! 3. [`join_bot_group`] takes the Welcome the admin's reconcile queued (or
//!    external-joins if none comes within the wait).
//!
//! **Adding a host (new host).** [`start_add_host_pairing`] for an existing bot,
//! then [`await_pairing`] (the account key arrives wrapped to this host's
//! ephemeral key) and [`join_bot_group`].
//!
//! **Adding a host (existing hosts).** On a `bot_host_approval` inbox wake-up, on
//! start, and at a [`HostApprovalOutcome::Deferred`]'s `retry_at`, call
//! [`handle_host_approvals`] with [`TransparencyLogAudit`]. Each approved request is
//! verified from this host's own state ([`verify_host_approval`]: signature under
//! the approver device's certified key, that device a leaf in this host's MLS view,
//! the approver an admin in the roster read, own bot and group, unexpired, not
//! consumed, ephemeral key byte-equal, transparency log not alarmed); the lowest
//! online device id wraps first, the next after [`WRAP_STAGGER_SECS`], and so on.
//! No host is assumed awake: the stagger is capped before the approval expires,
//! a peer not seen recently is not waited for, and a host that will not be up
//! at its `retry_at` calls [`handle_host_approvals_with`] with
//! [`WrapTiming::Now`] (the DS's status CAS keeps a second wrap from landing).
//!
//! **Restart.** [`start_unattended`] reopens an installation from the keystore,
//! with no PIN, and [`stop_unattended`] seals it again (the runtime does both
//! around every job); it refuses a human account. [`forget_installation`] removes one.
//!
//! **Trigger claims (runtime).** [`claim_trigger`]: before a host applies a
//! job's actions it claims `(installation_id, trigger_key)` on the DS; the first
//! claim wins, the winner's retry wins again, every other host discards.
//! [`trigger_cursor`] / [`set_trigger_cursor`] keep the job loop's per-channel
//! position in the installation's encrypted store.
//!
//! **App state (runtime, #2004).** [`app_state`]: an App's key-value state,
//! sealed under keys derived from the bot's account secret and stored on the
//! DS, shared by every host; [`app_state::commit_app_state`] writes a job's
//! changes once, under the claim the host won, at the versions it read.
//!
//! **App approvals (#2005).** [`app_approval`] holds the two signed record
//! types (production pins an App hash, development names a developer key; never
//! interchangeable) and the pure checks before every load
//! ([`app_approval::check_artifact`]). [`apps`] verifies a stored record from
//! the reader's own state with the same admin-signer rule as host approvals
//! (`signer.rs`), keeps the highest accepted `seq` in the installation's store
//! (rollback protection), and reports to admins. Bridge commands:
//! [`get_bot_app`], [`approve_bot_app`], [`auto_approve_bot_app`] (admins) and
//! [`list_group_apps`] (any member: the approved manifests, the marks).
//!
//! **Admin (bridge commands).** [`list_group_bots`], [`get_bot`], [`create_bot`],
//! [`add_bot_host`], [`revoke_bot_host`], [`delete_bot`]. Error strings start
//! with [`PAIRING_CODE_MISMATCH`], [`BOT_CAP_REACHED`] or [`PAIRING_REQUEST_EXPIRED`]
//! where the UI needs to tell them apart.
//!
//! **Pairing code.** [`derive_pairing_code`] binds the ephemeral key, the device
//! id and the bot's account key (or bot id); [`normalize_pairing_code`] is the
//! enrollment SAS input rule.

/// The App manifest crate, for callers that hold approvals (#2005).
pub use pollis_app_manifest as manifest;

mod admin;
pub mod app_approval;
pub mod app_state;
pub mod approval;
pub mod apps;
mod code;
mod host;
mod signer;
mod triggers;
mod unattended;

pub use admin::{
    add_bot_host, create_bot, delete_bot, get_bot, list_group_bots, revoke_bot_host, sign_approval, BotDetail,
    BotHost, BotSummary, BOT_CAP_REACHED, PAIRING_CODE_MISMATCH, PAIRING_REQUEST_EXPIRED,
};
pub use approval::{HostApproval, HOST_APPROVAL_DOMAIN, HOST_APPROVAL_MAX_TTL_SECS, HOST_APPROVAL_VERSION};
pub use apps::{
    auto_approve_bot_app, approve_bot_app, get_bot_app, list_group_apps, ApproveBotAppInput, BotAppView, GroupAppView,
    APP_APPROVAL_CHANGED, APP_APPROVAL_STALE,
};
pub use code::{code_lookup, derive_pairing_code, normalize_pairing_code, PAIRING_CODE_LEN};
pub use host::{
    await_pairing, handle_host_approvals, handle_host_approvals_with, join_bot_group, start_add_host_pairing, start_new_bot_pairing,
    verify_host_approval, AccountKeyAudit, ApprovalRefusal, HostApprovalOutcome, PairingHandle, PairingOutcome,
    TransparencyLogAudit, WrapTiming, WRAP_STAGGER_SECS,
};
#[cfg(feature = "test-harness")]
pub use host::FixedAudit;
pub use triggers::{claim_trigger, mentions_username, message_meta, set_trigger_cursor, trigger_cursor};
pub use unattended::{forget_installation, has_installation, start_unattended, stop_unattended, BotSession, UNATTENDED_REFUSED};
