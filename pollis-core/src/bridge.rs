//! Generic uniffi-exposed command dispatcher.
//!
//! Lets a non-Tauri host (currently `pollis-native` for mobile, future
//! CLI/TUI) drive the same command surface the desktop hits via Tauri's
//! `invoke()`. JS calls `init_pollis(config_json)` once at startup, then
//! `invoke("cmd_name", args_json)` for everything else — mirroring the
//! `@tauri-apps/api/core` `invoke()` shape so call-sites port 1:1.
//!
//! Lifecycle:
//!   - `init_pollis` constructs `AppState` once, parks it in a process-
//!     global `OnceCell`. Idempotent; subsequent calls are no-ops.
//!   - `invoke` looks up the state and dispatches on the command name,
//!     deserializing args / serializing return values as JSON. Unknown
//!     commands return a clear error string.
//!
//! Adding a command: add a `match` arm. Args are read off `args` via the
//! `arg!` / `arg_opt!` helpers; the body calls into `crate::commands::*`
//! the same way the Tauri shim does, and the return value goes through
//! `ok(...)` so it round-trips as a JSON string on the JS side.

use serde_json::Value;
use std::sync::Arc;
use tokio::sync::OnceCell;

use crate::config::Config;
use crate::state::AppState;

static APP_STATE: OnceCell<Arc<AppState>> = OnceCell::const_new();

#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum BridgeError {
    #[error("{0}")]
    Bridge(String),
}

impl From<crate::error::Error> for BridgeError {
    fn from(e: crate::error::Error) -> Self {
        BridgeError::Bridge(e.to_string())
    }
}
impl From<serde_json::Error> for BridgeError {
    fn from(e: serde_json::Error) -> Self {
        BridgeError::Bridge(format!("json: {e}"))
    }
}
impl From<anyhow::Error> for BridgeError {
    fn from(e: anyhow::Error) -> Self {
        BridgeError::Bridge(e.to_string())
    }
}

#[derive(serde::Deserialize)]
struct InitConfig {
    turso_url: String,
    turso_token: String,
    #[serde(default)]
    r2_endpoint: String,
    #[serde(default)]
    r2_access_key_id: String,
    #[serde(default)]
    r2_secret_access_key: String,
    #[serde(default)]
    r2_region: Option<String>,
    #[serde(default)]
    r2_public_url: String,
    #[serde(default)]
    livekit_url: String,
    #[serde(default)]
    livekit_api_key: String,
    #[serde(default)]
    livekit_api_secret: String,
    #[serde(default)]
    resend_api_key: String,
}

/// Initialize the process-global `AppState`. Safe to call multiple times —
/// only the first call does any work. `config_json` is a JSON object whose
/// fields mirror [`Config`]; the only required fields are `turso_url` and
/// `turso_token` (the rest default to empty so a mobile build that doesn't
/// use R2 / LiveKit yet can omit them).
#[uniffi::export(async_runtime = "tokio")]
pub async fn init_pollis(config_json: String) -> Result<(), BridgeError> {
    APP_STATE
        .get_or_try_init(|| async {
            let parsed: InitConfig = serde_json::from_str(&config_json)?;
            let config = Config {
                turso_url: parsed.turso_url,
                turso_token: parsed.turso_token,
                r2_endpoint: parsed.r2_endpoint,
                r2_access_key_id: parsed.r2_access_key_id,
                r2_secret_access_key: parsed.r2_secret_access_key,
                r2_region: parsed.r2_region.unwrap_or_else(|| "auto".into()),
                r2_public_url: parsed.r2_public_url,
                livekit_url: parsed.livekit_url,
                livekit_api_key: parsed.livekit_api_key,
                livekit_api_secret: parsed.livekit_api_secret,
                resend_api_key: parsed.resend_api_key,
            };
            let state = AppState::new(config).await?;
            Ok::<Arc<AppState>, BridgeError>(Arc::new(state))
        })
        .await?;
    Ok(())
}

fn state() -> Result<Arc<AppState>, BridgeError> {
    APP_STATE.get().cloned().ok_or_else(|| {
        BridgeError::Bridge(
            "pollis-core not initialized — call init_pollis() first".into(),
        )
    })
}

fn arg<T: serde::de::DeserializeOwned>(v: &Value, key: &str) -> Result<T, BridgeError> {
    let field = v
        .get(key)
        .ok_or_else(|| BridgeError::Bridge(format!("missing arg: {key}")))?;
    serde_json::from_value(field.clone())
        .map_err(|e| BridgeError::Bridge(format!("arg {key}: {e}")))
}

fn arg_opt<T: serde::de::DeserializeOwned>(
    v: &Value,
    key: &str,
) -> Result<Option<T>, BridgeError> {
    let Some(field) = v.get(key) else {
        return Ok(None);
    };
    if field.is_null() {
        return Ok(None);
    }
    Ok(Some(
        serde_json::from_value(field.clone())
            .map_err(|e| BridgeError::Bridge(format!("arg {key}: {e}")))?,
    ))
}

fn ok<T: serde::Serialize>(v: T) -> Result<String, BridgeError> {
    serde_json::to_string(&v).map_err(BridgeError::from)
}

/// Dispatch a command by name. `args_json` must be a JSON object; pass
/// `"{}"` (or `""`) for nullary commands. Returns the command's result
/// re-serialized as a JSON string (the JS side `JSON.parse`s it inside
/// `mobile/lib/native/bridge.ts`).
///
/// The initial command set is deliberately small — enough to wire auth
/// + the static screens. Add commands here as the mobile UI needs them.
#[uniffi::export(async_runtime = "tokio")]
pub async fn invoke(cmd: String, args_json: String) -> Result<String, BridgeError> {
    let args: Value = if args_json.trim().is_empty() {
        Value::Object(Default::default())
    } else {
        serde_json::from_str(&args_json)?
    };

    use crate::commands::{auth, device_enrollment, dm, groups, messages, pin, user};

    match cmd.as_str() {
        "version" => ok(env!("CARGO_PKG_VERSION")),

        // ----- auth -----
        "get_identity" => ok(auth::get_identity().await?),
        "get_session" => ok(auth::get_session(&state()?).await?),
        "request_otp" => {
            let email: String = arg(&args, "email")?;
            auth::request_otp(&state()?, email).await?;
            ok(())
        }
        "verify_otp" => {
            let email: String = arg(&args, "email")?;
            let code: String = arg(&args, "code")?;
            ok(auth::verify_otp(&state()?, email, code).await?)
        }
        "initialize_identity" => {
            let user_id: String = arg(&args, "userId")?;
            ok(auth::initialize_identity(&state()?, user_id).await?)
        }
        "logout" => {
            let delete: bool = arg_opt(&args, "deleteData")?.unwrap_or(false);
            auth::logout(&state()?, delete).await?;
            ok(())
        }
        "list_known_accounts" => ok(auth::list_known_accounts()?),
        // ----- device enrollment -----
        "start_device_enrollment" => {
            let user_id: String = arg(&args, "userId")?;
            ok(device_enrollment::start_device_enrollment(&state()?, user_id).await?)
        }
        "poll_enrollment_status" => {
            let request_id: String = arg(&args, "requestId")?;
            ok(device_enrollment::poll_enrollment_status(&state()?, request_id).await?)
        }
        "finalize_device_enrollment" => {
            let user_id: String = arg(&args, "userId")?;
            device_enrollment::finalize_device_enrollment(&state()?, user_id).await?;
            ok(())
        }
        "recover_with_secret_key" => {
            let user_id: String = arg(&args, "userId")?;
            let secret_key: String = arg(&args, "secretKey")?;
            device_enrollment::recover_with_secret_key(&state()?, user_id, secret_key).await?;
            ok(())
        }
        "list_pending_enrollment_requests" => {
            let user_id: String = arg(&args, "userId")?;
            ok(device_enrollment::list_pending_enrollment_requests(&state()?, user_id).await?)
        }
        "approve_device_enrollment" => {
            let request_id: String = arg(&args, "requestId")?;
            let verification_code: String = arg(&args, "verificationCode")?;
            device_enrollment::approve_device_enrollment(
                &state()?,
                request_id,
                verification_code,
            )
            .await?;
            ok(())
        }
        "reject_device_enrollment" => {
            let request_id: String = arg(&args, "requestId")?;
            device_enrollment::reject_device_enrollment(&state()?, request_id).await?;
            ok(())
        }
        "list_user_devices" => {
            let user_id: String = arg(&args, "userId")?;
            ok(auth::list_user_devices(&state()?, user_id).await?)
        }
        "revoke_device" => {
            let user_id: String = arg(&args, "userId")?;
            let device_id: String = arg(&args, "deviceId")?;
            auth::revoke_device(&state()?, user_id, device_id).await?;
            ok(())
        }

        // ----- pin -----
        "set_pin" => {
            let old_pin: Option<String> = arg_opt(&args, "oldPin")?;
            let new_pin: String = arg(&args, "newPin")?;
            pin::set_pin(&state()?, old_pin, new_pin).await?;
            ok(())
        }
        "unlock" => {
            let user_id: String = arg(&args, "userId")?;
            let p: String = arg(&args, "pin")?;
            ok(pin::unlock(&state()?, user_id, p).await?)
        }
        "lock" => {
            pin::lock(&state()?).await?;
            ok(())
        }
        "get_unlock_state" => ok(pin::get_unlock_state(&state()?).await?),

        // ----- user -----
        "get_user_profile" => {
            let user_id: String = arg(&args, "userId")?;
            ok(user::get_user_profile(user_id, &state()?).await?)
        }
        "update_user_profile" => {
            let user_id: String = arg(&args, "userId")?;
            let username: Option<String> = arg_opt(&args, "username")?;
            let preferred_name: Option<String> = arg_opt(&args, "preferredName")?;
            let phone: Option<String> = arg_opt(&args, "phone")?;
            let avatar_url: Option<String> = arg_opt(&args, "avatarUrl")?;
            user::update_user_profile(
                user_id,
                username,
                preferred_name,
                phone,
                avatar_url,
                &state()?,
            )
            .await?;
            ok(())
        }
        "search_user_by_username" => {
            let username: String = arg(&args, "username")?;
            ok(user::search_user_by_username(username, &state()?).await?)
        }
        "get_preferences" => {
            let user_id: String = arg(&args, "userId")?;
            // Rust returns a JSON-encoded string; the JS bridge will
            // wrap it in JSON.stringify again to feed our serde_json
            // ser path. The TS hook JSON.parses the outer wrapper to
            // recover the raw blob.
            ok(user::get_preferences(user_id, &state()?).await?)
        }
        "save_preferences" => {
            let user_id: String = arg(&args, "userId")?;
            let preferences_json: String = arg(&args, "preferencesJson")?;
            user::save_preferences(user_id, preferences_json, &state()?).await?;
            ok(())
        }

        // ----- groups -----
        "list_user_groups" => {
            let user_id: String = arg(&args, "userId")?;
            ok(groups::list_user_groups(user_id, &state()?).await?)
        }
        "list_user_groups_with_channels" => {
            let user_id: String = arg(&args, "userId")?;
            ok(groups::list_user_groups_with_channels(user_id, &state()?).await?)
        }
        "list_group_channels" => {
            let group_id: String = arg(&args, "groupId")?;
            ok(groups::list_group_channels(group_id, &state()?).await?)
        }
        "create_group" => {
            let name: String = arg(&args, "name")?;
            let description: Option<String> = arg_opt(&args, "description")?;
            let owner_id: String = arg(&args, "ownerId")?;
            let create_default_text_channel: Option<bool> =
                arg_opt(&args, "createDefaultTextChannel")?;
            let create_default_voice_channel: Option<bool> =
                arg_opt(&args, "createDefaultVoiceChannel")?;
            ok(groups::create_group(
                name,
                description,
                owner_id,
                create_default_text_channel,
                create_default_voice_channel,
                &state()?,
            )
            .await?)
        }
        "get_group_members" => {
            let group_id: String = arg(&args, "groupId")?;
            ok(groups::get_group_members(group_id, &state()?).await?)
        }
        "leave_group" => {
            let group_id: String = arg(&args, "groupId")?;
            let user_id: String = arg(&args, "userId")?;
            groups::leave_group(group_id, user_id, &state()?).await?;
            ok(())
        }
        "send_group_invite" => {
            let group_id: String = arg(&args, "groupId")?;
            let inviter_id: String = arg(&args, "inviterId")?;
            let invitee_identifier: String = arg(&args, "inviteeIdentifier")?;
            groups::send_group_invite(
                group_id,
                inviter_id,
                invitee_identifier,
                &state()?,
            )
            .await?;
            ok(())
        }
        "get_pending_invites" => {
            let user_id: String = arg(&args, "userId")?;
            ok(groups::get_pending_invites(user_id, &state()?).await?)
        }
        "accept_group_invite" => {
            let invite_id: String = arg(&args, "inviteId")?;
            let user_id: String = arg(&args, "userId")?;
            groups::accept_group_invite(invite_id, user_id, &state()?).await?;
            ok(())
        }
        "decline_group_invite" => {
            let invite_id: String = arg(&args, "inviteId")?;
            let user_id: String = arg(&args, "userId")?;
            groups::decline_group_invite(invite_id, user_id, &state()?).await?;
            ok(())
        }
        "create_channel" => {
            let group_id: String = arg(&args, "groupId")?;
            let name: String = arg(&args, "name")?;
            let description: Option<String> = arg_opt(&args, "description")?;
            let channel_type: Option<String> = arg_opt(&args, "channelType")?;
            let creator_id: String = arg(&args, "creatorId")?;
            ok(groups::create_channel(
                group_id,
                name,
                description,
                channel_type,
                creator_id,
                &state()?,
            )
            .await?)
        }

        // ----- messages -----
        "list_messages" => {
            let conversation_id: String = arg(&args, "conversationId")?;
            let limit: Option<i64> = arg_opt(&args, "limit")?;
            let before_id: Option<String> = arg_opt(&args, "beforeId")?;
            ok(messages::list_messages(
                conversation_id,
                limit,
                before_id,
                &state()?,
            )
            .await?)
        }
        "list_channel_previews" => {
            let user_id: String = arg(&args, "userId")?;
            ok(messages::list_channel_previews(user_id, &state()?).await?)
        }
        "send_message" => {
            let conversation_id: String = arg(&args, "conversationId")?;
            let sender_id: String = arg(&args, "senderId")?;
            let content: String = arg(&args, "content")?;
            let reply_to_id: Option<String> = arg_opt(&args, "replyToId")?;
            let sender_username: Option<String> = arg_opt(&args, "senderUsername")?;
            ok(messages::send_message(
                conversation_id,
                sender_id,
                content,
                reply_to_id,
                sender_username,
                &state()?,
            )
            .await?)
        }
        "get_channel_messages" => {
            let user_id: String = arg(&args, "userId")?;
            let channel_id: String = arg(&args, "channelId")?;
            let limit: Option<i64> = arg_opt(&args, "limit")?;
            let cursor: Option<messages::MessageCursor> = arg_opt(&args, "cursor")?;
            ok(messages::get_channel_messages(
                user_id, channel_id, limit, cursor, &state()?,
            )
            .await?)
        }
        "get_dm_messages" => {
            let user_id: String = arg(&args, "userId")?;
            let dm_channel_id: String = arg(&args, "dmChannelId")?;
            let limit: Option<i64> = arg_opt(&args, "limit")?;
            let cursor: Option<messages::MessageCursor> = arg_opt(&args, "cursor")?;
            ok(messages::get_dm_messages(
                user_id, dm_channel_id, limit, cursor, &state()?,
            )
            .await?)
        }
        "ingest_channel_envelopes" => {
            let user_id: String = arg(&args, "userId")?;
            let channel_id: String = arg(&args, "channelId")?;
            messages::ingest_channel_envelopes(user_id, channel_id, &state()?).await?;
            ok(())
        }
        "ingest_dm_envelopes" => {
            let user_id: String = arg(&args, "userId")?;
            let dm_channel_id: String = arg(&args, "dmChannelId")?;
            messages::ingest_dm_envelopes(user_id, dm_channel_id, &state()?).await?;
            ok(())
        }

        // ----- dm -----
        "list_dm_channels" => {
            let user_id: String = arg(&args, "userId")?;
            ok(dm::list_dm_channels(user_id, &state()?).await?)
        }
        "list_dm_requests" => {
            let user_id: String = arg(&args, "userId")?;
            ok(dm::list_dm_requests(user_id, &state()?).await?)
        }
        "accept_dm_request" => {
            let dm_channel_id: String = arg(&args, "dmChannelId")?;
            let user_id: String = arg(&args, "userId")?;
            dm::accept_dm_request(dm_channel_id, user_id, &state()?).await?;
            ok(())
        }
        "create_dm_channel" => {
            let creator_id: String = arg(&args, "creatorId")?;
            let member_ids: Vec<String> = arg(&args, "memberIds")?;
            ok(dm::create_dm_channel(creator_id, member_ids, &state()?).await?)
        }

        _ => Err(BridgeError::Bridge(format!(
            "unknown command: {cmd} (add it to pollis-core/src/bridge.rs)"
        ))),
    }
}
