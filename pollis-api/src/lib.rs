//! The Delivery Service's write API, expressed once as Rust types.
//!
//! # Why this crate exists
//!
//! Both ends of every `POST /v1/...` used to be written twice, and JSON drift is silent.
//!
//! # What replaces it
//!
//! One struct per endpoint, in this crate, used by BOTH ends:
//!
//!   - `pollis-delivery` parses and routes on [`DsRequest::PATH`]; `pollis-core` posts struct literals via `ds_post`.
//!
//! Struct literals name every field, so drift does not compile; see [`ENDPOINTS`] and the `compile_fail` doctests below.
//!
//! # Responses, the other direction (#922)
//!
//! Responses drifted the same way, so each endpoint also names its [`DsRequest::Response`]:
//!
//!   - `pollis-delivery` returns it through `writes::ok_response::<B>(…)`, which
//!     accepts `B::Response` and nothing else.
//!   - `pollis-core` decodes it through `ds_post_json::<B>(…) -> B::Response`.
//!
//! [`Empty`] (accepted only by `ok_empty`) marks endpoints with no response body.
//!
//! # What this does NOT catch
//!
//! The contract is *shape*, not *semantics* or *deployment*:
//!
//!   - A right-typed WRONG VALUE still compiles; only a test catches it.
//!   - Version skew with an older deployed DS: wire rules (additive, optional, defaulted) still apply.
//!   - ERROR bodies (4xx/5xx); `Response` is the SUCCESS body.
//!   - GETs (`/health`, `/version`, `/v1/config`, `/v1/retention/metrics`, `/v1/client-policy`), the only GETs the DS serves.
//!
//! # Adding an endpoint
//!
//! Add the types and one `endpoints!` row, and route `<Body>::PATH` in `pollis-delivery`; coverage tests check both routers.

pub mod account;
pub mod account_reads;
pub mod bootstrap;
pub mod broker;
pub mod client_policy;
pub mod commit;
pub mod devices;
pub mod directory;
pub mod email_change;
pub mod emoji;
pub mod file_keys;
pub mod groups;
pub mod links;
pub mod messages;
pub mod otp;
pub mod pins;
pub mod profile;
pub mod reads;
pub mod redact;
pub mod reports;
pub mod vault;
pub mod writes;

/// A request body that knows the one path it is addressed at, and the one body
/// that path answers with.
///
/// Implemented once per endpoint by `endpoints!`; `ds_post` takes no path, so a body cannot reach the wrong endpoint.
pub trait DsRequest: serde::Serialize {
    /// Request path, leading slash, no query — byte-for-byte what the DS routes
    /// on and what the client's request signature binds.
    const PATH: &'static str;

    /// The SUCCESS body this endpoint answers with (#922).
    ///
    /// One declaration both ends use; [`Empty`] means 200 with no body.
    type Response: serde::Serialize + serde::de::DeserializeOwned;

    /// Whether resending is safe (RFC 9110 §9.2.2), allowing a retry after an ambiguous connection failure (#1497).
    const IDEMPOTENT: bool = false;
}

/// Who is expected to call an endpoint (#925).
///
/// Makes "nobody calls this" a declared fact; [`ClientRequest`] keeps operator endpoints out of `pollis-core`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    /// Called by `pollis-core` in the normal course of running the app. The
    /// endpoint-coverage test requires at least one call site.
    Client,
    /// Deliberately client-less and operator-driven; must be documented in `docs/ds-operator-endpoints.md`.
    Operator,
}

/// A request `pollis-core` is allowed to post.
///
/// Implemented only for [`Audience::Client`] rows and required by every `ds_post*` helper.
pub trait ClientRequest: DsRequest {}

/// The body of an endpoint that answers 200 with NO body at all.
///
/// Not `()` (serializes as `null`) nor [`StatusOk`]; `ok_empty` accepts only `Response = Empty`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Empty;

/// `{"status":"ok"}` — what a write endpoint answers when it has nothing to
/// report beyond success. The overwhelming majority of the table.
///
/// An internally tagged enum, so only the literal the wire carries (de)serializes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum StatusOk {
    Ok,
}

/// One row of the endpoint table: the path, the Rust type that addresses it, and
/// who is expected to call it.
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
    ($($aud:ident $ty:ty => $path:literal, $resp:ty $(, $idem:ident)?);* $(;)?) => {
        $(
            impl $crate::DsRequest for $ty {
                const PATH: &'static str = $path;
                type Response = $resp;
                $(const IDEMPOTENT: bool = endpoints!(@idempotent $idem);)?
            }

            endpoints!(@client_impl $aud $ty);
        )*

        /// Every POST endpoint the Delivery Service serves, in router order.
        pub const ENDPOINTS: &[Endpoint] = &[
            $(Endpoint {
                path: $path,
                type_name: stringify!($ty),
                audience: $crate::Audience::$aud,
            }),*
        ];
    };

    // The only flag a row may carry; any other word is a compile error.
    (@idempotent idempotent) => { true };

    // `Client` rows — and only those — get the marker trait `ds_post` requires.
    (@client_impl Client $ty:ty) => { impl $crate::ClientRequest for $ty {} };
    (@client_impl Operator $ty:ty) => {};
}

endpoints! {
    Client   commit::SubmitBody                 => "/v1/commits",                        commit::SubmitResponse;
    // Answers a bare 200: the catch-up report is fire-and-forget.
    Client   commit::CommitSinceReport          => "/v1/commits/since",                  Empty;
    Client   writes::GroupInfoBody              => "/v1/group-info",                     StatusOk;
    Client   writes::AckBody                    => "/v1/welcomes/ack",                   writes::WelcomesUpdated;
    Client   writes::ResetBody                  => "/v1/welcomes/reset",                 writes::WelcomesUpdated;
    Client   writes::PurgeBody                  => "/v1/welcomes/purge",                 writes::WelcomesPurged;
    Client   reads::FetchWelcomesBody           => "/v1/welcomes/fetch",                 reads::FetchWelcomesResponse;
    Client   reads::ConversationStateBody       => "/v1/mls/conversation-state",         reads::ConversationStateResponse;
    Operator writes::ResubmitBody               => "/v1/welcomes/resubmit",              StatusOk;

    Client   messages::SendMessageBody          => "/v1/messages/send",                  StatusOk;
    Client   messages::EditMessageBody          => "/v1/messages/edit",                  StatusOk;
    Client   messages::DeleteMessageBody        => "/v1/messages/delete",                StatusOk;
    Client   messages::AddReaction              => "/v1/reactions/add",                  StatusOk;
    Client   messages::RemoveReaction           => "/v1/reactions/remove",               StatusOk;
    Client   messages::WatermarkBody            => "/v1/watermarks/advance",             StatusOk;
    Operator messages::EnvelopeGcBody           => "/v1/envelopes/gc",                   StatusOk;
    Client   messages::AttachmentRegisterBody   => "/v1/attachments/register",           StatusOk, idempotent;
    Client   messages::AttachmentDeleteBody     => "/v1/attachments/delete",             StatusOk;

    Client   groups::CreateGroupBody            => "/v1/groups/create",                  StatusOk;
    Client   groups::UpdateGroupBody            => "/v1/groups/update",                  groups::UpdatedGroup;
    Client   groups::DeleteGroupBody            => "/v1/groups/delete",                  StatusOk;
    Client   groups::LeaveGroupBody             => "/v1/groups/leave",                   StatusOk;
    Client   groups::CreateChannelBody          => "/v1/channels/create",                StatusOk;
    Client   groups::UpdateChannelBody          => "/v1/channels/update",                groups::UpdatedChannel;
    Client   groups::DeleteChannelBody          => "/v1/channels/delete",                StatusOk;
    Client   groups::RemoveMemberBody           => "/v1/members/remove",                 StatusOk;
    Client   groups::SetMemberRoleBody          => "/v1/members/role",                   StatusOk;
    Client   groups::CreateInviteBody           => "/v1/invites/create",                 groups::InviteCreated;
    Client   groups::AcceptInviteBody           => "/v1/invites/accept",                 groups::AcceptedInvite;
    Client   groups::DeclineInviteBody          => "/v1/invites/decline",                StatusOk;
    Client   groups::RevokeInviteBody           => "/v1/invites/revoke",                 StatusOk;
    Client   groups::CreateInviteLinkBody       => "/v1/invite-links/create",            StatusOk;
    Client   groups::RevokeInviteLinkBody       => "/v1/invite-links/revoke",            StatusOk;
    Client   groups::RedeemInviteLinkBody       => "/v1/invite-links/redeem",            groups::RedeemInviteLinkResponse;
    Client   groups::CreateJoinRequestBody      => "/v1/join-requests/create",           groups::JoinRequestCreated;
    Client   groups::ApproveJoinRequestBody     => "/v1/join-requests/approve",          groups::ReviewedJoinRequest;
    Client   groups::RejectJoinRequestBody      => "/v1/join-requests/reject",           groups::ReviewedJoinRequest;

    Client   pins::UpsertPinKeystateBody        => "/v1/pins/keystate",                  pins::PinKeystateUpserted;
    Client   pins::PinMessageBody               => "/v1/pins/pin",                       StatusOk;
    Client   pins::UnpinMessageBody             => "/v1/pins/unpin",                     StatusOk;
    Client   pins::PinKeystateBody              => "/v1/read/pin-keystate",              pins::PinKeystateResponse;
    Client   pins::ListPinsBody                 => "/v1/read/pins",                      pins::ListPinsResponse;

    Client   file_keys::UpsertFileKeystateBody  => "/v1/file-keys/keystate",             file_keys::FileKeystateUpserted;
    Client   file_keys::FileKeystateBody        => "/v1/read/file-keystate",             file_keys::FileKeystateResponse;

    Client   vault::SaveVaultMessageBody        => "/v1/vault/save",                     StatusOk, idempotent;
    Client   vault::DeleteVaultMessageBody      => "/v1/vault/delete",                   StatusOk, idempotent;
    Client   vault::VaultMessagesBody           => "/v1/read/vault",                     vault::VaultMessagesResponse;

    Client   emoji::CreateEmojiBody             => "/v1/emoji/create",                   StatusOk;
    Client   emoji::RemoveEmojiBody             => "/v1/emoji/remove",                   StatusOk;
    Client   emoji::EmojiGcBody                 => "/v1/emoji/gc",                       emoji::EmojiGcResponse;

    Client   profile::UpdateProfileBody         => "/v1/profile/update",                 StatusOk;
    Client   profile::SavePreferencesBody       => "/v1/profile/preferences",            StatusOk;
    Client   profile::AddBlock                  => "/v1/blocks/add",                     StatusOk;
    Client   profile::RemoveBlock               => "/v1/blocks/remove",                  StatusOk;
    Client   profile::CreateDmBody              => "/v1/dm/create",                      profile::CreatedDm;
    Client   profile::AcceptDmBody              => "/v1/dm/accept",                      StatusOk;
    Client   profile::AddDmMemberBody           => "/v1/dm/add",                         StatusOk;
    Client   profile::RemoveDmMemberBody        => "/v1/dm/remove",                      StatusOk;
    Client   profile::LeaveDmBody               => "/v1/dm/leave",                       StatusOk;

    Client   reports::ReportUserBody            => "/v1/reports",                        StatusOk;

    Client   profile::SaveReadCursorsBody       => "/v1/read-cursors/save",              StatusOk;

    Client   profile::SetChannelMuteBody        => "/v1/channel-mutes/set",              StatusOk;
    Client   profile::ChannelMutesBody          => "/v1/read/channel-mutes",             profile::ChannelMutesResponse;

    Client   devices::PublishKeyPackagesBody    => "/v1/key-packages",                   StatusOk;
    Client   devices::ClaimKeyPackageBody       => "/v1/key-packages/claim",             devices::ClaimKeyPackageResponse;
    Client   devices::ReplenishKeyPackagesBody  => "/v1/key-packages/replenish",         StatusOk;
    Client   devices::ResignDeviceCertsBody     => "/v1/devices/resign",                 StatusOk;
    Client   devices::PushTokenBody             => "/v1/push-tokens",                    StatusOk;
    Client   devices::ResolvePushHandleBody     => "/v1/push/resolve",                   devices::ResolvePushHandleResponse;
    Client   devices::DismissPushBody           => "/v1/push/dismiss",                   devices::DismissPushResponse;
    Client   devices::DismissedPushHandlesBody  => "/v1/push/dismissed",                 devices::DismissedPushHandlesResponse;

    Client   account::RotateIdentityBody        => "/v1/account/rotate-identity",        account::RotateIdentityResponse;
    Client   account::DeleteAccountBody         => "/v1/account/delete",                 StatusOk;
    Client   account::ResetRecoverBody          => "/v1/account/reset-recover",          StatusOk;
    Client   account::SecurityEventBody         => "/v1/security-events",                StatusOk;
    Client   account::ApproveEnrollmentBody     => "/v1/enrollment/approve",             StatusOk;
    Client   account::RejectEnrollmentBody      => "/v1/enrollment/reject",              StatusOk;
    Client   account::RevokeDeviceBody          => "/v1/devices/revoke",                 account::DeviceRevoked;
    Client   account::LogoutDeviceBody          => "/v1/auth/logout",                    StatusOk;

    Client   otp::RequestOtpBody                => "/v1/auth/request-otp",               StatusOk;
    Client   otp::VerifyOtpBody                 => "/v1/auth/verify-otp",                otp::VerifyOtpResponse;
    Client   bootstrap::EstablishIdentityBody   => "/v1/auth/establish-identity",        StatusOk;
    Client   bootstrap::RegisterDeviceBody      => "/v1/auth/register-device",           StatusOk;
    Client   bootstrap::PublishCertBody         => "/v1/auth/publish-device-cert",       StatusOk;
    Client   bootstrap::EnrollmentRequestBody   => "/v1/auth/enrollment-request",        StatusOk;
    Client   links::CreateLinkBody              => "/v1/link/create",                    links::CreateLinkResponse;
    Client   links::ClaimLinkBody               => "/v1/link/claim",                     links::ClaimLinkResponse;
    Client   links::LinkStatusBody              => "/v1/link/status",                    links::LinkStatusResponse;
    Client   links::OfferLinkBody               => "/v1/link/offer",                     links::OfferLinkResponse;
    Client   links::AcceptOfferBody             => "/v1/link/accept-offer",              links::AcceptOfferResponse;
    Client   links::PollOfferBody               => "/v1/link/poll-offer",                links::PollOfferResponse;
    Client   email_change::RequestEmailChangeBody => "/v1/auth/request-email-change-otp", StatusOk;
    Client   email_change::VerifyEmailChangeBody  => "/v1/auth/verify-email-change",      email_change::EmailChanged;

    Client   broker::LivekitTokenBody           => "/v1/livekit/token",                  broker::LivekitTokenResponse;
    Client   broker::LivekitSendDataBody        => "/v1/livekit/send-data",              broker::LivekitSendDataResponse;
    Client   broker::LivekitParticipantsBody    => "/v1/livekit/participants",           broker::LivekitParticipantsResponse;
    Client   broker::LivekitIdentitiesBody      => "/v1/livekit/identities",             broker::LivekitIdentitiesResponse;
    Client   broker::R2PresignBody              => "/v1/r2/presign",                     broker::R2PresignResponse, idempotent;

    // All POST: the signature does not cover a query string (#681).
    Client   directory::CatchUpBody              => "/v1/conversations/catch-up",         directory::CatchUpResponse;
    Client   directory::MessageLookupBody        => "/v1/messages/lookup",                directory::MessageLookupResponse;
    Client   directory::DirectoryBootstrapBody   => "/v1/directory/bootstrap",            directory::DirectoryBootstrapResponse;
    Client   directory::DirectoryGroupBody       => "/v1/directory/group",                directory::DirectoryGroupResponse;
    Client   directory::DirectoryMembersBody     => "/v1/directory/members",              directory::DirectoryMembersResponse;
    Client   directory::DirectoryUsersBody       => "/v1/directory/users",                directory::DirectoryUsersResponse;
    Client   directory::GroupBySlugBody          => "/v1/directory/group-by-slug",        directory::GroupBySlugResponse;
    Client   directory::DirectoryConversationsBody => "/v1/directory/conversations",      directory::DirectoryConversationsResponse;

    Client   account_reads::AccountKeysBody      => "/v1/read/account-keys",              account_reads::AccountKeysResponse;
    Client   account_reads::DevicesBody          => "/v1/read/devices",                   account_reads::DevicesResponse;
    Client   account_reads::KeyPackagesBody      => "/v1/read/key-packages",              account_reads::KeyPackagesResponse;
    Client   account_reads::RegisteredDevicesBody => "/v1/read/registered-devices",       account_reads::RegisteredDevicesResponse;
    Client   account_reads::RosterIdentitiesBody => "/v1/read/roster-identities",        account_reads::RosterIdentitiesResponse;
    Client   account_reads::EnrollmentRequestBody => "/v1/read/enrollment",               account_reads::EnrollmentRequestResponse;
    Client   account_reads::PendingEnrollmentsBody => "/v1/read/pending-enrollments",     account_reads::PendingEnrollmentsResponse;
    Client   account_reads::RecoveryBlobBody     => "/v1/read/recovery-blob",             account_reads::RecoveryBlobResponse;
    Client   account_reads::AccountStatusBody    => "/v1/read/account-status",            account_reads::AccountStatusResponse;
    Client   account_reads::AccountProbeBody     => "/v1/auth/account-probe",             account_reads::AccountProbeResponse;
    Client   account_reads::SecurityEventsBody   => "/v1/read/security-events",           account_reads::SecurityEventsResponse;
    Client   account_reads::EmojiReadBody        => "/v1/read/emoji",                     account_reads::EmojiReadResponse;
    Client   account_reads::ObjectExistsBody     => "/v1/read/objects",                   account_reads::ObjectExistsResponse;
    Client   account_reads::ReadCursorsBody      => "/v1/read/read-cursors",             account_reads::ReadCursorsResponse;
}

/// The guarantee, executable: a body missing a field does not compile.
///
/// The bug that shipped: `/v1/messages/delete` sent no actor and 403'd with auth disabled.
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
/// Naming every field compiles:
///
/// ```
/// use pollis_api::messages::DeleteMessageBody;
/// let _ = DeleteMessageBody {
///     message_id: "m".into(),
///     conversation_id: "c".into(),
///     msg_sender_id: None,
///     actor_id: Some("u".into()),
///     delete_token: None,
/// };
/// ```
///
/// A field the server does not have is [E0560] "no field on type":
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
/// The path is a property of the type:
///
/// ```
/// use pollis_api::DsRequest;
/// assert_eq!(pollis_api::messages::SendMessageBody::PATH, "/v1/messages/send");
/// assert_eq!(pollis_api::emoji::CreateEmojiBody::PATH, "/v1/emoji/create");
/// ```
///
/// # Responses (#922)
///
/// `answer` stands in for `ok_response::<B>` and `ds_post_json::<B>`, which share this signature:
///
/// ```
/// # use pollis_api::DsRequest;
/// /// Produce the ONE body this endpoint is declared to answer with.
/// fn answer<B: DsRequest>(_: B::Response) {}
/// ```
///
/// A field the server forgets is [E0063]:
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
/// A field the client invents is [E0609] "no field on type":
///
/// ```compile_fail,E0609
/// use pollis_api::broker::LivekitTokenResponse;
/// fn read(r: LivekitTokenResponse) -> String {
///     // The wire field is `url`. A client that spells it otherwise now fails
///     // to compile instead of quietly defaulting to the empty string and
///     // dialling its compiled-in SFU.
///     r.livekit_url
/// }
/// ```
///
/// An endpoint cannot answer with another endpoint's body — [E0308]:
///
/// ```compile_fail,E0308
/// # use pollis_api::DsRequest;
/// # fn answer<B: DsRequest>(_: B::Response) {}
/// use pollis_api::{broker::LivekitTokenBody, StatusOk};
/// // `/v1/livekit/token` answers a token, not `{"status":"ok"}`.
/// answer::<LivekitTokenBody>(StatusOk::Ok);
/// ```
///
/// The matching positive case, so the failures above mean something:
///
/// ```
/// # use pollis_api::DsRequest;
/// # fn answer<B: DsRequest>(_: B::Response) {}
/// use pollis_api::broker::{LivekitTokenBody, LivekitTokenResponse};
/// answer::<LivekitTokenBody>(LivekitTokenResponse {
///     token: "jwt".into(),
///     url: "wss://sfu".into(),
/// });
/// ```
///
/// A send-path attachment registration cannot omit its message (#925):
///
/// ```compile_fail,E0063
/// use pollis_api::messages::AttachmentRegisterBody;
/// let _ = AttachmentRegisterBody::ForMessage {
///     content_hash: "h".into(),
///     r2_key: "media/h.enc".into(),
///     delete_token: None,
///     // message_id omitted — the whole point of this variant. Does not compile.
/// };
/// ```
///
/// And the upload-time registration cannot invent one, because that variant has
/// no such field ([E0559]):
///
/// ```compile_fail,E0559
/// use pollis_api::messages::AttachmentRegisterBody;
/// let _ = AttachmentRegisterBody::ObjectOnly {
///     content_hash: "h".into(),
///     r2_key: "media/h.enc".into(),
///     message_id: "m".into(),
/// };
/// ```
///
/// An operator-only endpoint has no client half — [E0277]:
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
                group_name: Some("Team".into()),
            },
            r#"{"status":"ok","group_id":"g1","group_name":"Team"}"#,
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
        // The roster carries no `kind` (`skip_serializing_if`, so its bytes are unchanged since #836).
        round_trip(
            &broker::LivekitParticipantsResponse {
                participants: vec![broker::ResolvedIdentity {
                    identity: "d-x".into(),
                    user_id: "u1".into(),
                    name: "alice".into(),
                    kind: None,
                }],
            },
            r#"{"participants":[{"identity":"d-x","user_id":"u1","name":"alice"}]}"#,
        );
        // Resolving raw identities DOES carry it.
        round_trip(
            &broker::LivekitIdentitiesResponse {
                identities: vec![broker::ResolvedIdentity {
                    identity: "v-x".into(),
                    user_id: "u1".into(),
                    name: "alice".into(),
                    kind: Some("voice".into()),
                }],
            },
            r#"{"identities":[{"identity":"v-x","user_id":"u1","name":"alice","kind":"voice"}]}"#,
        );
        // Pinned messages (#99): the CAS answer and base64 ciphertext reads.
        round_trip(
            &pins::PinKeystateUpserted::Ok { updated: true },
            r#"{"status":"ok","updated":true}"#,
        );
        round_trip(
            &pins::PinKeystateResponse {
                keystate: Some(pins::StoredPinKeystate {
                    wrapped_kpin: "d3JhcA".into(),
                    nonce: "bm9uY2U".into(),
                    generation: 0,
                    epoch: 7,
                }),
            },
            r#"{"keystate":{"wrapped_kpin":"d3JhcA","nonce":"bm9uY2U","generation":0,"epoch":7}}"#,
        );
        // Attachment key ring (#1382): lowercase op tag; reads carry the ring length.
        round_trip(
            &file_keys::FileKeystateOp::Rotate,
            r#""rotate""#,
        );
        round_trip(
            &file_keys::FileKeystateResponse {
                keystate: Some(file_keys::StoredFileKeystate {
                    wrapped_keyring: "d3JhcA".into(),
                    nonce: "bm9uY2U".into(),
                    key_version: 2,
                    generation: 0,
                    epoch: 7,
                }),
            },
            r#"{"keystate":{"wrapped_keyring":"d3JhcA","nonce":"bm9uY2U","key_version":2,"generation":0,"epoch":7}}"#,
        );
        round_trip(
            &pins::ListPinsResponse {
                pins: vec![pins::StoredPin {
                    id: "p1".into(),
                    conversation_id: "c1".into(),
                    message_id: "m1".into(),
                    pinned_by: "u1".into(),
                    encrypted_content: "Y3Q".into(),
                    nonce: "bm9uY2U".into(),
                    pinned_at: "2026-01-01T00:00:00Z".into(),
                }],
            },
            r#"{"pins":[{"id":"p1","conversation_id":"c1","message_id":"m1","pinned_by":"u1","encrypted_content":"Y3Q","nonce":"bm9uY2U","pinned_at":"2026-01-01T00:00:00Z"}]}"#,
        );
        // Vault (#107): opaque rows, byte-exact custody.
        round_trip(
            &vault::VaultMessagesResponse {
                messages: vec![vault::StoredVaultMessage {
                    id: "v1".into(),
                    nonce: "bm9uY2U".into(),
                    blob: "Y3Q".into(),
                    created_at: "2026-01-01T00:00:00Z".into(),
                    updated_at: "2026-01-02T00:00:00Z".into(),
                }],
            },
            r#"{"messages":[{"id":"v1","nonce":"bm9uY2U","blob":"Y3Q","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-02T00:00:00Z"}]}"#,
        );
        round_trip(
            &otp::VerifyOtpResponse {
                user_id: "u".into(),
                username: "alice".into(),
                is_new_account: false,
                has_identity: true,
                session_token: "s".into(),
                session_expires_at: 42,
                account_id_pub: Some("AQID".into()),
            },
            r#"{"user_id":"u","username":"alice","is_new_account":false,"has_identity":true,"session_token":"s","session_expires_at":42,"account_id_pub":"AQID"}"#,
        );
    }

    /// `{"status":"ok"}` is a fixed literal, not a free-form string.
    #[test]
    fn a_status_other_than_ok_does_not_decode() {
        assert!(serde_json::from_str::<StatusOk>(r#"{"status":"ok"}"#).is_ok());
        assert!(serde_json::from_str::<StatusOk>(r#"{"status":"borked"}"#).is_err());
        // And a counted response is not satisfied by a bare ok — the count is
        // part of the shape, not an optional extra.
        assert!(serde_json::from_str::<writes::WelcomesUpdated>(r#"{"status":"ok"}"#).is_err());
    }

    /// Splitting `AttachmentRegisterBody` must not change the wire (#925).
    ///
    /// Deployed endpoint: `untagged` keeps the wire, and variant ORDER keeps `message_id` from being dropped.
    #[test]
    fn the_attachment_register_split_is_wire_compatible() {
        use messages::AttachmentRegisterBody as B;

        let for_message = B::ForMessage {
            content_hash: "h".into(),
            r2_key: "media/h.enc".into(),
            message_id: "m1".into(),
            delete_token: None,
        };
        assert_eq!(
            serde_json::to_string(&for_message).unwrap(),
            r#"{"content_hash":"h","r2_key":"media/h.enc","message_id":"m1"}"#
        );

        let object_only = B::ObjectOnly {
            content_hash: "h".into(),
            r2_key: "media/h.enc".into(),
        };
        assert_eq!(
            serde_json::to_string(&object_only).unwrap(),
            r#"{"content_hash":"h","r2_key":"media/h.enc"}"#
        );

        // What a current client sends on the send path.
        let decoded: B =
            serde_json::from_str(r#"{"content_hash":"h","r2_key":"k","message_id":"m1"}"#).unwrap();
        assert_eq!(decoded.message_id(), Some("m1"), "the reference must survive");

        // What the upload-time dedup path — and a pre-#690 client — sends.
        let decoded: B = serde_json::from_str(r#"{"content_hash":"h","r2_key":"k"}"#).unwrap();
        assert_eq!(decoded.message_id(), None);
        assert_eq!(decoded.content_hash(), "h");
        assert_eq!(decoded.r2_key(), "k");

        // An explicit null is the old `Option`'s absent case, not a message id.
        let decoded: B =
            serde_json::from_str(r#"{"content_hash":"h","r2_key":"k","message_id":null}"#).unwrap();
        assert_eq!(decoded.message_id(), None);
    }

    /// Every `Operator` endpoint is written down in the runbook (#925).
    ///
    /// [`ClientRequest`] blocks client callers structurally; this checks a human was told the endpoint exists.
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
