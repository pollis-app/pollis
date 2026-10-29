//! Pollis MLS Delivery Service.
//!
//! The sole writer to the MLS control-plane tables; it sees only opaque blobs (RFC 9420 DS role).
//!
//! ## Write authentication ([`auth`])
//!
//! Every write is device-signed; only `/health`, `/version` and two operator-token GETs are open.
//!
//! Enforcement is **ON by default** (#921); see [`require_auth_from_value`]. Off is for local dev only.

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
pub mod error_log;
pub mod groups;
pub mod links;
pub mod headers;
pub mod invite_token;
pub mod last_seen;
pub mod messages;
pub mod ops_log;
pub mod otp;
pub mod participant_id;
pub mod file_keys;
pub mod pins;
pub mod profile;
pub mod push;
pub mod ratelimit;
pub mod reads;
pub mod reports;
pub mod redact;
pub mod room_id;
pub mod session;
pub mod teardown;
pub mod util;
pub mod vault;
pub mod writes;

use std::sync::Arc;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    middleware::{from_fn, from_fn_with_state},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use pollis_api::DsRequest;

use crate::commit::{CommitSinceReport, SubmitBody, SubmitResponse};
use crate::db::Db;
use crate::error::{AppError, AuthRejection};
use crate::writes::RawRequest;

/// Shared handler state: the DBs plus whether write auth is enforced.
#[derive(Clone)]
pub struct AppState {
    /// Main DB — user/device metadata (auth lookups live here).
    pub db: Arc<Db>,
    /// Commit-log DB (MLS control-plane tables); the same handle as `db` when none is configured.
    pub log_db: Arc<Db>,
    /// When true, `POST /v1/commits` requires a valid device signature.
    pub require_auth: bool,
    /// In-memory OTP store (request-otp / verify-otp).
    pub otp: otp::OtpStore,
    /// In-memory OTP-session store gating the bootstrap writes.
    pub sessions: session::SessionStore,
    /// QR device links (#1207). In-memory, like `sessions`.
    pub links: links::LinkStore,
    /// OTP/session tunables + the Resend key (DS env).
    pub otp_config: otp::OtpConfig,
    /// Email-change OTP store + requester binding.
    pub email_change: email_change::EmailChangeStore,
    /// Authorized-secrets broker config (#393); endpoints 503 until configured.
    pub broker: broker::BrokerConfig,
    /// In-memory per-IP rate limiter.
    pub ratelimit: ratelimit::RateLimiter,
    /// Per-IP rate-limit tunables (DS env).
    pub ratelimit_config: ratelimit::RateLimitConfig,
    /// In-memory device-pubkey cache (#658); see [`auth::DeviceKeyCache`].
    pub device_keys: auth::DeviceKeyCache,
    /// Latest identity-free envelope-growth snapshot (#720), from the GC sweep.
    pub retention_metrics: messages::RetentionMetricsHandle,
    /// Operator token for `GET /v1/retention/metrics` (#720); `None` → 404, as it is an activity meter.
    pub metrics_token: Option<String>,
    /// Dev-only ops log ([`ops_log`]); `None` in production.
    pub ops_log: Option<ops_log::OpsLog>,
    /// Write budget for the failed-request log ([`error_log`], #1305).
    pub error_log: error_log::ErrorLog,
}

impl AppState {
    /// Single-DB state; `main` swaps in [`otp::OtpConfig::from_env`].
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
            links: links::LinkStore::default(),
            otp_config: otp::OtpConfig::default(),
            email_change: email_change::EmailChangeStore::default(),
            broker: broker::BrokerConfig::default(),
            ratelimit: ratelimit::RateLimiter::default(),
            ratelimit_config: ratelimit::RateLimitConfig::default(),
            device_keys: auth::DeviceKeyCache::default(),
            retention_metrics: messages::RetentionMetricsHandle::default(),
            // Default-closed; `main` opts in via `with_metrics_token`.
            metrics_token: None,
            // Default-off, like the metrics token: `main` opts in from env.
            ops_log: None,
            error_log: error_log::ErrorLog::default(),
        }
    }

    /// Override the OTP config (Resend key, `DEV_OTP`, TTLs).
    pub fn with_otp_config(mut self, config: otp::OtpConfig) -> Self {
        self.otp_config = config;
        self
    }

    /// Override the broker config (LiveKit + R2 secrets).
    pub fn with_broker_config(mut self, config: broker::BrokerConfig) -> Self {
        self.broker = config;
        self
    }

    /// Override the per-IP rate-limit config.
    pub fn with_ratelimit_config(mut self, config: ratelimit::RateLimitConfig) -> Self {
        self.ratelimit_config = config;
        self
    }

    /// Set the operator token for `GET /v1/retention/metrics`; `None` leaves it 404.
    pub fn with_metrics_token(mut self, token: Option<String>) -> Self {
        self.metrics_token = token;
        self
    }

    /// Turn the dev ops log ([`ops_log`]) on or off. `main` passes
    /// [`ops_log::enabled_from_env`]; tests opt in explicitly.
    pub fn with_ops_log(mut self, enabled: bool) -> Self {
        self.ops_log = enabled.then(ops_log::OpsLog::default);
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
/// It defaulted OFF until #921, which was safe only while a bug survived.
///
/// Fails closed on garbage: a typo means "enforce".
fn require_auth_from_value(raw: Option<&str>) -> bool {
    !matches!(
        raw.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("false") | Some("0") | Some("no") | Some("off")
    )
}

/// Build the router from a bare DB (the commit log shares it), reading the auth gate from env.
pub fn build_router(db: Arc<Db>) -> Router {
    let log_db = Arc::clone(&db);
    build_router_with_log_db(db, log_db)
}

/// Like [`build_router`], with a separate commit-log DB.
pub fn build_router_with_log_db(db: Arc<Db>, log_db: Arc<Db>) -> Router {
    build_router_with_state(build_app_state(db, log_db))
}

/// Build the [`AppState`] from env; separate so `main` can hold it for the GC sweep.
pub fn build_app_state(db: Arc<Db>, log_db: Arc<Db>) -> AppState {
    let require_auth = require_auth_from_env();
    if require_auth {
        tracing::info!(
            require_auth,
            "pollis-delivery write auth: ENFORCED (device-cert signature required on writes)"
        );
    } else {
        // `error!`, not `info!` (#921): with auth off any client may write as anyone.
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
    let ops_log = ops_log::enabled_from_env();
    if ops_log {
        tracing::info!(
            ops_log,
            "pollis-delivery ops log: ENABLED (dev) — request-otp and rate-limited requests are \
             recorded by client pseudonym; GET /v1/ops/otp-requests (operator token)"
        );
    }
    AppState::new_with_log_db(db, log_db, require_auth)
        .with_otp_config(otp::OtpConfig::from_env())
        .with_broker_config(broker::BrokerConfig::from_env())
        .with_ratelimit_config(ratelimit::RateLimitConfig::from_env())
        .with_metrics_token(metrics_token)
        .with_ops_log(ops_log)
}

/// Read `POLLIS_DS_METRICS_TOKEN`; empty counts as unset.
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
        .route("/v1/ops/otp-requests", get(ops_log::otp_requests))
        .route("/v1/ops/errors", get(error_log::list))
        .route("/v1/ops/push", get(push::ops_push))
        .route(<SubmitBody as DsRequest>::PATH, post(submit))
        .route(<CommitSinceReport as DsRequest>::PATH, post(report_commit_since))
        .route(<writes::GroupInfoBody as DsRequest>::PATH, post(writes::group_info))
        .route(<writes::AckBody as DsRequest>::PATH, post(writes::welcomes_ack))
        .route(<writes::ResetBody as DsRequest>::PATH, post(writes::welcomes_reset))
        .route(<writes::PurgeBody as DsRequest>::PATH, post(writes::welcomes_purge))
        .route(<writes::ResubmitBody as DsRequest>::PATH, post(writes::welcomes_resubmit))
        // MLS control-plane READS (#987): POST, as signing excludes the query string.
        .route(<reads::FetchWelcomesBody as DsRequest>::PATH, post(reads::fetch_welcomes))
        .route(<reads::ConversationStateBody as DsRequest>::PATH, post(reads::conversation_state))
        // Domain A (#419) — messages / envelopes / watermarks / reactions / attachments.
        .route(<messages::SendMessageBody as DsRequest>::PATH, post(messages::send_message))
        .route(<messages::EditMessageBody as DsRequest>::PATH, post(messages::edit_message))
        .route(<messages::DeleteMessageBody as DsRequest>::PATH, post(messages::delete_message))
        .route(<messages::AddReaction as DsRequest>::PATH, post(messages::add_reaction))
        .route(<messages::RemoveReaction as DsRequest>::PATH, post(messages::remove_reaction))
        .route(<messages::WatermarkBody as DsRequest>::PATH, post(messages::advance_watermark))
        .route(<messages::EnvelopeGcBody as DsRequest>::PATH, post(messages::envelope_gc))
        .route(<messages::AttachmentRegisterBody as DsRequest>::PATH, post(messages::register_attachment))
        .route(<messages::AttachmentDeleteBody as DsRequest>::PATH, post(messages::delete_attachment))
        // Domain B (#419) — groups / channels / membership / invites / join-requests.
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
        .route(<groups::RevokeInviteBody as DsRequest>::PATH, post(groups::revoke_invite))
        .route(<groups::CreateInviteLinkBody as DsRequest>::PATH, post(groups::create_invite_link))
        .route(<groups::RevokeInviteLinkBody as DsRequest>::PATH, post(groups::revoke_invite_link))
        .route(<groups::RedeemInviteLinkBody as DsRequest>::PATH, post(groups::redeem_invite_link))
        .route(<groups::CreateJoinRequestBody as DsRequest>::PATH, post(groups::create_join_request))
        .route(<groups::ApproveJoinRequestBody as DsRequest>::PATH, post(groups::approve_join_request))
        .route(<groups::RejectJoinRequestBody as DsRequest>::PATH, post(groups::reject_join_request))
        // Domain F (#848) — custom per-group emoji (see the `emoji` module docs).
        .route(<emoji::CreateEmojiBody as DsRequest>::PATH, post(emoji::create_emoji))
        .route(<emoji::RemoveEmojiBody as DsRequest>::PATH, post(emoji::remove_emoji))
        .route(<emoji::EmojiGcBody as DsRequest>::PATH, post(emoji::emoji_gc))
        // Domain C (#419) — profile / preferences / blocks / DMs.
        .route(<profile::UpdateProfileBody as DsRequest>::PATH, post(profile::update_profile))
        .route(<profile::SavePreferencesBody as DsRequest>::PATH, post(profile::save_preferences))
        .route(<profile::AddBlock as DsRequest>::PATH, post(profile::block_user))
        .route(<profile::RemoveBlock as DsRequest>::PATH, post(profile::unblock_user))
        // Abuse reports (#1213): ids and a reason, never content.
        .route(<pollis_api::reports::ReportUserBody as DsRequest>::PATH, post(reports::report_user))
        .route(<profile::CreateDmBody as DsRequest>::PATH, post(profile::create_dm))
        .route(<profile::AcceptDmBody as DsRequest>::PATH, post(profile::accept_dm))
        .route(<profile::AddDmMemberBody as DsRequest>::PATH, post(profile::add_dm_member))
        .route(<profile::RemoveDmMemberBody as DsRequest>::PATH, post(profile::remove_dm_member))
        .route(<profile::LeaveDmBody as DsRequest>::PATH, post(profile::leave_dm))
        // The encrypted read-cursor push (#844).
        .route(<profile::SaveReadCursorsBody as DsRequest>::PATH, post(profile::save_read_cursors))
        // Per-channel mute (#1431), plaintext for the push fan-out.
        .route(<profile::SetChannelMuteBody as DsRequest>::PATH, post(profile::set_channel_mute))
        .route(<profile::ChannelMutesBody as DsRequest>::PATH, post(profile::read_channel_mutes))
        // Pinned messages (#99): ciphertext only.
        .route(<pins::UpsertPinKeystateBody as DsRequest>::PATH, post(pins::upsert_keystate))
        .route(<pins::PinMessageBody as DsRequest>::PATH, post(pins::pin_message))
        .route(<pins::UnpinMessageBody as DsRequest>::PATH, post(pins::unpin_message))
        .route(<pins::PinKeystateBody as DsRequest>::PATH, post(pins::read_keystate))
        .route(<pins::ListPinsBody as DsRequest>::PATH, post(pins::read_pins))
        // The attachment key ring (#1382).
        .route(<file_keys::UpsertFileKeystateBody as DsRequest>::PATH, post(file_keys::upsert_keystate))
        .route(<file_keys::FileKeystateBody as DsRequest>::PATH, post(file_keys::read_keystate))
        // The Vault (#107).
        .route(<vault::SaveVaultMessageBody as DsRequest>::PATH, post(vault::save_vault_message))
        .route(<vault::DeleteVaultMessageBody as DsRequest>::PATH, post(vault::delete_vault_message))
        .route(<vault::VaultMessagesBody as DsRequest>::PATH, post(vault::read_vault))
        // Domain D (#419) — key-packages / device-cert re-sign / push tokens.
        .route(<devices::PublishKeyPackagesBody as DsRequest>::PATH, post(devices::publish_key_packages))
        .route(<devices::ClaimKeyPackageBody as DsRequest>::PATH, post(devices::claim_key_package))
        .route(<devices::ReplenishKeyPackagesBody as DsRequest>::PATH, post(devices::replenish_key_packages))
        .route(<devices::ResignDeviceCertsBody as DsRequest>::PATH, post(devices::resign_device_certs))
        .route(<devices::PushTokenBody as DsRequest>::PATH, post(devices::register_push_token))
        .route(<devices::ResolvePushHandleBody as DsRequest>::PATH, post(push::resolve_push_handle))
        // #1508: a read clears its notifications on every device.
        .route(<devices::DismissPushBody as DsRequest>::PATH, post(push::dismiss_push))
        .route(<devices::DismissedPushHandlesBody as DsRequest>::PATH, post(push::read_dismissed_push_handles))
        // Domains E + G (#419) — account lifecycle / identity / recovery / enrollment / audit.
        .route(<account::RotateIdentityBody as DsRequest>::PATH, post(account::rotate_identity))
        .route(<account::DeleteAccountBody as DsRequest>::PATH, post(account::delete_account))
        .route(<account::ResetRecoverBody as DsRequest>::PATH, post(account::reset_recover))
        .route(<account::SecurityEventBody as DsRequest>::PATH, post(account::record_security_event))
        .route(<account::ApproveEnrollmentBody as DsRequest>::PATH, post(account::approve_enrollment))
        .route(<account::RejectEnrollmentBody as DsRequest>::PATH, post(account::reject_enrollment))
        .route(<account::RevokeDeviceBody as DsRequest>::PATH, post(account::revoke_device))
        // Logout: DEVICE-SIGNED DELETE of the signer's own row (unlike revoke's tombstone).
        .route(<account::LogoutDeviceBody as DsRequest>::PATH, post(account::logout_device))
        // Server-side OTP + the session-gated bootstrap writes (#419).
        .route(<otp::RequestOtpBody as DsRequest>::PATH, post(otp::request_otp))
        .route(<otp::VerifyOtpBody as DsRequest>::PATH, post(otp::verify_otp))
        .route(<bootstrap::EstablishIdentityBody as DsRequest>::PATH, post(bootstrap::establish_identity))
        .route(<bootstrap::RegisterDeviceBody as DsRequest>::PATH, post(bootstrap::register_device))
        .route(<bootstrap::PublishCertBody as DsRequest>::PATH, post(bootstrap::publish_device_cert))
        // Subsequent-device enrollment request (session-gated, pre-credential).
        .route(<bootstrap::EnrollmentRequestBody as DsRequest>::PATH, post(bootstrap::enrollment_request))
        // QR device links (#1207): create/status are device-signed; claim is
        // pre-credential and mints an enrollment-only session.
        .route(<links::CreateLinkBody as DsRequest>::PATH, post(links::create_link))
        .route(<links::ClaimLinkBody as DsRequest>::PATH, post(links::claim_link))
        .route(<links::LinkStatusBody as DsRequest>::PATH, post(links::link_status))
        // The reverse direction (#1361): offer and poll are pre-credential, accept device-signed.
        .route(<links::OfferLinkBody as DsRequest>::PATH, post(links::offer_link))
        .route(<links::AcceptOfferBody as DsRequest>::PATH, post(links::accept_offer))
        .route(<links::PollOfferBody as DsRequest>::PATH, post(links::poll_offer))
        // Email change (#419) — DEVICE-SIGNED; see `email_change`.
        .route(
            <email_change::RequestEmailChangeBody as DsRequest>::PATH,
            post(email_change::request_email_change_otp),
        )
        .route(
            <email_change::VerifyEmailChangeBody as DsRequest>::PATH,
            post(email_change::verify_email_change),
        )
        // Authorized-secrets broker (#393) — DEVICE-SIGNED; see `broker`.
        .route(<broker::LivekitTokenBody as DsRequest>::PATH, post(broker::livekit_token))
        .route(<broker::LivekitSendDataBody as DsRequest>::PATH, post(broker::livekit_send_data))
        .route(<broker::LivekitParticipantsBody as DsRequest>::PATH, post(broker::livekit_participants))
        .route(<broker::LivekitIdentitiesBody as DsRequest>::PATH, post(broker::livekit_identities))
        .route(<broker::R2PresignBody as DsRequest>::PATH, post(broker::r2_presign))

        // Directory + conversation READS (#987)
        .route(<directory::CatchUpBody as DsRequest>::PATH, post(directory::catch_up))
        .route(<directory::MessageLookupBody as DsRequest>::PATH, post(directory::message_lookup))
        .route(<directory::DirectoryBootstrapBody as DsRequest>::PATH, post(directory::bootstrap))
        .route(<directory::DirectoryGroupBody as DsRequest>::PATH, post(directory::group))
        .route(<directory::DirectoryMembersBody as DsRequest>::PATH, post(directory::members))
        .route(<directory::DirectoryUsersBody as DsRequest>::PATH, post(directory::users))
        .route(<directory::GroupBySlugBody as DsRequest>::PATH, post(directory::group_by_slug))
        .route(<directory::DirectoryConversationsBody as DsRequest>::PATH, post(directory::conversations))

        // Account, device and object READS (#987)
        .route(<account_reads::AccountKeysBody as DsRequest>::PATH, post(account_reads::account_keys))
        .route(<account_reads::DevicesBody as DsRequest>::PATH, post(account_reads::devices))
        .route(<account_reads::KeyPackagesBody as DsRequest>::PATH, post(account_reads::key_packages))
        .route(<account_reads::RegisteredDevicesBody as DsRequest>::PATH, post(account_reads::registered_devices))
        .route(<account_reads::RosterIdentitiesBody as DsRequest>::PATH, post(account_reads::roster_identities))
        .route(<account_reads::EnrollmentRequestBody as DsRequest>::PATH, post(account_reads::enrollment))
        .route(<account_reads::PendingEnrollmentsBody as DsRequest>::PATH, post(account_reads::pending_enrollments))
        .route(<account_reads::RecoveryBlobBody as DsRequest>::PATH, post(account_reads::recovery_blob))
        .route(<account_reads::AccountStatusBody as DsRequest>::PATH, post(account_reads::account_status))
        .route(<account_reads::SecurityEventsBody as DsRequest>::PATH, post(account_reads::security_events))
        .route(<account_reads::EmojiReadBody as DsRequest>::PATH, post(account_reads::emoji))
        .route(<account_reads::ObjectExistsBody as DsRequest>::PATH, post(account_reads::object_exists))
        .route(<account_reads::ReadCursorsBody as DsRequest>::PATH, post(account_reads::read_cursors))
        // UNAUTHENTICATED by necessity (pre-PIN); `probe` tier (#987 §6.5).
        .route(<account_reads::AccountProbeBody as DsRequest>::PATH, post(account_reads::account_probe))
        // Failed-request log (#1305): a ROUTE layer (matched patterns only), inside the limiter.
        .route_layer(from_fn_with_state(state.clone(), error_log::record))
        // Hardening (#345): rate limiting inner; security headers wrap everything, 429s included.
        .layer(from_fn_with_state(state.clone(), ratelimit::rate_limit))
        .layer(from_fn(headers::security_headers))
        .with_state(state)
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

/// The git SHA baked in by the Docker build (`"unknown"` locally), so a deploy can confirm it.
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

/// `GET /v1/config` — the config the DS is **actually running with**, gated by the
/// same operator bearer token as the metrics route (#760).
///
/// Deployed config was unverifiable from outside, which let #720's values sit inert.
///
/// NEVER returns a secret's value, only whether one is set.
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
            // The same function `spawn_envelope_gc_sweep` binds, not the raw env string.
            "gc_sweep_secs": messages::gc_sweep_secs(),
            "require_auth": state.require_auth,
            // #847: a silently-defaulted brute-force bound would look configured and defend nothing.
            "invite_redeem_max": state.ratelimit_config.invite_redeem_max,
            "invite_redeem_window_secs": state.ratelimit_config.invite_redeem_window_secs,
            // The OTP tiers dev turns off; `"off"` = none.
            "request_otp_max": ratelimit::max_json(state.ratelimit_config.request_otp_max),
            "request_otp_window_secs": state.ratelimit_config.request_otp_window_secs,
            "verify_otp_max": ratelimit::max_json(state.ratelimit_config.verify_otp_max),
            "verify_otp_window_secs": state.ratelimit_config.verify_otp_window_secs,
            // Whether the dev ops log is recording (`POLLIS_DS_OPS_LOG`).
            "ops_log": state.ops_log.is_some(),
            // Presence only — never the token itself.
            "metrics_token_set": true,
        })),
    )
        .into_response()
}

async fn retention_metrics(State(state): State<AppState>, headers: HeaderMap) -> Response {
    // Fail closed: no token configured → a bare 404.
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

/// Constant-time `Authorization: Bearer <token>` check; anything malformed is a non-match.
pub(crate) fn bearer_token_matches(headers: &HeaderMap, expected: &str) -> bool {
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
/// Takes raw [`Bytes`]: the signature binds `sha256(body)`.
async fn submit(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    // Verify the signature over the *raw* body before parsing.
    let authed_user = if state.require_auth {
        match auth::verify_request_cached(
            &state.device_keys,
            &state.db,
            &req.headers,
            req.method.as_str(),
            req.uri.path(),
            &req.body,
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
    let parsed: SubmitBody = match serde_json::from_slice(&req.body) {
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
        // Authz: only a CURRENT member may submit a commit; skipped on the no-auth path.
        let conn = state.db.conn().await?;
        if !writes::is_member(&conn, &parsed.conversation_id, user_id).await? {
            return Ok(AuthRejection::Forbidden.into_response());
        }
    }

    // Control-plane tables on the log DB; membership (Welcome checks, C1) on main.
    let conn = state.log_db.conn().await?;
    let main_conn = state.db.conn().await?;
    // Bound to the DECLARED response type (#922), so drift is a compile error.
    let outcome: <SubmitBody as DsRequest>::Response =
        match commit::submit_commit(&main_conn, &conn, &parsed).await? {
            commit::SubmitVerdict::Response(r) => r,
            // A bundle Welcome targeted a non-member or took another's pending Welcome; nothing written.
            commit::SubmitVerdict::Forbidden => {
                return Ok(AuthRejection::Forbidden.into_response())
            }
            commit::SubmitVerdict::Invalid(why) => return Ok(writes::bad_request(why)),
        };

    // Retention floor (#539, I4): an event-driven prune, best-effort.
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

/// POST /v1/commits/since — record the signing device's catch-up high-water and
/// run the EVENT-DRIVEN retention prune (#539, I4). This is a WRITE: it raises
/// the retention floor, so it is AUTHENTICATED as the device it claims to be,
/// exactly like `POST /v1/commits` — reuse [`auth::verify_request_identity`], the
/// four `X-Pollis-*` headers, and its refusal to authenticate a revoked device.
///
/// Bound by signature to the device (#681); a 401 here can never WIDEN pruning.
///
/// Takes raw [`Bytes`]: the signature binds `sha256(body)`.
async fn report_commit_since(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    // Authenticate first, over the RAW body; the identity comes from the signature.
    let authed = if state.require_auth {
        match auth::verify_request_identity_cached(
            &state.device_keys,
            &state.db,
            &req.headers,
            req.method.as_str(),
            req.uri.path(),
            &req.body,
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

    let parsed: CommitSinceReport = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => {
            return Ok((
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "invalid body" })),
            )
                .into_response())
        }
    };

    // Auth on: the verified signer, reporting only as itself. Auth off: the body identity.
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

    // Record the high-water, then apply the floor. Best-effort.
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
