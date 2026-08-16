//! The Delivery Service's write API, expressed once as Rust types.
//!
//! # Why this crate exists
//!
//! Every remote write in Pollis is a `POST /v1/...` to the Delivery Service.
//! Before this crate the two ends of each of those calls were written down
//! twice: the DS declared a typed `…Body` struct, and `pollis-core` built the
//! request as an untyped `serde_json::json!{}` literal. Nothing connected the
//! two, and JSON's failure mode is silence — a field the client forgets is not
//! *wrong*, it is simply *absent*, and `#[serde(default)]` turns it into `None`.
//!
//! That drift shipped, twice:
//!
//!   - Four endpoints (`/v1/messages/delete` on both its call sites,
//!     `/v1/emoji/create`, `/v1/emoji/remove`) omitted the actor field entirely
//!     and hard-403'd wherever the DS ran with auth disabled.
//!   - The `flows` harness re-declared the router by hand and was missing ~10
//!     routes (#918), so `/v1/invite-links/*` 404'd under test and custom emoji
//!     (#848) had no integration coverage at all.
//!
//! Both are the same bug: two hand-maintained lists, no mechanism forcing them
//! to agree.
//!
//! # What replaces it
//!
//! One struct per endpoint, in this crate, used by BOTH ends:
//!
//!   - `pollis-delivery` parses requests into these types (each of its modules
//!     re-exports the matching module here, so `pollis_delivery::messages::
//!     SendMessageBody` still resolves) and builds its router from
//!     [`DsRequest::PATH`] rather than a string literal.
//!   - `pollis-core` constructs them as struct literals and posts them with
//!     `ds_post(&state, &body)` — no path argument, because the path is a
//!     property of the type.
//!
//! Rust struct literals must name every field. So a client that omits, renames,
//! or invents a field does not compile, and a client cannot post a body to an
//! endpoint that does not expect it. See [`ENDPOINTS`] for the full table and
//! the `compile_fail` doctests below for the guarantee, executable.
//!
//! # What this does NOT catch
//!
//! The contract is *shape*, not *semantics* or *deployment*:
//!
//!   - A field of the right name and type carrying the WRONG VALUE still
//!     compiles. `actor_id: None` where `Some(me)` was meant is a type-correct
//!     mistake; only a test catches it.
//!   - The DS runs as a separately deployed binary. An OLD server still on the
//!     previous field set is a version-skew problem, unchanged by this crate —
//!     wire compatibility rules (additive, optional, defaulted) still apply.
//!   - RESPONSE bodies are not covered. Callers still `#[derive(Deserialize)]`
//!     their own local response structs.
//!   - GET endpoints (`/health`, `/version`, `/v1/config`,
//!     `/v1/retention/metrics`, `GET /v1/commits/:id`) have no request body and
//!     so no entry here.
//!
//! # Adding an endpoint
//!
//! Add the struct to the matching module, add one line to the `endpoints!`
//! table below, and route it in `pollis-delivery` as `.route(<Body>::PATH, …)`.
//! The route-coverage tests (in `pollis-delivery` and in the `flows` harness)
//! walk [`ENDPOINTS`] and fail if either router does not serve it, so a new
//! endpoint cannot be added to the client without also reaching both routers.

pub mod account;
pub mod bootstrap;
pub mod broker;
pub mod commit;
pub mod devices;
pub mod email_change;
pub mod emoji;
pub mod groups;
pub mod messages;
pub mod otp;
pub mod profile;
pub mod writes;

/// A request body that knows the one path it is addressed at.
///
/// Implemented exactly once per endpoint by the `endpoints!` table, which is
/// also what populates [`ENDPOINTS`]. Binding the path to the TYPE (rather than
/// passing it alongside the body) is what removes the second hand-maintained
/// list: `ds_post` takes no path argument, so a client physically cannot send a
/// body to the wrong endpoint, and the DS routes on `Body::PATH` so the router
/// and the client cannot disagree about the spelling.
pub trait DsRequest: serde::Serialize {
    /// Request path, leading slash, no query — byte-for-byte what the DS routes
    /// on and what the client's request signature binds.
    const PATH: &'static str;
}

/// One row of the endpoint table: the path and the Rust type that addresses it.
///
/// `type_name` exists so a coverage failure names the struct to look at, not
/// just a URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpoint {
    pub path: &'static str,
    pub type_name: &'static str,
}

/// Declare the endpoint table once: the `DsRequest` impls and [`ENDPOINTS`] are
/// generated from the same lines, so they cannot drift from each other.
macro_rules! endpoints {
    ($($ty:ty => $path:literal),* $(,)?) => {
        $(
            impl $crate::DsRequest for $ty {
                const PATH: &'static str = $path;
            }
        )*

        /// Every POST endpoint the Delivery Service serves, in router order.
        ///
        /// The single source of truth for "which endpoints exist". Route-coverage
        /// tests walk it; nothing hand-maintains a second copy.
        pub const ENDPOINTS: &[Endpoint] = &[
            $(Endpoint { path: $path, type_name: stringify!($ty) }),*
        ];
    };
}

endpoints! {
    // ── MLS control plane ────────────────────────────────────────────────────
    commit::SubmitBody => "/v1/commits",
    commit::CommitSinceReport => "/v1/commits/since",
    writes::GroupInfoBody => "/v1/group-info",
    writes::AckBody => "/v1/welcomes/ack",
    writes::ResetBody => "/v1/welcomes/reset",
    writes::PurgeBody => "/v1/welcomes/purge",
    writes::ResubmitBody => "/v1/welcomes/resubmit",

    // ── Domain A — messages ──────────────────────────────────────────────────
    messages::SendMessageBody => "/v1/messages/send",
    messages::EditMessageBody => "/v1/messages/edit",
    messages::DeleteMessageBody => "/v1/messages/delete",
    messages::AddReaction => "/v1/reactions/add",
    messages::RemoveReaction => "/v1/reactions/remove",
    messages::WatermarkBody => "/v1/watermarks/advance",
    messages::EnvelopeGcBody => "/v1/envelopes/gc",
    messages::AttachmentRegisterBody => "/v1/attachments/register",
    messages::AttachmentDeleteBody => "/v1/attachments/delete",

    // ── Domain B — groups, channels, membership, invites, join requests ──────
    groups::CreateGroupBody => "/v1/groups/create",
    groups::UpdateGroupBody => "/v1/groups/update",
    groups::DeleteGroupBody => "/v1/groups/delete",
    groups::LeaveGroupBody => "/v1/groups/leave",
    groups::CreateChannelBody => "/v1/channels/create",
    groups::UpdateChannelBody => "/v1/channels/update",
    groups::DeleteChannelBody => "/v1/channels/delete",
    groups::RemoveMemberBody => "/v1/members/remove",
    groups::SetMemberRoleBody => "/v1/members/role",
    groups::CreateInviteBody => "/v1/invites/create",
    groups::AcceptInviteBody => "/v1/invites/accept",
    groups::DeclineInviteBody => "/v1/invites/decline",
    groups::CreateInviteLinkBody => "/v1/invite-links/create",
    groups::RevokeInviteLinkBody => "/v1/invite-links/revoke",
    groups::RedeemInviteLinkBody => "/v1/invite-links/redeem",
    groups::CreateJoinRequestBody => "/v1/join-requests/create",
    groups::ApproveJoinRequestBody => "/v1/join-requests/approve",
    groups::RejectJoinRequestBody => "/v1/join-requests/reject",

    // ── Custom emoji (#848) ──────────────────────────────────────────────────
    emoji::CreateEmojiBody => "/v1/emoji/create",
    emoji::RemoveEmojiBody => "/v1/emoji/remove",
    emoji::EmojiGcBody => "/v1/emoji/gc",

    // ── Domain C — profile, blocks, DMs ──────────────────────────────────────
    profile::UpdateProfileBody => "/v1/profile/update",
    profile::SavePreferencesBody => "/v1/profile/preferences",
    profile::AddBlock => "/v1/blocks/add",
    profile::RemoveBlock => "/v1/blocks/remove",
    profile::CreateDmBody => "/v1/dm/create",
    profile::AcceptDmBody => "/v1/dm/accept",
    profile::AddDmMemberBody => "/v1/dm/add",
    profile::RemoveDmMemberBody => "/v1/dm/remove",
    profile::LeaveDmBody => "/v1/dm/leave",

    // ── Domain D — devices and key packages ──────────────────────────────────
    devices::PublishKeyPackagesBody => "/v1/key-packages",
    devices::ClaimKeyPackageBody => "/v1/key-packages/claim",
    devices::ReplenishKeyPackagesBody => "/v1/key-packages/replenish",
    devices::ResignDeviceCertsBody => "/v1/devices/resign",
    devices::PushTokenBody => "/v1/push-tokens",

    // ── Domains E + G — account lifecycle ────────────────────────────────────
    account::RotateIdentityBody => "/v1/account/rotate-identity",
    account::DeleteAccountBody => "/v1/account/delete",
    account::ResetRecoverBody => "/v1/account/reset-recover",
    account::SecurityEventBody => "/v1/security-events",
    account::ApproveEnrollmentBody => "/v1/enrollment/approve",
    account::RejectEnrollmentBody => "/v1/enrollment/reject",
    account::RevokeDeviceBody => "/v1/devices/revoke",
    account::LogoutDeviceBody => "/v1/auth/logout",

    // ── OTP + bootstrap ──────────────────────────────────────────────────────
    otp::RequestOtpBody => "/v1/auth/request-otp",
    otp::VerifyOtpBody => "/v1/auth/verify-otp",
    bootstrap::EstablishIdentityBody => "/v1/auth/establish-identity",
    bootstrap::RegisterDeviceBody => "/v1/auth/register-device",
    bootstrap::PublishCertBody => "/v1/auth/publish-device-cert",
    bootstrap::EnrollmentRequestBody => "/v1/auth/enrollment-request",
    email_change::RequestEmailChangeBody => "/v1/auth/request-email-change-otp",
    email_change::VerifyEmailChangeBody => "/v1/auth/verify-email-change",

    // ── Authorized-secrets broker (#393) ─────────────────────────────────────
    broker::LivekitTokenBody => "/v1/livekit/token",
    broker::LivekitSendDataBody => "/v1/livekit/send-data",
    broker::LivekitParticipantsBody => "/v1/livekit/participants",
    broker::TursoTokenBody => "/v1/turso/token",
    broker::R2PresignBody => "/v1/r2/presign",
}

/// The guarantee, executable: a body missing a field does not compile.
///
/// This is the exact shape of the bug that shipped — `/v1/messages/delete` sent
/// no actor, so the DS could not name the acting user and 403'd whenever auth
/// was disabled. As a `json!{}` literal it compiled fine and failed in
/// production; as a struct literal it is [E0063] "missing field", checked by
/// `cargo test -p pollis-api`.
///
/// ```compile_fail,E0063
/// use pollis_api::messages::DeleteMessageBody;
/// let _ = DeleteMessageBody {
///     message_id: "m".into(),
///     conversation_id: "c".into(),
///     msg_sender_id: None,
///     // actor_id omitted — exactly the bug. Does not compile.
/// };
/// ```
///
/// Naming every field compiles, which is what makes the failure above
/// meaningful rather than a struct that never builds:
///
/// ```
/// use pollis_api::messages::DeleteMessageBody;
/// let _ = DeleteMessageBody {
///     message_id: "m".into(),
///     conversation_id: "c".into(),
///     msg_sender_id: None,
///     actor_id: Some("u".into()),
/// };
/// ```
///
/// A field the server does not have is [E0560] "no field on type" — the
/// misspelling case, which JSON accepts silently and the server then ignores:
///
/// ```compile_fail,E0560
/// use pollis_api::messages::SendMessageBody;
/// let _ = SendMessageBody {
///     id: "m".into(),
///     conversation_id: "c".into(),
///     sender_id: None,
///     ciphertext: "mls:00".into(),
///     reply_to_id: None,
///     sent_at: "2026-01-01T00:00:00Z".into(),
///     sealed: 1,
///     // Not a field of SendMessageBody.
///     replyto_id: None,
/// };
/// ```
///
/// And a body cannot be sent to an endpoint that does not expect it, because the
/// path is a property of the type rather than a separate argument:
///
/// ```
/// use pollis_api::DsRequest;
/// assert_eq!(pollis_api::messages::SendMessageBody::PATH, "/v1/messages/send");
/// assert_eq!(pollis_api::emoji::CreateEmojiBody::PATH, "/v1/emoji/create");
/// ```
#[cfg(doctest)]
pub struct CompileTimeContractDocs;

#[cfg(test)]
mod tests {
    use super::*;

    /// Two endpoints answering on one path is a routing ambiguity: axum would
    /// panic at router build time in the DS, but only for the pair that happens
    /// to be registered — this catches it at the declaration.
    #[test]
    fn every_endpoint_path_is_unique() {
        let mut seen: Vec<&str> = ENDPOINTS.iter().map(|e| e.path).collect();
        seen.sort_unstable();
        let before = seen.len();
        seen.dedup();
        assert_eq!(
            before,
            seen.len(),
            "duplicate path in the ENDPOINTS table — two request types claim one route"
        );
    }

    /// Guard, not a proof: paths are what the client signs and the DS routes on,
    /// so a stray space or a missing leading slash is a silent 404. Cheap to
    /// assert at the table level.
    #[test]
    fn every_path_is_a_well_formed_v1_route() {
        for e in ENDPOINTS {
            assert!(
                e.path.starts_with("/v1/"),
                "{}: path {:?} must start with /v1/",
                e.type_name,
                e.path
            );
            assert!(
                !e.path.contains(' ') && !e.path.ends_with('/') && !e.path.contains('?'),
                "{}: path {:?} is not a bare route",
                e.type_name,
                e.path
            );
        }
    }
}
