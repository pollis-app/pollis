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
//! # Responses, the other direction (#922)
//!
//! Requests were only half the drift. Until #922 the *responses* were the same
//! two hand-maintained lists with the arrow reversed: the DS built them as
//! `serde_json::json!{}` literals and `pollis-core` declared six private
//! `#[derive(Deserialize)]` mirrors (`Resp`, `ClaimResp`, `PresignResp`,
//! `GcResp`, …) plus three call sites that indexed a bare `serde_json::Value` by
//! string. Same failure mode, same silence: a field the server renames is not
//! *wrong* on the client, it is simply *absent*, and `Option`/`unwrap_or_default`
//! turns it into a default the caller cannot distinguish from a real answer.
//!
//! So every endpoint now also names the body it answers with, as
//! [`DsRequest::Response`] — one declaration, both ends:
//!
//!   - `pollis-delivery` returns it through `writes::ok_response::<B>(…)`, which
//!     accepts `B::Response` and nothing else.
//!   - `pollis-core` decodes it through `ds_post_json::<B>(…) -> B::Response`.
//!
//! Neither end names the type twice, so a renamed field breaks *both* at compile
//! time rather than silently defaulting on one. The endpoints that answer 200
//! with no body at all say so in the type system too — [`Empty`], accepted only
//! by `ok_empty`, so "no body" cannot quietly become "a body nobody reads".
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
//!   - ERROR bodies (4xx/5xx) are not covered. `Response` is the SUCCESS body;
//!     failures are carried by the status code, which is what every caller
//!     branches on.
//!   - GET endpoints (`/health`, `/version`, `/v1/config`,
//!     `/v1/retention/metrics`, `GET /v1/commits/:id`) have no request body and
//!     so no entry here.
//!
//! # Adding an endpoint
//!
//! Add the request struct and its response type to the matching module, add one
//! line to the `endpoints!` table below, and route it in `pollis-delivery` as
//! `.route(<Body>::PATH, …)`. The route-coverage tests (in `pollis-delivery` and
//! in the `flows` harness) walk [`ENDPOINTS`] and fail if either router does not
//! serve it, so a new endpoint cannot be added to the client without also
//! reaching both routers.

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

/// A request body that knows the one path it is addressed at, and the one body
/// that path answers with.
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

    /// The SUCCESS body this endpoint answers with (#922).
    ///
    /// The same reasoning as the request body, in the other direction: the DS
    /// hands this exact type to `ok_response`, `pollis-core` decodes this exact
    /// type out of the response, and neither writes the field list down a second
    /// time. [`Empty`] means the endpoint answers 200 with no body.
    ///
    /// `Serialize` for the server half, `DeserializeOwned` for the client half —
    /// both bounds on one associated type is what makes the two halves the same
    /// declaration rather than two that happen to agree today.
    type Response: serde::Serialize + serde::de::DeserializeOwned;
}

/// Who is expected to call an endpoint (#925).
///
/// The route-coverage test added in #875 asserted every declared endpoint is
/// *routed*; nothing asserted anything ever *calls* it, which is how
/// `/v1/welcomes/resubmit` and `/v1/envelopes/gc` sat routed, typed and
/// caller-less. Classifying each endpoint here makes "nobody calls this" a
/// declared fact instead of an accident, and [`ClientRequest`] turns the
/// operator half into a compile error at any `pollis-core` call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    /// Called by `pollis-core` in the normal course of running the app. The
    /// endpoint-coverage test requires at least one call site.
    Client,
    /// Deliberately has no client caller: a recovery/maintenance lever an
    /// operator drives by hand. Documented in `docs/ds-operator-endpoints.md`,
    /// which the coverage test reads — an operator endpoint that is not written
    /// down there fails the build.
    Operator,
}

/// A request `pollis-core` is allowed to post.
///
/// Implemented by the `endpoints!` table only for [`Audience::Client`] rows, and
/// required by every `ds_post*` helper. That is what stops the client half of an
/// operator-only endpoint from being written by accident: `ds_post(&state,
/// &ResubmitBody { … })` does not compile, so "no client caller" is enforced by
/// the type system rather than re-checked by a reviewer.
pub trait ClientRequest: DsRequest {}

/// The body of an endpoint that answers 200 with NO body at all.
///
/// A distinct type rather than `()` (which would serialize as `null`) or
/// [`StatusOk`] (which would put bytes on the wire that are not there today):
/// `pollis-delivery`'s `ok_empty` accepts only `DsRequest<Response = Empty>`, so
/// "this endpoint has no response body" is checked rather than remembered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Empty;

/// `{"status":"ok"}` — what a write endpoint answers when it has nothing to
/// report beyond success. The overwhelming majority of the table.
///
/// An enum with an internal `status` tag rather than a struct with a
/// `status: String` field, so the only value that serializes (and the only one
/// that deserializes) is the literal the wire actually carries. A struct would
/// let `status: "borked"` typecheck.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum StatusOk {
    Ok,
}

/// One row of the endpoint table: the path, the Rust type that addresses it, and
/// who is expected to call it.
///
/// `type_name` exists so a coverage failure names the struct to look at, not
/// just a URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpoint {
    pub path: &'static str,
    pub type_name: &'static str,
    pub audience: Audience,
}

/// Declare the endpoint table once: the `DsRequest` impls, the [`ClientRequest`]
/// impls and [`ENDPOINTS`] are generated from the same lines, so they cannot
/// drift from each other.
macro_rules! endpoints {
    ($($aud:ident $ty:ty => $path:literal, $resp:ty);* $(;)?) => {
        $(
            impl $crate::DsRequest for $ty {
                const PATH: &'static str = $path;
                type Response = $resp;
            }

            endpoints!(@client_impl $aud $ty);
        )*

        /// Every POST endpoint the Delivery Service serves, in router order.
        ///
        /// The single source of truth for "which endpoints exist". Route-coverage
        /// tests walk it; nothing hand-maintains a second copy.
        pub const ENDPOINTS: &[Endpoint] = &[
            $(Endpoint {
                path: $path,
                type_name: stringify!($ty),
                audience: $crate::Audience::$aud,
            }),*
        ];
    };

    // `Client` rows — and only those — get the marker trait `ds_post` requires.
    (@client_impl Client $ty:ty) => { impl $crate::ClientRequest for $ty {} };
    (@client_impl Operator $ty:ty) => {};
}

endpoints! {
    // ── MLS control plane ────────────────────────────────────────────────────
    Client   commit::SubmitBody                 => "/v1/commits",                        commit::SubmitResponse;
    // Answers a bare 200: the client's catch-up report is fire-and-forget and
    // has never had a body to read. `Empty` says so in the type system.
    Client   commit::CommitSinceReport          => "/v1/commits/since",                  Empty;
    Client   writes::GroupInfoBody              => "/v1/group-info",                     StatusOk;
    Client   writes::AckBody                    => "/v1/welcomes/ack",                   writes::WelcomesUpdated;
    Client   writes::ResetBody                  => "/v1/welcomes/reset",                 writes::WelcomesUpdated;
    Client   writes::PurgeBody                  => "/v1/welcomes/purge",                 writes::WelcomesPurged;
    Operator writes::ResubmitBody               => "/v1/welcomes/resubmit",              StatusOk;

    // ── Domain A — messages ──────────────────────────────────────────────────
    Client   messages::SendMessageBody          => "/v1/messages/send",                  StatusOk;
    Client   messages::EditMessageBody          => "/v1/messages/edit",                  StatusOk;
    Client   messages::DeleteMessageBody        => "/v1/messages/delete",                StatusOk;
    Client   messages::AddReaction              => "/v1/reactions/add",                  StatusOk;
    Client   messages::RemoveReaction           => "/v1/reactions/remove",               StatusOk;
    Client   messages::WatermarkBody            => "/v1/watermarks/advance",             StatusOk;
    Operator messages::EnvelopeGcBody           => "/v1/envelopes/gc",                   StatusOk;
    Client   messages::AttachmentRegisterBody   => "/v1/attachments/register",           StatusOk;
    Client   messages::AttachmentDeleteBody     => "/v1/attachments/delete",             StatusOk;

    // ── Domain B — groups, channels, membership, invites, join requests ──────
    Client   groups::CreateGroupBody            => "/v1/groups/create",                  StatusOk;
    Client   groups::UpdateGroupBody            => "/v1/groups/update",                  StatusOk;
    Client   groups::DeleteGroupBody            => "/v1/groups/delete",                  StatusOk;
    Client   groups::LeaveGroupBody             => "/v1/groups/leave",                   StatusOk;
    Client   groups::CreateChannelBody          => "/v1/channels/create",                StatusOk;
    Client   groups::UpdateChannelBody          => "/v1/channels/update",                StatusOk;
    Client   groups::DeleteChannelBody          => "/v1/channels/delete",                StatusOk;
    Client   groups::RemoveMemberBody           => "/v1/members/remove",                 StatusOk;
    Client   groups::SetMemberRoleBody          => "/v1/members/role",                   StatusOk;
    Client   groups::CreateInviteBody           => "/v1/invites/create",                 StatusOk;
    Client   groups::AcceptInviteBody           => "/v1/invites/accept",                 StatusOk;
    Client   groups::DeclineInviteBody          => "/v1/invites/decline",                StatusOk;
    Client   groups::CreateInviteLinkBody       => "/v1/invite-links/create",            StatusOk;
    Client   groups::RevokeInviteLinkBody       => "/v1/invite-links/revoke",            StatusOk;
    Client   groups::RedeemInviteLinkBody       => "/v1/invite-links/redeem",            groups::RedeemInviteLinkResponse;
    Client   groups::CreateJoinRequestBody      => "/v1/join-requests/create",           StatusOk;
    Client   groups::ApproveJoinRequestBody     => "/v1/join-requests/approve",          StatusOk;
    Client   groups::RejectJoinRequestBody      => "/v1/join-requests/reject",           StatusOk;

    // ── Custom emoji (#848) ──────────────────────────────────────────────────
    Client   emoji::CreateEmojiBody             => "/v1/emoji/create",                   StatusOk;
    Client   emoji::RemoveEmojiBody             => "/v1/emoji/remove",                   StatusOk;
    Client   emoji::EmojiGcBody                 => "/v1/emoji/gc",                       emoji::EmojiGcResponse;

    // ── Domain C — profile, blocks, DMs ──────────────────────────────────────
    Client   profile::UpdateProfileBody         => "/v1/profile/update",                 StatusOk;
    Client   profile::SavePreferencesBody       => "/v1/profile/preferences",            StatusOk;
    Client   profile::AddBlock                  => "/v1/blocks/add",                     StatusOk;
    Client   profile::RemoveBlock               => "/v1/blocks/remove",                  StatusOk;
    Client   profile::CreateDmBody              => "/v1/dm/create",                      StatusOk;
    Client   profile::AcceptDmBody              => "/v1/dm/accept",                      StatusOk;
    Client   profile::AddDmMemberBody           => "/v1/dm/add",                         StatusOk;
    Client   profile::RemoveDmMemberBody        => "/v1/dm/remove",                      StatusOk;
    Client   profile::LeaveDmBody               => "/v1/dm/leave",                       StatusOk;

    // ── Domain D — devices and key packages ──────────────────────────────────
    Client   devices::PublishKeyPackagesBody    => "/v1/key-packages",                   StatusOk;
    Client   devices::ClaimKeyPackageBody       => "/v1/key-packages/claim",             devices::ClaimKeyPackageResponse;
    Client   devices::ReplenishKeyPackagesBody  => "/v1/key-packages/replenish",         StatusOk;
    Client   devices::ResignDeviceCertsBody     => "/v1/devices/resign",                 StatusOk;
    Client   devices::PushTokenBody             => "/v1/push-tokens",                    StatusOk;

    // ── Domains E + G — account lifecycle ────────────────────────────────────
    Client   account::RotateIdentityBody        => "/v1/account/rotate-identity",        account::RotateIdentityResponse;
    Client   account::DeleteAccountBody         => "/v1/account/delete",                 StatusOk;
    Client   account::ResetRecoverBody          => "/v1/account/reset-recover",          StatusOk;
    Client   account::SecurityEventBody         => "/v1/security-events",                StatusOk;
    Client   account::ApproveEnrollmentBody     => "/v1/enrollment/approve",             StatusOk;
    Client   account::RejectEnrollmentBody      => "/v1/enrollment/reject",              StatusOk;
    Client   account::RevokeDeviceBody          => "/v1/devices/revoke",                 StatusOk;
    Client   account::LogoutDeviceBody          => "/v1/auth/logout",                    StatusOk;

    // ── OTP + bootstrap ──────────────────────────────────────────────────────
    Client   otp::RequestOtpBody                => "/v1/auth/request-otp",               StatusOk;
    Client   otp::VerifyOtpBody                 => "/v1/auth/verify-otp",                otp::VerifyOtpResponse;
    Client   bootstrap::EstablishIdentityBody   => "/v1/auth/establish-identity",        StatusOk;
    Client   bootstrap::RegisterDeviceBody      => "/v1/auth/register-device",           StatusOk;
    Client   bootstrap::PublishCertBody         => "/v1/auth/publish-device-cert",       StatusOk;
    Client   bootstrap::EnrollmentRequestBody   => "/v1/auth/enrollment-request",        StatusOk;
    Client   email_change::RequestEmailChangeBody => "/v1/auth/request-email-change-otp", StatusOk;
    Client   email_change::VerifyEmailChangeBody  => "/v1/auth/verify-email-change",      StatusOk;

    // ── Authorized-secrets broker (#393) ─────────────────────────────────────
    Client   broker::LivekitTokenBody           => "/v1/livekit/token",                  broker::LivekitTokenResponse;
    Client   broker::LivekitSendDataBody        => "/v1/livekit/send-data",              broker::LivekitSendDataResponse;
    Client   broker::LivekitParticipantsBody    => "/v1/livekit/participants",           broker::LivekitParticipantsResponse;
    Client   broker::LivekitIdentitiesBody      => "/v1/livekit/identities",             broker::LivekitIdentitiesResponse;
    Client   broker::TursoTokenBody             => "/v1/turso/token",                    broker::TursoTokenResponse;
    Client   broker::R2PresignBody              => "/v1/r2/presign",                     broker::R2PresignResponse;
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
///
/// # Responses (#922)
///
/// The same four failures, in the other direction. `answer` below stands in for
/// the two real ends — `pollis_delivery::writes::ok_response::<B>` and
/// `pollis_core`'s `ds_post_json::<B>` — both of which are exactly this
/// signature, so what fails here fails there:
///
/// ```
/// # use pollis_api::DsRequest;
/// /// Produce the ONE body this endpoint is declared to answer with.
/// fn answer<B: DsRequest>(_: B::Response) {}
/// ```
///
/// A field the server forgets is [E0063], exactly as on the request side — and
/// this is the half that used to be a `json!{}` literal, where forgetting one
/// compiled fine and reached the client as `None`:
///
/// ```compile_fail,E0063
/// use pollis_api::broker::LivekitTokenResponse;
/// let _ = LivekitTokenResponse {
///     token: "jwt".into(),
///     // `url` omitted. As a `json!{}` literal this shipped; the client read a
///     // default and silently dialled the compiled-in SFU instead.
/// };
/// ```
///
/// A field the client invents (or the server renames out from under it) is
/// [E0609] "no field on type" — the read that used to yield `Option::None`:
///
/// ```compile_fail,E0609
/// use pollis_api::broker::TursoTokenResponse;
/// fn read(r: TursoTokenResponse) -> u64 {
///     // The wire field is `expires_in`. A client that spells it otherwise now
///     // fails to compile instead of quietly defaulting to 0.
///     r.expires_in_secs
/// }
/// ```
///
/// And an endpoint cannot answer with another endpoint's body — [E0308] — which
/// is what binds the two ends to ONE declaration rather than two that agree
/// today:
///
/// ```compile_fail,E0308
/// # use pollis_api::DsRequest;
/// # fn answer<B: DsRequest>(_: B::Response) {}
/// use pollis_api::{broker::TursoTokenBody, StatusOk};
/// // `/v1/turso/token` answers a token, not `{"status":"ok"}`.
/// answer::<TursoTokenBody>(StatusOk::Ok);
/// ```
///
/// The matching positive case, so the failures above mean something:
///
/// ```
/// # use pollis_api::DsRequest;
/// # fn answer<B: DsRequest>(_: B::Response) {}
/// use pollis_api::broker::{TursoTokenBody, TursoTokenResponse};
/// answer::<TursoTokenBody>(TursoTokenResponse {
///     token: "jwt".into(),
///     expires_in: 600,
/// });
/// ```
///
/// An operator-only endpoint has no client half at all: `ds_post` requires
/// [`ClientRequest`], which the table implements only for `Audience::Client`
/// rows, so posting `/v1/welcomes/resubmit` from `pollis-core` is [E0277]
/// rather than a review comment:
///
/// ```compile_fail,E0277
/// use pollis_api::ClientRequest;
/// fn post<B: ClientRequest>(_: &B) {}
/// post(&pollis_api::writes::ResubmitBody {
///     conversation_id: "c".into(),
///     generation: 0,
///     recipient_id: "u".into(),
///     recipient_device_id: "d".into(),
///     welcome: String::new(),
/// });
/// ```
#[cfg(doctest)]
pub struct CompileTimeContractDocs;

#[cfg(test)]
mod tests {
    use super::*;

    /// Every wire response must survive a round trip through JSON, and produce
    /// the bytes the deployed DS produces today (#922).
    ///
    /// Two claims in one test, because they fail together. The associated type
    /// is only worth anything if ONE declaration really is both what the DS
    /// writes and what the client reads — `#[serde(tag = "status")]` enums in
    /// particular are easy to declare in a shape that encodes fine and refuses
    /// to decode. And because these types REPLACE hand-written `json!{}`
    /// literals on a service that is already deployed, the exact bytes are part
    /// of the contract: a live client decodes them.
    #[test]
    fn every_response_shape_round_trips_and_keeps_its_wire_bytes() {
        fn round_trip<T>(value: &T, expected_json: &str)
        where
            T: serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + PartialEq,
        {
            let encoded = serde_json::to_string(value).expect("serialize");
            assert_eq!(
                encoded, expected_json,
                "wire bytes changed — a deployed client decodes these"
            );
            let decoded: T = serde_json::from_str(&encoded).expect("deserialize");
            assert_eq!(&decoded, value, "response did not survive its own encoding");
        }

        round_trip(&StatusOk::Ok, r#"{"status":"ok"}"#);
        round_trip(
            &writes::WelcomesUpdated::Ok { updated: 3 },
            r#"{"status":"ok","updated":3}"#,
        );
        round_trip(
            &writes::WelcomesPurged::Ok { deleted: 2 },
            r#"{"status":"ok","deleted":2}"#,
        );
        round_trip(
            &account::RotateIdentityResponse::Ok {
                identity_version: 7,
            },
            r#"{"status":"ok","identity_version":7}"#,
        );
        round_trip(
            &groups::RedeemInviteLinkResponse::Ok {
                group_id: "g1".into(),
            },
            r#"{"status":"ok","group_id":"g1"}"#,
        );
        round_trip(
            &broker::TursoTokenResponse {
                token: "t".into(),
                expires_in: 600,
            },
            r#"{"token":"t","expires_in":600}"#,
        );
        round_trip(
            &broker::LivekitTokenResponse {
                token: "jwt".into(),
                url: "wss://sfu".into(),
            },
            r#"{"token":"jwt","url":"wss://sfu"}"#,
        );
        round_trip(
            &broker::LivekitSendDataResponse { ok: true },
            r#"{"ok":true}"#,
        );
        round_trip(
            &broker::R2PresignResponse {
                url: "https://r2/o".into(),
                method: "PUT".into(),
                expires_in: 900,
            },
            r#"{"url":"https://r2/o","method":"PUT","expires_in":900}"#,
        );
        round_trip(
            &devices::ClaimKeyPackageResponse {
                ref_hash: "h".into(),
                key_package: "kp".into(),
            },
            r#"{"ref_hash":"h","key_package":"kp"}"#,
        );
        round_trip(
            &emoji::EmojiGcResponse {
                collected: 1,
                r2_keys: vec!["emoji/a".into()],
            },
            r#"{"collected":1,"r2_keys":["emoji/a"]}"#,
        );
        round_trip(
            &broker::LivekitParticipantsResponse {
                participants: vec![broker::LivekitParticipant {
                    identity: "d-x".into(),
                    name: "alice".into(),
                }],
            },
            r#"{"participants":[{"identity":"d-x","name":"alice"}]}"#,
        );
        round_trip(
            &otp::VerifyOtpResponse {
                user_id: "u".into(),
                username: "alice".into(),
                is_new_account: false,
                has_identity: true,
                session_token: "s".into(),
                session_expires_at: 42,
            },
            r#"{"user_id":"u","username":"alice","is_new_account":false,"has_identity":true,"session_token":"s","session_expires_at":42}"#,
        );
    }

    /// `{"status":"ok"}` is a fixed literal, not a free-form string.
    ///
    /// The reason [`StatusOk`] is an enum rather than a struct with a
    /// `status: String`: a struct would let a handler answer
    /// `{"status":"borked"}` and typecheck, and every caller branches on the
    /// HTTP status anyway, so nothing downstream would ever notice.
    #[test]
    fn a_status_other_than_ok_does_not_decode() {
        assert!(serde_json::from_str::<StatusOk>(r#"{"status":"ok"}"#).is_ok());
        assert!(serde_json::from_str::<StatusOk>(r#"{"status":"borked"}"#).is_err());
        // And a counted response is not satisfied by a bare ok — the count is
        // part of the shape, not an optional extra.
        assert!(serde_json::from_str::<writes::WelcomesUpdated>(r#"{"status":"ok"}"#).is_err());
    }

    /// Every `Operator` endpoint is written down in the runbook (#925).
    ///
    /// The route-coverage tests added in #875 assert every endpoint is ROUTED.
    /// Nothing asserted anything CALLS it, which is how `/v1/welcomes/resubmit`
    /// and `/v1/envelopes/gc` came to sit fully typed and fully routed with no
    /// caller anywhere in the workspace and no note saying that was deliberate.
    ///
    /// The other direction is enforced structurally instead of here:
    /// [`ClientRequest`] is not implemented for these rows, so `ds_post` will
    /// not accept one and a client caller cannot be added by accident. What a
    /// type cannot check is whether the human on the other end was told the
    /// endpoint exists — so that is what this checks.
    #[test]
    fn every_operator_endpoint_is_documented_in_the_runbook() {
        let runbook = include_str!("../../docs/ds-operator-endpoints.md");
        let operators: Vec<&Endpoint> = ENDPOINTS
            .iter()
            .filter(|e| e.audience == Audience::Operator)
            .collect();
        assert!(
            !operators.is_empty(),
            "Audience::Operator exists for the two endpoints #925 found. If the last one gained \
             a client caller, delete the variant rather than leave it as unused machinery."
        );
        for e in operators {
            assert!(
                runbook.contains(e.path),
                "{} ({}) is Audience::Operator but docs/ds-operator-endpoints.md never mentions \
                 it. An endpoint nobody calls and nobody documented is precisely the dead \
                 surface #925 is about.",
                e.type_name,
                e.path
            );
        }
    }

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
