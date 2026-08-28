//! Pollis MLS Delivery Service.
//!
//! The sole writer to the MLS control-plane tables. It serializes commits per
//! conversation and serves the contiguous commit log. It sees only opaque
//! blobs — never plaintext or group/private keys — so it cannot decrypt or
//! forge a commit (RFC 9420 "Delivery Service" role). Clients keep all MLS
//! crypto; they submit commits here instead of writing the DB directly, which
//! is what makes forks/wedges structurally impossible (see [`commit`]).
//!
//! ## Write authentication ([`auth`])
//!
//! Writes (`POST /v1/commits`) can be gated behind device-certificate-signature
//! auth: the client signs each request with its Ed25519 device key and the DS
//! verifies against the registered `user_device.mls_signature_pub`. Reads
//! (`GET /v1/commits/:id`) and `/health` stay open.
//!
//! Enforcement is **ON by default** (#921). `POLLIS_DS_REQUIRE_AUTH` is an
//! explicit *opt-out* — only `false`/`0`/`no`/`off` disables it, unset and
//! unrecognised values both enforce, and starting with it off logs at ERROR.
//! See [`require_auth_from_value`]. With the gate off the DS attributes every
//! write to an actor id the *caller* put in the body, so a no-auth deployment
//! trusts whoever asks; that is a local development affordance and never a
//! production configuration.

pub mod account;
pub mod account_reads;
pub mod auth;
pub mod bootstrap;
pub mod broker;
pub mod cert;
pub mod commit;
pub mod db;
pub mod devices;
pub mod directory;
pub mod email_change;
pub mod emoji;
pub mod error;
pub mod groups;
pub mod headers;
pub mod invite_token;
pub mod messages;
pub mod otp;
pub mod participant_id;
pub mod pins;
pub mod profile;
pub mod push;
pub mod ratelimit;
pub mod reads;
pub mod redact;
pub mod room_id;
pub mod session;
pub mod teardown;
pub mod util;
pub mod vault;
pub mod writes;

use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, Method, StatusCode, Uri},
    middleware::{from_fn, from_fn_with_state},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use pollis_api::DsRequest;
use serde::Deserialize;

use crate::commit::{CommitSinceReport, CommitsResponse, SubmitBody, SubmitResponse};
use crate::db::Db;
use crate::error::{AppError, AuthRejection};

/// Shared handler state: the DBs plus whether write auth is enforced.
#[derive(Clone)]
pub struct AppState {
    /// Main DB — user/device metadata (auth lookups live here).
    pub db: Arc<Db>,
    /// Commit-log DB — the MLS control-plane tables (`mls_commit_log`,
    /// `mls_group_info`, `mls_welcome`). The DS is its sole writer. Defaults to
    /// the same handle as `db` when no separate log DB is configured.
    pub log_db: Arc<Db>,
    /// When true, `POST /v1/commits` requires a valid device signature.
    pub require_auth: bool,
    /// In-memory OTP store (request-otp / verify-otp). Shallow-`Clone` (shared
    /// `Arc`), so every `AppState` clone sees the same codes.
    pub otp: otp::OtpStore,
    /// In-memory OTP-session store gating the bootstrap writes.
    pub sessions: session::SessionStore,
    /// OTP/session tunables + the Resend key (DS env).
    pub otp_config: otp::OtpConfig,
    /// Email-change OTP store + requester binding (device-signed, separate from
    /// the signup OTP store so the two can't collide on the same address).
    pub email_change: email_change::EmailChangeStore,
    /// Authorized-secrets broker config (#393) — LiveKit + R2 secrets read from
    /// DS env. Default all-`None`; the matching endpoint 503s until configured.
    pub broker: broker::BrokerConfig,
    /// In-memory per-IP rate limiter for the signup-OTP endpoints. Shallow-`Clone`
    /// (shared `Arc`), so every `AppState` clone shares the same counters.
    pub ratelimit: ratelimit::RateLimiter,
    /// Per-IP rate-limit tunables (DS env).
    pub ratelimit_config: ratelimit::RateLimitConfig,
    /// In-memory device-pubkey cache for the device-signature auth path (#658).
    /// Shallow-`Clone` (shared `Arc`), so every `AppState` clone — i.e. every
    /// request — sees the same map, and an eviction from any handler is
    /// immediately visible to the auth gate. See [`auth::DeviceKeyCache`] for
    /// the invalidation contract and the `max_instances: 1` assumption it rests
    /// on.
    pub device_keys: auth::DeviceKeyCache,
    /// Latest identity-free envelope-growth snapshot (#720 checkbox 1), written
    /// by the GC sweep and served on `GET /v1/retention/metrics`. Shared `Arc`, so
    /// the sweep task and every request handler see the same cell. `None` until
    /// the first sweep completes.
    pub retention_metrics: messages::RetentionMetricsHandle,
    /// Operator bearer token gating `GET /v1/retention/metrics` (#720). Read from
    /// `POLLIS_DS_METRICS_TOKEN`. `None` (unset) means the endpoint is NOT served
    /// — it 404s, fail-closed — because the growth snapshot is a pollable activity
    /// meter (envelope totals, conversation counts, growth over time) that a
    /// privacy-first messenger must not publish openly, even though it names no
    /// user. `Some(token)` → callers must present `Authorization: Bearer <token>`.
    /// The consumer is the OWNER'S scraper, not a client device, so this is a
    /// static operator secret rather than a device signature. See
    /// [`retention_metrics`].
    pub metrics_token: Option<String>,
}

impl AppState {
    /// Single-DB state: the commit log shares `db`. OTP/session stores start
    /// empty with a default config (no Resend key, no `DEV_OTP`) — `main` swaps
    /// in [`otp::OtpConfig::from_env`].
    pub fn new(db: Arc<Db>, require_auth: bool) -> Self {
        let log_db = Arc::clone(&db);
        Self::new_with_log_db(db, log_db, require_auth)
    }

    /// State with a separate commit-log DB for the MLS control-plane tables.
    pub fn new_with_log_db(db: Arc<Db>, log_db: Arc<Db>, require_auth: bool) -> Self {
        Self {
            db,
            log_db,
            require_auth,
            otp: otp::OtpStore::default(),
            sessions: session::SessionStore::default(),
            otp_config: otp::OtpConfig::default(),
            email_change: email_change::EmailChangeStore::default(),
            broker: broker::BrokerConfig::default(),
            ratelimit: ratelimit::RateLimiter::default(),
            ratelimit_config: ratelimit::RateLimitConfig::default(),
            device_keys: auth::DeviceKeyCache::default(),
            retention_metrics: messages::RetentionMetricsHandle::default(),
            // Default-closed: no token → the metrics endpoint is not served. `main`
            // reads `POLLIS_DS_METRICS_TOKEN` via `with_metrics_token`; tests opt in
            // explicitly. Leaving it `None` here keeps the integration harness and
            // every other test that never touches the endpoint unaffected.
            metrics_token: None,
        }
    }

    /// Override the OTP config (Resend key, `DEV_OTP`, TTLs). Builder so `main`
    /// can thread DS env without widening the constructors.
    pub fn with_otp_config(mut self, config: otp::OtpConfig) -> Self {
        self.otp_config = config;
        self
    }

    /// Override the broker config (LiveKit + R2 secrets). Builder so `main` can
    /// thread DS env without widening the constructors, mirroring
    /// [`Self::with_otp_config`].
    pub fn with_broker_config(mut self, config: broker::BrokerConfig) -> Self {
        self.broker = config;
        self
    }

    /// Override the per-IP rate-limit config. Builder so `main` can thread DS env
    /// (and tests can set tiny windows), mirroring [`Self::with_otp_config`].
    pub fn with_ratelimit_config(mut self, config: ratelimit::RateLimitConfig) -> Self {
        self.ratelimit_config = config;
        self
    }

    /// Set the operator bearer token gating `GET /v1/retention/metrics`. `None`
    /// leaves the endpoint fail-closed (404, not served). Builder so `main` can
    /// thread `POLLIS_DS_METRICS_TOKEN` and tests can opt in, mirroring
    /// [`Self::with_otp_config`].
    pub fn with_metrics_token(mut self, token: Option<String>) -> Self {
        self.metrics_token = token;
        self
    }
}

/// Read the `POLLIS_DS_REQUIRE_AUTH` gate. **Default ON** (#921) — see
/// [`require_auth_from_value`] for the parsing rule and why it fails closed.
pub fn require_auth_from_env() -> bool {
    require_auth_from_value(std::env::var("POLLIS_DS_REQUIRE_AUTH").ok().as_deref())
}

/// [`require_auth_from_env`] with the environment lifted out, so the rule itself
/// is testable without mutating process-global state from a parallel test.
///
/// **ON unless explicitly turned off.** Only `false`/`0`/`no`/`off`
/// (case-insensitive, surrounding whitespace ignored) disable enforcement;
/// unset, empty, and anything unrecognised all mean ON.
///
/// It defaulted OFF until #921. That was survivable only by accident: with the
/// gate off the DS takes the actor from a client-supplied body field, and until
/// #875 twenty of those bodies simply never SET that field, so the unsigned path
/// 403'd on its own. #875 fixed the omissions — which turned "auth off" from
/// mostly-broken into a working deployment that believes whatever actor a caller
/// names, on every write endpoint. A default that is only safe while a bug
/// survives is not a default.
///
/// Fails closed on garbage on purpose: `POLLIS_DS_REQUIRE_AUTH=ture` is a typo,
/// and the safe reading of a typo is "enforce", never "trust everyone". The one
/// way to run open is to say so, in words the parser recognises.
fn require_auth_from_value(raw: Option<&str>) -> bool {
    !matches!(
        raw.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("false") | Some("0") | Some("no") | Some("off")
    )
}

/// Build the HTTP router from a bare DB, reading the auth gate from the
/// environment. Used by `main`; logs the enforcement state at startup. The
/// commit log shares the single `db` (no separate log DB).
pub fn build_router(db: Arc<Db>) -> Router {
    let log_db = Arc::clone(&db);
    build_router_with_log_db(db, log_db)
}

/// Like [`build_router`], but with a separate commit-log DB for the MLS
/// control-plane tables. `log_db` may be the same handle as `db` (single-DB
/// fallback). Reads the auth gate from the environment.
pub fn build_router_with_log_db(db: Arc<Db>, log_db: Arc<Db>) -> Router {
    build_router_with_state(build_app_state(db, log_db))
}

/// Build the [`AppState`] from the environment (auth gate + OTP/broker/rate-limit
/// config). Extracted from [`build_router_with_log_db`] so `main` can hold the
/// state — specifically `state.retention_metrics` — before spawning the GC sweep,
/// then build the router from the same state.
pub fn build_app_state(db: Arc<Db>, log_db: Arc<Db>) -> AppState {
    let require_auth = require_auth_from_env();
    if require_auth {
        tracing::info!(
            require_auth,
            "pollis-delivery write auth: ENFORCED (device-cert signature required on writes)"
        );
    } else {
        // Not `info!` (#921). Enforcement is the default and the only safe
        // setting; running without it means every write endpoint takes the
        // acting user from a body field the caller controls, so any client may
        // write as anyone. It is reachable ONLY by setting the variable to an
        // explicit off value, and it says so loudly enough to be noticed in a
        // log the operator is scanning for problems rather than for startup
        // chatter.
        tracing::error!(
            require_auth,
            "pollis-delivery write auth: DISABLED by explicit POLLIS_DS_REQUIRE_AUTH opt-out — \
             every write is attributed to a CLIENT-SUPPLIED actor id and CANNOT be trusted. \
             This is not a supported production configuration; unset the variable to restore \
             device-signature enforcement."
        );
    }
    let metrics_token = metrics_token_from_env();
    tracing::info!(
        metrics_endpoint = if metrics_token.is_some() { "gated" } else { "disabled" },
        "pollis-delivery retention metrics: {}",
        if metrics_token.is_some() {
            "GET /v1/retention/metrics ENABLED (Authorization: Bearer required)"
        } else {
            "GET /v1/retention/metrics DISABLED (POLLIS_DS_METRICS_TOKEN unset — endpoint 404s)"
        }
    );
    AppState::new_with_log_db(db, log_db, require_auth)
        .with_otp_config(otp::OtpConfig::from_env())
        .with_broker_config(broker::BrokerConfig::from_env())
        .with_ratelimit_config(ratelimit::RateLimitConfig::from_env())
        .with_metrics_token(metrics_token)
}

/// Read the `POLLIS_DS_METRICS_TOKEN` operator secret gating
/// `GET /v1/retention/metrics`. Empty is treated as unset. Unset → `None`, and the
/// endpoint is not served (fail-closed) — see [`AppState::metrics_token`].
pub fn metrics_token_from_env() -> Option<String> {
    std::env::var("POLLIS_DS_METRICS_TOKEN")
        .ok()
        .filter(|s| !s.is_empty())
}

/// Build the HTTP router from an explicit [`AppState`]. Exposed so tests can
/// drive the real router with auth toggled either way.
pub fn build_router_with_state(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/version", get(version))
        .route("/v1/retention/metrics", get(retention_metrics))
        .route("/v1/config", get(effective_config))
        .route(<SubmitBody as DsRequest>::PATH, post(submit))
        .route(<CommitSinceReport as DsRequest>::PATH, post(report_commit_since))
        .route("/v1/commits/:conversation_id", get(commits))
        .route(<writes::GroupInfoBody as DsRequest>::PATH, post(writes::group_info))
        .route(<writes::AckBody as DsRequest>::PATH, post(writes::welcomes_ack))
        .route(<writes::ResetBody as DsRequest>::PATH, post(writes::welcomes_reset))
        .route(<writes::PurgeBody as DsRequest>::PATH, post(writes::welcomes_purge))
        .route(<writes::ResubmitBody as DsRequest>::PATH, post(writes::welcomes_resubmit))
        // ── MLS control-plane READS (#987) ───────────────────────────────────
        // POST, not GET: the canonical signing message covers the path without
        // its query string, so a signed GET's parameters are unauthenticated.
        .route(<reads::FetchWelcomesBody as DsRequest>::PATH, post(reads::fetch_welcomes))
        .route(<reads::ConversationStateBody as DsRequest>::PATH, post(reads::conversation_state))
        // Domain A (#419) — messages / envelopes / watermarks / reactions /
        // attachments. All land on the MAIN DB.
        .route(<messages::SendMessageBody as DsRequest>::PATH, post(messages::send_message))
        .route(<messages::EditMessageBody as DsRequest>::PATH, post(messages::edit_message))
        .route(<messages::DeleteMessageBody as DsRequest>::PATH, post(messages::delete_message))
        .route(<messages::AddReaction as DsRequest>::PATH, post(messages::add_reaction))
        .route(<messages::RemoveReaction as DsRequest>::PATH, post(messages::remove_reaction))
        .route(<messages::WatermarkBody as DsRequest>::PATH, post(messages::advance_watermark))
        .route(<messages::EnvelopeGcBody as DsRequest>::PATH, post(messages::envelope_gc))
        .route(<messages::AttachmentRegisterBody as DsRequest>::PATH, post(messages::register_attachment))
        .route(<messages::AttachmentDeleteBody as DsRequest>::PATH, post(messages::delete_attachment))
        // Domain B (#419) — groups / channels / membership / invites /
        // join-requests. All land on the MAIN DB.
        .route(<groups::CreateGroupBody as DsRequest>::PATH, post(groups::create_group))
        .route(<groups::UpdateGroupBody as DsRequest>::PATH, post(groups::update_group))
        .route(<groups::DeleteGroupBody as DsRequest>::PATH, post(groups::delete_group))
        .route(<groups::LeaveGroupBody as DsRequest>::PATH, post(groups::leave_group))
        .route(<groups::CreateChannelBody as DsRequest>::PATH, post(groups::create_channel))
        .route(<groups::UpdateChannelBody as DsRequest>::PATH, post(groups::update_channel))
        .route(<groups::DeleteChannelBody as DsRequest>::PATH, post(groups::delete_channel))
        .route(<groups::RemoveMemberBody as DsRequest>::PATH, post(groups::remove_member))
        .route(<groups::SetMemberRoleBody as DsRequest>::PATH, post(groups::set_member_role))
        .route(<groups::CreateInviteBody as DsRequest>::PATH, post(groups::create_invite))
        .route(<groups::AcceptInviteBody as DsRequest>::PATH, post(groups::accept_invite))
        .route(<groups::DeclineInviteBody as DsRequest>::PATH, post(groups::decline_invite))
        .route(<groups::CreateInviteLinkBody as DsRequest>::PATH, post(groups::create_invite_link))
        .route(<groups::RevokeInviteLinkBody as DsRequest>::PATH, post(groups::revoke_invite_link))
        .route(<groups::RedeemInviteLinkBody as DsRequest>::PATH, post(groups::redeem_invite_link))
        .route(<groups::CreateJoinRequestBody as DsRequest>::PATH, post(groups::create_join_request))
        .route(<groups::ApproveJoinRequestBody as DsRequest>::PATH, post(groups::approve_join_request))
        .route(<groups::RejectJoinRequestBody as DsRequest>::PATH, post(groups::reject_join_request))
        // Domain F (#848) — custom per-group emoji. Lands on the MAIN DB.
        // `create` binds a content-addressed object to a (group, shortcode);
        // `remove` releases it and collects the object if that was the last
        // reference; `gc` sweeps objects whose references vanished with their
        // group. See the `emoji` module docs for the storage + permission model.
        .route(<emoji::CreateEmojiBody as DsRequest>::PATH, post(emoji::create_emoji))
        .route(<emoji::RemoveEmojiBody as DsRequest>::PATH, post(emoji::remove_emoji))
        .route(<emoji::EmojiGcBody as DsRequest>::PATH, post(emoji::emoji_gc))
        // Domain C (#419) — profile / preferences / blocks / DMs. All land on
        // the MAIN DB.
        .route(<profile::UpdateProfileBody as DsRequest>::PATH, post(profile::update_profile))
        .route(<profile::SavePreferencesBody as DsRequest>::PATH, post(profile::save_preferences))
        .route(<profile::AddBlock as DsRequest>::PATH, post(profile::block_user))
        .route(<profile::RemoveBlock as DsRequest>::PATH, post(profile::unblock_user))
        .route(<profile::CreateDmBody as DsRequest>::PATH, post(profile::create_dm))
        .route(<profile::AcceptDmBody as DsRequest>::PATH, post(profile::accept_dm))
        .route(<profile::AddDmMemberBody as DsRequest>::PATH, post(profile::add_dm_member))
        .route(<profile::RemoveDmMemberBody as DsRequest>::PATH, post(profile::remove_dm_member))
        .route(<profile::LeaveDmBody as DsRequest>::PATH, post(profile::leave_dm))
        // The encrypted read-cursor push (#844). Its plaintext neighbour two
        // lines up is `/v1/profile/preferences`; this one stores ciphertext.
        .route(<profile::SaveReadCursorsBody as DsRequest>::PATH, post(profile::save_read_cursors))
        // Pinned messages (#99) — the wrapped pin-key state (Kpin, CAS on
        // (generation, epoch)) and the pin snapshots sealed under it. Both
        // ciphertext-only tables on the MAIN DB.
        .route(<pins::UpsertPinKeystateBody as DsRequest>::PATH, post(pins::upsert_keystate))
        .route(<pins::PinMessageBody as DsRequest>::PATH, post(pins::pin_message))
        .route(<pins::UnpinMessageBody as DsRequest>::PATH, post(pins::unpin_message))
        .route(<pins::PinKeystateBody as DsRequest>::PATH, post(pins::read_keystate))
        .route(<pins::ListPinsBody as DsRequest>::PATH, post(pins::read_pins))
        // The Vault (#107) — per-entry opaque blobs under the account identity
        // key, the read-cursor construction applied per entry. MAIN DB.
        .route(<vault::SaveVaultMessageBody as DsRequest>::PATH, post(vault::save_vault_message))
        .route(<vault::DeleteVaultMessageBody as DsRequest>::PATH, post(vault::delete_vault_message))
        .route(<vault::VaultMessagesBody as DsRequest>::PATH, post(vault::read_vault))
        // Domain D (#419) — key-packages / device-cert re-sign / push tokens.
        // All land on the MAIN DB. Device registration + the FIRST cert publish
        // are bootstrap writes that stay on the client's direct path (see
        // `devices` module docs).
        .route(<devices::PublishKeyPackagesBody as DsRequest>::PATH, post(devices::publish_key_packages))
        .route(<devices::ClaimKeyPackageBody as DsRequest>::PATH, post(devices::claim_key_package))
        .route(<devices::ReplenishKeyPackagesBody as DsRequest>::PATH, post(devices::replenish_key_packages))
        .route(<devices::ResignDeviceCertsBody as DsRequest>::PATH, post(devices::resign_device_certs))
        .route(<devices::PushTokenBody as DsRequest>::PATH, post(devices::register_push_token))
        // Domains E + G (#419) — account lifecycle / identity rotation /
        // recovery / device-enrollment / security audit. All land on the MAIN
        // DB. The account-identity bootstrap (signup version-1 establishment),
        // device registration, and the enrollment *request* stay on the client's
        // direct path (see `account` module docs).
        .route(<account::RotateIdentityBody as DsRequest>::PATH, post(account::rotate_identity))
        .route(<account::DeleteAccountBody as DsRequest>::PATH, post(account::delete_account))
        .route(<account::ResetRecoverBody as DsRequest>::PATH, post(account::reset_recover))
        .route(<account::SecurityEventBody as DsRequest>::PATH, post(account::record_security_event))
        .route(<account::ApproveEnrollmentBody as DsRequest>::PATH, post(account::approve_enrollment))
        .route(<account::RejectEnrollmentBody as DsRequest>::PATH, post(account::reject_enrollment))
        .route(<account::RevokeDeviceBody as DsRequest>::PATH, post(account::revoke_device))
        // Logout device removal (bucket-C C4) — DEVICE-SIGNED DELETE of the
        // signer's OWN `user_device` row. Distinct from `/v1/devices/revoke`
        // (which tombstones): logout must re-register cleanly on next sign-in.
        .route(<account::LogoutDeviceBody as DsRequest>::PATH, post(account::logout_device))
        // Server-side OTP + bootstrap (Goal B #419). request/verify-otp generate,
        // validate, and email the OTP server-side and mint an OTP-session token;
        // the three session-gated bootstrap writes establish the device's signing
        // credential (which therefore cannot itself be device-signed). See
        // `docs/otp-server-bootstrap-design.md`.
        .route(<otp::RequestOtpBody as DsRequest>::PATH, post(otp::request_otp))
        .route(<otp::VerifyOtpBody as DsRequest>::PATH, post(otp::verify_otp))
        .route(<bootstrap::EstablishIdentityBody as DsRequest>::PATH, post(bootstrap::establish_identity))
        .route(<bootstrap::RegisterDeviceBody as DsRequest>::PATH, post(bootstrap::register_device))
        .route(<bootstrap::PublishCertBody as DsRequest>::PATH, post(bootstrap::publish_device_cert))
        // Subsequent-device (re-login) bootstrap: the session-gated enrollment
        // REQUEST write (the requesting device is still pre-credential). The
        // subsequent-device cert publish reuses publish-device-cert above, gated
        // by cert-validity ALONE (no session). See docs §5.
        .route(<bootstrap::EnrollmentRequestBody as DsRequest>::PATH, post(bootstrap::enrollment_request))
        // Email change (Goal B #419 final piece) — DEVICE-SIGNED (the user is
        // already authenticated; the OTP only proves control of the NEW mailbox).
        // request records (authed → new_email); verify requires the signed caller
        // to match and swaps `users.email`. See `email_change` module docs.
        .route(
            <email_change::RequestEmailChangeBody as DsRequest>::PATH,
            post(email_change::request_email_change_otp),
        )
        .route(
            <email_change::VerifyEmailChangeBody as DsRequest>::PATH,
            post(email_change::verify_email_change),
        )
        // Authorized-secrets broker (#393) — DEVICE-SIGNED. Move the two
        // secret-dependent on-device ops server-side: mint a LiveKit token
        // (identity derived from the verified signer, never the client) and
        // presign an R2 GET/PUT. Secrets live in DS env, never the client
        // bundle. See `broker` module docs + `docs/secrets-broker.md`.
        .route(<broker::LivekitTokenBody as DsRequest>::PATH, post(broker::livekit_token))
        .route(<broker::LivekitSendDataBody as DsRequest>::PATH, post(broker::livekit_send_data))
        .route(<broker::LivekitParticipantsBody as DsRequest>::PATH, post(broker::livekit_participants))
        .route(<broker::LivekitIdentitiesBody as DsRequest>::PATH, post(broker::livekit_identities))
        .route(<broker::R2PresignBody as DsRequest>::PATH, post(broker::r2_presign))

        // ── Directory + conversation READS (#987) ────────────────────────────
        .route(<directory::CatchUpBody as DsRequest>::PATH, post(directory::catch_up))
        .route(<directory::MessageLookupBody as DsRequest>::PATH, post(directory::message_lookup))
        .route(<directory::DirectoryBootstrapBody as DsRequest>::PATH, post(directory::bootstrap))
        .route(<directory::DirectoryGroupBody as DsRequest>::PATH, post(directory::group))
        .route(<directory::DirectoryMembersBody as DsRequest>::PATH, post(directory::members))
        .route(<directory::DirectoryUsersBody as DsRequest>::PATH, post(directory::users))
        .route(<directory::GroupBySlugBody as DsRequest>::PATH, post(directory::group_by_slug))
        .route(<directory::DirectoryConversationsBody as DsRequest>::PATH, post(directory::conversations))

        // ── Account, device and object READS (#987) ──────────────────────────
        .route(<account_reads::AccountKeysBody as DsRequest>::PATH, post(account_reads::account_keys))
        .route(<account_reads::DevicesBody as DsRequest>::PATH, post(account_reads::devices))
        .route(<account_reads::KeyPackagesBody as DsRequest>::PATH, post(account_reads::key_packages))
        .route(<account_reads::RegisteredDevicesBody as DsRequest>::PATH, post(account_reads::registered_devices))
        .route(<account_reads::EnrollmentRequestBody as DsRequest>::PATH, post(account_reads::enrollment))
        .route(<account_reads::PendingEnrollmentsBody as DsRequest>::PATH, post(account_reads::pending_enrollments))
        .route(<account_reads::RecoveryBlobBody as DsRequest>::PATH, post(account_reads::recovery_blob))
        .route(<account_reads::AccountStatusBody as DsRequest>::PATH, post(account_reads::account_status))
        .route(<account_reads::SecurityEventsBody as DsRequest>::PATH, post(account_reads::security_events))
        .route(<account_reads::EmojiReadBody as DsRequest>::PATH, post(account_reads::emoji))
        .route(<account_reads::ObjectExistsBody as DsRequest>::PATH, post(account_reads::object_exists))
        .route(<account_reads::ReadCursorsBody as DsRequest>::PATH, post(account_reads::read_cursors))
        // UNAUTHENTICATED, and it has to be: it runs before PIN entry, when the
        // local DB is closed (no signer), `device_id` is unset, and no OTP has
        // run (no session). Rate-limited on the tight `probe` tier. See #987
        // §6.5 and `account_reads::account_probe`.
        .route(<account_reads::AccountProbeBody as DsRequest>::PATH, post(account_reads::account_probe))
        // Hardening middleware (#345). Rate limiting runs first (inner); security
        // headers are added last so they wrap every response, including the
        // rate-limiter's own 429s and any error replies.
        .layer(from_fn_with_state(state.clone(), ratelimit::rate_limit))
        .layer(from_fn(headers::security_headers))
        .with_state(state)
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

/// The git SHA this binary was built from, baked in at compile time by the
/// Docker build (`ARG GIT_SHA` → `ENV GIT_SHA` → `option_env!`). `"unknown"` for
/// local `cargo` builds. Exposed at `/version` so a deploy can confirm the new
/// image is actually LIVE (not merely built) — otherwise a silently no-op'd
/// recreate leaves the service running an old version indefinitely.
pub const GIT_SHA: &str = match option_env!("GIT_SHA") {
    Some(s) => s,
    None => "unknown",
};

/// GET /version — the running build's git SHA. Open (no auth), like `/health`.
async fn version() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(serde_json::json!({ "service": "pollis-delivery", "sha": GIT_SHA })),
    )
}

/// GET /v1/retention/metrics — the latest identity-free `message_envelope` growth
/// snapshot the GC sweep last computed (#720 checkbox 1). `computed: false` until
/// the first sweep runs.
///
/// **Gated by an operator bearer token — NOT open like `/health` and `/version`.**
/// Those two return a service name and a git sha; this returns the total envelope
/// count, how many conversations hold envelopes, the oldest retained `sent_at`, and
/// the worst offender's shape. Even though it carries no conversation id or user id
/// (identity-free per `docs/metadata-retention-policy.md` §3), that is a public,
/// pollable activity meter for a privacy-first messenger: scraped on a schedule it
/// reveals how many conversations exist, how fast the network is growing, and when
/// it is busy — "traffic spiked at 14:00 UTC" is exactly the aggregate metadata this
/// product exists not to disclose. §3's identity-free rule is necessary, not
/// sufficient; a comment telling the owner to "restrict it at the edge" is a control
/// substituted for by a comment (default-exposed, depends on the owner remembering,
/// silently public again if the Cloudflare rule is ever lost). Default-closed here
/// is the only version that holds.
///
/// So: `POLLIS_DS_METRICS_TOKEN` unset → the endpoint is not served (404 — a bare
/// status, deliberately not advertising the route). Set → callers must present
/// `Authorization: Bearer <token>`; a wrong/absent token is 401. The consumer is the
/// owner's scraper (a `curl` from their console), which is why this is a static
/// operator secret and not the device-signed `X-Pollis-*` path — there is no client
/// device on this call to hold a device key.
///
/// This remains the SOURCE OF TRUTH for the numbers only. Alert ROUTING —
/// thresholds/paging in Cloudflare/Grafana/email — is the owner's console and is
/// explicitly out of scope.
/// `GET /v1/config` — the config the DS is **actually running with**, gated by the
/// same operator bearer token as the metrics route (#760).
///
/// Exists because deployed config was unverifiable from outside, and that let two
/// values sit inert in production without anyone noticing. A DS config value has
/// to cross five boundaries — Doppler, the sync script's key list, the wrangler
/// binding, the Worker's env forwarding, then the process — and #720's two values
/// were silently dropped at the fourth. The deploy went green, the secret existed
/// in the store, the binding was registered, and the DS still never saw either.
///
/// The startup log already reports this, but nothing can read it: `wrangler tail`
/// surfaces Worker logs, not container stdout (17k lines across a forced roll,
/// zero DS output). So the log was a self-report nobody could hear.
///
/// NEVER returns a secret's value — only whether one is set. A wrong answer here
/// is worth more than a hidden one: the whole point is to make "did the config
/// actually arrive" answerable in one request.
async fn effective_config(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(expected) = state.metrics_token.as_deref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !bearer_token_matches(&headers, expected) {
        return AuthRejection::Unauthorized.into_response();
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({
            // Effective, not configured: the SQLite modifier the GC actually
            // binds, so a var that never arrived shows its fallback here.
            "watermark_stale_modifier": messages::watermark_stale_modifier(),
            // Also effective, not configured: the same function
            // `spawn_envelope_gc_sweep` binds. Reading the raw env string here let
            // this endpoint answer "1h" while the sweep ran at 3600 — the exact
            // class of lie it exists to prevent.
            "gc_sweep_secs": messages::gc_sweep_secs(),
            "require_auth": state.require_auth,
            // #847 — the invite-link redemption tier. Surfaced for the same
            // reason as the two above: a brute-force bound that silently fell
            // back to the generic 1200/60s `write` tier would look configured
            // and defend nothing.
            "invite_redeem_max": state.ratelimit_config.invite_redeem_max,
            "invite_redeem_window_secs": state.ratelimit_config.invite_redeem_window_secs,
            // Presence only — never the token itself.
            "metrics_token_set": true,
        })),
    )
        .into_response()
}

async fn retention_metrics(State(state): State<AppState>, headers: HeaderMap) -> Response {
    // Fail closed: no token configured → the endpoint does not exist. 404 (a bare
    // status, no body) so an unconfigured deploy does not even advertise the route.
    let Some(expected) = state.metrics_token.as_deref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // Configured → require a matching bearer token. Wrong/absent → 401.
    if !bearer_token_matches(&headers, expected) {
        return AuthRejection::Unauthorized.into_response();
    }
    match state.retention_metrics.lock().unwrap().clone() {
        Some(snapshot) => (
            StatusCode::OK,
            Json(serde_json::json!({ "computed": true, "metrics": snapshot })),
        )
            .into_response(),
        None => (
            StatusCode::OK,
            Json(serde_json::json!({ "computed": false, "metrics": null })),
        )
            .into_response(),
    }
}

/// Whether the `Authorization: Bearer <token>` header carries exactly `expected`.
/// Constant-time over the token bytes so a wrong guess cannot be narrowed by timing
/// (same discipline as `otp::constant_time_eq`). A missing/malformed header, or the
/// wrong scheme, is simply a non-match.
fn bearer_token_matches(headers: &HeaderMap, expected: &str) -> bool {
    let Some(value) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let Some(presented) = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))
    else {
        return false;
    };
    let (a, b) = (presented.as_bytes(), expected.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// POST /v1/commits — submit a commit. When auth is enforced, the request must
/// carry a valid device-cert signature and the commit's `sender_id` must equal
/// the authenticated user; otherwise 401/403. On success: 200 Accepted (won the
/// epoch) or 409 Rejected (not at head; body carries head + missing commits).
///
/// Takes the raw [`Bytes`] (not `Json<SubmitBody>`) because the signature binds
/// `sha256(body)` — we must hash the exact wire bytes before deserializing.
async fn submit(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    // Verify the signature over the *raw* body before parsing. The path is
    // taken from the matched URI (no query on this route).
    let authed_user = if state.require_auth {
        let conn = state.db.conn().await?;
        match auth::verify_request_cached(
            &state.device_keys,
            &conn,
            &headers,
            method.as_str(),
            uri.path(),
            &body,
            util::now_unix() as i64,
        )
        .await
        {
            Ok(user_id) => Some(user_id),
            Err(rej) => return Ok(rej.into_response()),
        }
    } else {
        None
    };

    // Parse after the signature check so a forged body can't even reach the DB.
    let parsed: SubmitBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        // Malformed JSON is a client error, not a server error.
        Err(_) => {
            return Ok((
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "invalid body" })),
            )
                .into_response())
        }
    };

    // Identity binding: a validly-signed request may only write as itself.
    if let Some(user_id) = &authed_user {
        if parsed.sender_id != *user_id {
            return Ok(AuthRejection::Forbidden.into_response());
        }
        // Authz: only a CURRENT member may submit a commit for this conversation
        // — the committer must already be in the group (a commit that adds a new
        // member is authored by an existing one). Mirrors `/v1/group-info`'s
        // membership gate; skipped on the no-auth path.
        let conn = state.db.conn().await?;
        if !writes::is_member(&conn, &parsed.conversation_id, user_id).await? {
            return Ok(AuthRejection::Forbidden.into_response());
        }
    }

    // The MLS control-plane tables live on the commit-log DB (== main DB when no
    // separate log DB is configured).
    let conn = state.log_db.conn().await?;
    // Bound to the endpoint's DECLARED response type (#922), not merely to
    // whatever `submit_commit` happens to return: `SubmitResponse` now lives in
    // `pollis-api` next to `SubmitBody`, and this annotation is what makes a
    // drift between the two a compile error here rather than a decode surprise
    // on a client.
    let outcome: <SubmitBody as DsRequest>::Response =
        commit::submit_commit(&conn, &parsed).await?;

    // Retention floor (#539, I4): a landed commit advanced the head, so run an
    // EVENT-DRIVEN prune (no timer — repo rule). Best-effort: a prune failure must
    // never fail an accepted commit. Membership is read on the MAIN DB, the log is
    // pruned on the LOG DB.
    if matches!(outcome, SubmitResponse::Accepted { .. }) {
        if let Ok(main_conn) = state.db.conn().await {
            if let Err(e) = commit::prune_commit_log(&main_conn, &conn, &parsed.conversation_id).await
            {
                tracing::warn!(error = %e, conversation_id = %parsed.conversation_id, "commit-log prune (on submit) failed");
            }
        }
    }

    let code = match &outcome {
        SubmitResponse::Accepted { .. } => StatusCode::OK,
        SubmitResponse::Rejected { .. } => StatusCode::CONFLICT,
    };
    Ok((code, Json(outcome)).into_response())
}

#[derive(Deserialize)]
struct Since {
    #[serde(default)]
    since: i64,
    /// The suite generation being caught up on (#454 P4). Absent (a pre-hybrid
    /// client) → 0, which is the lineage such a client is in, so its request and
    /// the `head` it reads back are unchanged.
    #[serde(default)]
    generation: i64,
}

/// GET /v1/commits/:conversation_id?since=N[&generation=G] — the contiguous
/// commit log of generation G (default 0) from epoch N (default 0) to that
/// lineage's head. Reads are open (unauthenticated) and have NO side effects.
///
/// The catch-up high-water report that FEEDS the retention floor is a SEPARATE,
/// AUTHENTICATED write — `POST /v1/commits/since` ([`report_commit_since`]).
/// Recording it here off unsigned `user_id`/`device_id` query params (as this
/// handler used to) let anyone raise the floor on behalf of any device, which —
/// chained with an unclamped [`commit::prune_floor`] — was a remote,
/// unauthenticated, per-conversation commit-log wipe (#681). Any stray
/// `user_id`/`device_id` query params an older client still appends are simply
/// ignored now; its report is dropped, which only ever leaves the floor MORE
/// conservative (an unreported member disables Tier 1).
async fn commits(
    State(state): State<AppState>,
    Path(conversation_id): Path<String>,
    Query(q): Query<Since>,
) -> Result<impl IntoResponse, AppError> {
    let conn = state.log_db.conn().await?;

    // `head` is the head of the lineage the caller ASKED for — that is what it
    // must drain — while `head_generation` is how it learns a newer one exists.
    let head = commit::head_epoch_in(&conn, &conversation_id, q.generation).await?;
    let head_generation = commit::head_generation(&conn, &conversation_id).await?;
    let commits = commit::fetch_commits(&conn, &conversation_id, q.generation, q.since).await?;
    Ok(Json(CommitsResponse {
        head,
        generation: q.generation,
        head_generation,
        commits,
    }))
}

/// POST /v1/commits/since — record the signing device's catch-up high-water and
/// run the EVENT-DRIVEN retention prune (#539, I4). This is a WRITE: it raises
/// the retention floor, so it is AUTHENTICATED as the device it claims to be,
/// exactly like `POST /v1/commits` — reuse [`auth::verify_request_identity`], the
/// four `X-Pollis-*` headers, and its refusal to authenticate a revoked device.
///
/// The report is bound to the device by signature, so nobody can raise the floor
/// on another device's behalf (#681). An unauthenticated report is REJECTED
/// (401) rather than served: there is no read to protect on this endpoint (the
/// open read is `GET /v1/commits`), and dropping the report only ever leaves the
/// floor conservative — a device we cannot authenticate stays UNREPORTED, which
/// disables Tier 1 for the roster, so a rejected report can never WIDEN pruning.
///
/// Takes the raw [`Bytes`] because the signature binds `sha256(body)` — the
/// reported `(conversation_id, generation, since)` is authenticated by that hash,
/// which a signed empty-body GET with the values in the query string could not
/// have provided.
async fn report_commit_since(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    // Authenticate first, over the RAW body, so a forged report can't reach the
    // DB. `(user_id, device_id)` come from the verified signature — never from
    // the body — when auth is on.
    let authed = if state.require_auth {
        let conn = state.db.conn().await?;
        match auth::verify_request_identity_cached(
            &state.device_keys,
            &conn,
            &headers,
            method.as_str(),
            uri.path(),
            &body,
            util::now_unix() as i64,
        )
        .await
        {
            Ok(id) => Some(id),
            Err(rej) => return Ok(rej.into_response()),
        }
    } else {
        None
    };

    let parsed: CommitSinceReport = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => {
            return Ok((
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "invalid body" })),
            )
                .into_response())
        }
    };

    // Resolve the reporting identity. Auth on: the verified signer, and the body
    // may only report as itself. Auth off (dev/test): the body-declared identity,
    // mirroring how `submit` trusts `sender_id` on the no-auth path.
    let (user_id, device_id) = match &authed {
        Some((u, d)) => (u.clone(), d.clone()),
        None => match (parsed.user_id.clone(), parsed.device_id.clone()) {
            (Some(u), Some(d)) if !u.is_empty() && !d.is_empty() => (u, d),
            // No identity to attribute the report to → nothing to record.
            _ => return Ok(crate::writes::ok_empty::<CommitSinceReport>()),
        },
    };
    if let Some((authed_user, _)) = &authed {
        if let Some(claimed) = parsed.user_id.as_deref() {
            if !claimed.is_empty() && claimed != authed_user {
                return Ok(AuthRejection::Forbidden.into_response());
            }
        }
    }

    // Record the high-water, then re-compute + apply the floor. Best-effort: a
    // failure here must not fail the request. Membership is read on MAIN, the
    // log + high-waters on LOG.
    let conn = state.log_db.conn().await?;
    if let Err(e) = commit::record_commit_since(
        &conn,
        &parsed.conversation_id,
        &user_id,
        &device_id,
        parsed.generation,
        parsed.since,
    )
    .await
    {
        tracing::warn!(error = %e, conversation_id = %parsed.conversation_id, "record commit-since failed");
    } else if let Ok(main_conn) = state.db.conn().await {
        if let Err(e) = commit::prune_commit_log(&main_conn, &conn, &parsed.conversation_id).await {
            tracing::warn!(error = %e, conversation_id = %parsed.conversation_id, "commit-log prune (on catch-up) failed");
        }
    }

    Ok(crate::writes::ok_empty::<CommitSinceReport>())
}

#[cfg(test)]
mod require_auth_gate_tests {
    use super::require_auth_from_value;

    /// The #921 fix itself: an unconfigured DS ENFORCES. This is the assertion
    /// that flips against the old parser, which read "unset" as "no auth".
    #[test]
    fn an_unconfigured_ds_enforces_auth() {
        assert!(
            require_auth_from_value(None),
            "an operator who sets nothing must get the safe configuration, not an \
             open one — with the gate off every write endpoint takes its actor \
             from a client-controlled body field"
        );
    }

    /// Turning it off takes a word the parser recognises — and only those. Every
    /// other value, including near-misses and typos of "true", enforces.
    #[test]
    fn only_an_explicit_off_value_disables_enforcement() {
        for off in ["false", "FALSE", "False", "0", "no", "NO", "off", "OFF", "  false  "] {
            assert!(
                !require_auth_from_value(Some(off)),
                "{off:?} is an explicit opt-out and must disable enforcement"
            );
        }
        for on in ["true", "TRUE", "True", "1", "yes", "on", "", "   ", "ture", "flase", "maybe"] {
            assert!(
                require_auth_from_value(Some(on)),
                "{on:?} is not a recognised opt-out, so it must ENFORCE — a typo \
                 must never be read as permission to trust every caller"
            );
        }
    }

    /// `require_auth_from_env` really is [`require_auth_from_value`] applied to
    /// the variable, so the rules above are about the shipped gate and not about
    /// a parser nothing calls.
    ///
    /// Reads the ambient environment rather than setting it: `set_var` mutates
    /// process-global state that every other test in this binary is concurrently
    /// reading (`watermark_stale_modifier`, `gc_sweep_secs`, `OtpConfig::from_env`
    /// …), which is a data race — the reason Rust 2024 made `set_var` `unsafe`.
    /// Nothing is lost by not setting it: CI and every dev machine run with the
    /// variable unset, so this asserts delegation on exactly the `None` case the
    /// #921 fix is about, and the table above covers the rest of the rule.
    #[test]
    fn from_env_delegates_to_the_tested_rule() {
        let ambient = std::env::var("POLLIS_DS_REQUIRE_AUTH").ok();
        assert_eq!(
            super::require_auth_from_env(),
            require_auth_from_value(ambient.as_deref()),
            "the shipped gate must be the rule tested above, applied to \
             POLLIS_DS_REQUIRE_AUTH={ambient:?}"
        );
    }
}
