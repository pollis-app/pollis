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
use std::sync::{Arc, OnceLock};
use tokio::sync::OnceCell;

use crate::config::Config;
use crate::state::AppState;

static APP_STATE: OnceCell<Arc<AppState>> = OnceCell::const_new();

/// A dedicated multi-threaded Tokio runtime whose worker threads carry a
/// large stack, used to run *all* bridge command work off the calling
/// thread.
///
/// Why this exists: uniffi-bindgen-react-native drives exported `async`
/// functions by polling their futures **directly on the JS/Hermes thread**
/// (see the `ffi_*_rust_future_poll` → JSI call path). That thread has a
/// small stack — roughly 1 MB on iOS, often less on Android — while some
/// synchronous work we call into needs far more. The original offender was
/// `libsql`, which parsed every SQL statement client-side through a
/// deeply-recursive lemon parser (`yyParser::yy_reduce`) and blew past the JS
/// thread's guard page, crashing the whole app with SIGBUS
/// (`KERN_PROTECTION_FAILURE` at the stack guard region) on the first query.
/// #987 removed `libsql` from the client, but the rule stands on its own:
/// SQLCipher's parser is the same shape on the local database, and MLS
/// operations are not modest either. Desktop never hits this because Tauri
/// already runs commands on a multi-threaded Tokio runtime with generous
/// worker stacks.
///
/// By spawning the real work onto these big-stack workers and only
/// `await`ing the join handle on the JS thread, the JS thread does nothing
/// stack-hungry and the parser gets the headroom it needs.
fn worker_runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .thread_stack_size(8 * 1024 * 1024)
            .thread_name("pollis-bridge")
            .enable_all()
            .build()
            .expect("failed to build pollis bridge worker runtime")
    })
}

/// Run a command future on the big-stack [`worker_runtime`] and await its
/// result from the (small-stack) calling thread. The closure builds the
/// future on the worker so nothing stack-hungry runs on the JS thread.
async fn run_on_worker<F, T>(fut: F) -> Result<T, BridgeError>
where
    F: std::future::Future<Output = Result<T, BridgeError>> + Send + 'static,
    T: Send + 'static,
{
    worker_runtime()
        .spawn(fut)
        .await
        .map_err(|e| BridgeError::Bridge(format!("bridge worker task failed: {e}")))?
}

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

/// The JSON the mobile host hands `init_pollis`.
///
/// `turso_url` / `turso_token` / `log_db_url` / `log_db_token` are gone (#987) —
/// the client holds no database credential, so the host has none to pass. Extra
/// keys are IGNORED rather than rejected (serde's default), so a mobile build
/// still sending the old `EXPO_PUBLIC_TURSO_*` values starts cleanly instead of
/// failing at init; they simply do nothing.
#[derive(serde::Deserialize)]
struct InitConfig {
    /// Absolute path of a writable directory the bridge can park per-app
    /// state in. Required on Android (we drop the CA bundle there and
    /// scope `POLLIS_DATA_DIR` to it); optional everywhere else, where
    /// the platform already gives the binary a sensible default via
    /// `db::dirs_path()`.
    #[serde(default)]
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    data_dir: Option<String>,
    #[serde(default)]
    r2_endpoint: String,
    #[serde(default)]
    r2_public_url: String,
    #[serde(default)]
    livekit_url: String,
    /// Optional Delivery Service base URL. Absent → direct Turso writes.
    #[serde(default)]
    pollis_delivery_url: Option<String>,
}

/// Initialize the process-global `AppState`. Safe to call multiple times —
/// only the first call does any work. `config_json` is a JSON object whose
/// fields mirror [`Config`]; every field defaults, so a mobile build that does
/// not use R2 / LiveKit yet can omit them all. In practice `pollis_delivery_url`
/// is the one that matters: since #987 it is the only backend there is.
#[uniffi::export(async_runtime = "tokio")]
pub async fn init_pollis(config_json: String) -> Result<(), BridgeError> {
    run_on_worker(init_pollis_inner(config_json)).await
}

async fn init_pollis_inner(config_json: String) -> Result<(), BridgeError> {
    APP_STATE
        .get_or_try_init(|| async {
            let parsed: InitConfig = serde_json::from_str(&config_json)?;
            #[cfg(target_os = "android")]
            android_bootstrap(parsed.data_dir.as_deref())?;
            let config = Config {
                r2_endpoint: parsed.r2_endpoint,
                r2_public_url: parsed.r2_public_url,
                livekit_url: parsed.livekit_url,
                pollis_delivery_url: parsed.pollis_delivery_url.filter(|s| !s.is_empty()),
                // Overlay is off on the mobile bridge path in Slice 1b: the shim
                // + connector wiring ships desktop-first (config-from-env).
                // Mobile opt-in follows once the relay pool is deployed (Slice 2).
                overlay_mode: pollis_relay::OverlayMode::Off,
                overlay_relay_url: None,
                overlay_relay_cert: None,
                overlay_directory_url: None,
                overlay_directory_key: None,
            };
            let state = AppState::new(config).await?;
            Ok::<Arc<AppState>, BridgeError>(Arc::new(state))
        })
        .await?;
    Ok(())
}

#[cfg(target_os = "android")]
fn android_bootstrap(data_dir: Option<&str>) -> Result<(), BridgeError> {
    let dir = data_dir.ok_or_else(|| {
        BridgeError::Bridge(
            "init_pollis on Android needs `data_dir` set to a writable sandbox \
             path (Expo's FileSystem.documentDirectory)"
                .into(),
        )
    })?;
    let path = std::path::PathBuf::from(dir);
    crate::android_tls::install(&path).map_err(|e| {
        BridgeError::Bridge(format!("android_tls install at {}: {e}", path.display()))
    })?;
    // SAFETY: see `android_tls::set_env`.
    unsafe {
        std::env::set_var("POLLIS_DATA_DIR", &path);
    }
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
    run_on_worker(invoke_inner(cmd, args_json)).await
}

async fn invoke_inner(cmd: String, args_json: String) -> Result<String, BridgeError> {
    let args: Value = if args_json.trim().is_empty() {
        Value::Object(Default::default())
    } else {
        serde_json::from_str(&args_json)?
    };

    use crate::commands::{
        auth, blocks, bookmarks, device_enrollment, dm, emoji, groups, messages, pin,
        pinned_messages, safety, user, vault,
    };

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
        "poll_mls_welcomes" => {
            let user_id: String = arg(&args, "userId")?;
            crate::commands::mls::poll_mls_welcomes(&state()?, user_id).await?;
            ok(())
        }
        "logout" => {
            let delete: bool = arg_opt(&args, "deleteData")?.unwrap_or(false);
            auth::logout(&state()?, delete).await?;
            ok(())
        }
        "list_known_accounts" => ok(auth::list_known_accounts(&state()?).await?),
        "request_email_change_otp" => {
            let user_id: String = arg(&args, "userId")?;
            let new_email: String = arg(&args, "newEmail")?;
            auth::request_email_change_otp(&state()?, user_id, new_email).await?;
            ok(())
        }
        "verify_email_change" => {
            let user_id: String = arg(&args, "userId")?;
            let new_email: String = arg(&args, "newEmail")?;
            let code: String = arg(&args, "code")?;
            auth::verify_email_change(&state()?, user_id, new_email, code).await?;
            ok(())
        }
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
        // Authoritative, spoof-resistant check of whether THIS device is still
        // registered for the user. Mobile calls it from the inbox realtime
        // handler on a `device_revoked` nudge before deciding to self-sign-out.
        "is_current_device_registered" => {
            let user_id: String = arg(&args, "userId")?;
            ok(auth::is_current_device_registered(&state()?, user_id).await?)
        }
        "list_security_events" => {
            let user_id: String = arg(&args, "userId")?;
            let limit: Option<i64> = arg_opt(&args, "limit")?;
            ok(device_enrollment::list_security_events(&state()?, user_id, limit).await?)
        }
        "delete_account" => {
            let user_id: String = arg(&args, "userId")?;
            auth::delete_account(&state()?, user_id).await?;
            ok(())
        }
        "wipe_local_data" => {
            auth::wipe_local_data(&state()?).await?;
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
            let requester_id: String = arg(&args, "requesterId")?;
            ok(groups::list_group_channels(group_id, requester_id, &state()?).await?)
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
        "update_group" => {
            let group_id: String = arg(&args, "groupId")?;
            let requester_id: String = arg(&args, "requesterId")?;
            let name: Option<String> = arg_opt(&args, "name")?;
            let description: Option<String> = arg_opt(&args, "description")?;
            let icon_url: Option<String> = arg_opt(&args, "iconUrl")?;
            ok(groups::update_group(
                group_id,
                requester_id,
                name,
                description,
                icon_url,
                &state()?,
            )
            .await?)
        }
        "delete_group" => {
            let group_id: String = arg(&args, "groupId")?;
            let requester_id: String = arg(&args, "requesterId")?;
            groups::delete_group(group_id, requester_id, &state()?).await?;
            ok(())
        }
        "update_channel" => {
            let channel_id: String = arg(&args, "channelId")?;
            let requester_id: String = arg(&args, "requesterId")?;
            let name: Option<String> = arg_opt(&args, "name")?;
            let description: Option<String> = arg_opt(&args, "description")?;
            ok(groups::update_channel(
                channel_id,
                requester_id,
                name,
                description,
                &state()?,
            )
            .await?)
        }
        "delete_channel" => {
            let channel_id: String = arg(&args, "channelId")?;
            let requester_id: String = arg(&args, "requesterId")?;
            groups::delete_channel(channel_id, requester_id, &state()?).await?;
            ok(())
        }
        "remove_member_from_group" => {
            let group_id: String = arg(&args, "groupId")?;
            let user_id: String = arg(&args, "userId")?;
            let requester_id: String = arg(&args, "requesterId")?;
            groups::remove_member_from_group(group_id, user_id, requester_id, &state()?).await?;
            ok(())
        }
        "search_group_by_slug" => {
            let slug: String = arg(&args, "slug")?;
            ok(groups::search_group_by_slug(slug, &state()?).await?)
        }
        "request_group_access" => {
            let group_id: String = arg(&args, "groupId")?;
            let requester_id: String = arg(&args, "requesterId")?;
            groups::request_group_access(group_id, requester_id, &state()?).await?;
            ok(())
        }
        "get_group_join_requests" => {
            let group_id: String = arg(&args, "groupId")?;
            let requester_id: String = arg(&args, "requesterId")?;
            ok(groups::get_group_join_requests(group_id, requester_id, &state()?).await?)
        }
        "get_my_join_request" => {
            let group_id: String = arg(&args, "groupId")?;
            let requester_id: String = arg(&args, "requesterId")?;
            ok(groups::get_my_join_request(group_id, requester_id, &state()?).await?)
        }
        "approve_join_request" => {
            let request_id: String = arg(&args, "requestId")?;
            let approver_id: String = arg(&args, "approverId")?;
            groups::approve_join_request(request_id, approver_id, &state()?).await?;
            ok(())
        }
        "reject_join_request" => {
            let request_id: String = arg(&args, "requestId")?;
            let approver_id: String = arg(&args, "approverId")?;
            groups::reject_join_request(request_id, approver_id, &state()?).await?;
            ok(())
        }
        "set_member_role" => {
            let group_id: String = arg(&args, "groupId")?;
            let user_id: String = arg(&args, "userId")?;
            let role: String = arg(&args, "role")?;
            let requester_id: String = arg(&args, "requesterId")?;
            groups::set_member_role(group_id, user_id, role, requester_id, &state()?).await?;
            ok(())
        }
        "get_group_members" => {
            let group_id: String = arg(&args, "groupId")?;
            let requester_id: String = arg(&args, "requesterId")?;
            ok(groups::get_group_members(group_id, requester_id, &state()?).await?)
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
        // ----- invite links -----
        "create_group_invite_link" => {
            let group_id: String = arg(&args, "groupId")?;
            let creator_id: String = arg(&args, "creatorId")?;
            let expires_in_hours: Option<i64> = arg_opt(&args, "expiresInHours")?;
            let max_uses: Option<i64> = arg_opt(&args, "maxUses")?;
            ok(groups::create_group_invite_link(
                group_id,
                creator_id,
                expires_in_hours,
                max_uses,
                &state()?,
            )
            .await?)
        }
        "list_group_invite_links" => {
            let group_id: String = arg(&args, "groupId")?;
            let user_id: String = arg(&args, "userId")?;
            ok(groups::list_group_invite_links(group_id, user_id, &state()?).await?)
        }
        "revoke_group_invite_link" => {
            let link_id: String = arg(&args, "linkId")?;
            let user_id: String = arg(&args, "userId")?;
            groups::revoke_group_invite_link(link_id, user_id, &state()?).await?;
            ok(())
        }
        "redeem_group_invite_link" => {
            let token: String = arg(&args, "token")?;
            let user_id: String = arg(&args, "userId")?;
            ok(groups::redeem_group_invite_link(token, user_id, &state()?).await?)
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
        "send_message" => {
            let conversation_id: String = arg(&args, "conversationId")?;
            let sender_id: String = arg(&args, "senderId")?;
            let content: String = arg(&args, "content")?;
            let reply_to_id: Option<String> = arg_opt(&args, "replyToId")?;
            let thread_id: Option<String> = arg_opt(&args, "threadId")?;
            let sender_username: Option<String> = arg_opt(&args, "senderUsername")?;
            ok(messages::send_message(
                conversation_id,
                sender_id,
                content,
                reply_to_id,
                thread_id,
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
        // Local-first batched reads (#936): pages come straight from the local
        // DB; the caller fires ingest separately, exactly as desktop does.
        "read_channel_messages" => {
            let channel_id: String = arg(&args, "channelId")?;
            let limit: Option<i64> = arg_opt(&args, "limit")?;
            let cursor: Option<messages::MessageCursor> = arg_opt(&args, "cursor")?;
            ok(messages::read_channel_messages(channel_id, limit, cursor, &state()?).await?)
        }
        "read_dm_messages" => {
            let dm_channel_id: String = arg(&args, "dmChannelId")?;
            let limit: Option<i64> = arg_opt(&args, "limit")?;
            let cursor: Option<messages::MessageCursor> = arg_opt(&args, "cursor")?;
            ok(messages::read_dm_messages(dm_channel_id, limit, cursor, &state()?).await?)
        }
        "read_last_messages" => {
            let conversation_ids: Vec<String> = arg(&args, "conversationIds")?;
            ok(messages::read_last_messages(conversation_ids, &state()?).await?)
        }
        // ----- threads -----
        "read_thread_messages" => {
            let thread_id: String = arg(&args, "threadId")?;
            ok(messages::read_thread_messages(thread_id, &state()?).await?)
        }
        "list_thread_summaries" => {
            let conversation_id: String = arg(&args, "conversationId")?;
            ok(messages::list_thread_summaries(conversation_id, &state()?).await?)
        }
        // ----- read receipts -----
        "get_conversation_receipts" => {
            let conversation_id: String = arg(&args, "conversationId")?;
            ok(messages::get_conversation_receipts(conversation_id, &state()?).await?)
        }
        "mark_messages_read" => {
            let conversation_id: String = arg(&args, "conversationId")?;
            let user_id: String = arg(&args, "userId")?;
            let message_ids: Vec<String> = arg(&args, "messageIds")?;
            messages::mark_messages_read(conversation_id, user_id, message_ids, &state()?)
                .await?;
            ok(())
        }
        "ingest_channel_envelopes" => {
            let user_id: String = arg(&args, "userId")?;
            let channel_id: String = arg(&args, "channelId")?;
            messages::ingest_channel_envelopes(user_id, channel_id, &state()?).await?;
            ok(())
        }
        "add_reaction" => {
            let message_id: String = arg(&args, "messageId")?;
            let user_id: String = arg(&args, "userId")?;
            let emoji: String = arg(&args, "emoji")?;
            messages::add_reaction(message_id, user_id, emoji, &state()?).await?;
            ok(())
        }
        "remove_reaction" => {
            let message_id: String = arg(&args, "messageId")?;
            let user_id: String = arg(&args, "userId")?;
            let emoji: String = arg(&args, "emoji")?;
            messages::remove_reaction(message_id, user_id, emoji, &state()?).await?;
            ok(())
        }
        "get_reactions" => {
            let message_id: String = arg(&args, "messageId")?;
            ok(messages::get_reactions(message_id, &state()?).await?)
        }
        "edit_message" => {
            let conversation_id: String = arg(&args, "conversationId")?;
            let message_id: String = arg(&args, "messageId")?;
            let user_id: String = arg(&args, "userId")?;
            let new_content: String = arg(&args, "newContent")?;
            messages::edit_message(
                conversation_id,
                message_id,
                user_id,
                new_content,
                &state()?,
            )
            .await?;
            ok(())
        }
        "delete_message" => {
            let message_id: String = arg(&args, "messageId")?;
            let user_id: String = arg(&args, "userId")?;
            messages::delete_message(message_id, user_id, &state()?).await?;
            ok(())
        }
        "ingest_dm_envelopes" => {
            let user_id: String = arg(&args, "userId")?;
            let dm_channel_id: String = arg(&args, "dmChannelId")?;
            messages::ingest_dm_envelopes(user_id, dm_channel_id, &state()?).await?;
            ok(())
        }

        // ----- saved messages + permalinks (#854) -----
        // Device-local, exactly as on desktop: no DS endpoint backs any of
        // these, and resolve_message_permalink only ever consults the local DB.
        "list_saved_messages" => ok(bookmarks::list_saved_messages(&state()?).await?),
        "save_message" => {
            let message_id: String = arg(&args, "messageId")?;
            bookmarks::save_message(message_id, &state()?).await?;
            ok(())
        }
        "toggle_saved_message" => {
            let message_id: String = arg(&args, "messageId")?;
            ok(bookmarks::toggle_saved_message(message_id, &state()?).await?)
        }
        "unsave_message" => {
            let message_id: String = arg(&args, "messageId")?;
            ok(bookmarks::unsave_message(message_id, &state()?).await?)
        }
        "resolve_message_permalink" => {
            let conversation_id: String = arg(&args, "conversationId")?;
            let message_id: String = arg(&args, "messageId")?;
            ok(bookmarks::resolve_message_permalink(conversation_id, message_id, &state()?)
                .await?)
        }

        // ----- pinned messages (#99) -----
        "pin_message" => {
            let conversation_id: String = arg(&args, "conversationId")?;
            let message_id: String = arg(&args, "messageId")?;
            let user_id: String = arg(&args, "userId")?;
            pinned_messages::pin_message(conversation_id, message_id, user_id, &state()?).await?;
            ok(())
        }
        "unpin_message" => {
            let conversation_id: String = arg(&args, "conversationId")?;
            let message_id: String = arg(&args, "messageId")?;
            let user_id: String = arg(&args, "userId")?;
            pinned_messages::unpin_message(conversation_id, message_id, user_id, &state()?)
                .await?;
            ok(())
        }
        "list_pinned_messages" => {
            let conversation_id: String = arg(&args, "conversationId")?;
            let user_id: String = arg(&args, "userId")?;
            ok(pinned_messages::list_pinned_messages(conversation_id, user_id, &state()?).await?)
        }

        // ----- vault (#107) -----
        "get_vault_messages" => {
            let user_id: String = arg(&args, "userId")?;
            ok(vault::get_vault_messages(user_id, &state()?).await?)
        }
        "send_vault_message" => {
            let user_id: String = arg(&args, "userId")?;
            let content: String = arg(&args, "content")?;
            ok(vault::send_vault_message(user_id, content, &state()?).await?)
        }
        "edit_vault_message" => {
            let id: String = arg(&args, "id")?;
            let new_content: String = arg(&args, "newContent")?;
            let user_id: String = arg(&args, "userId")?;
            ok(vault::edit_vault_message(id, new_content, user_id, &state()?).await?)
        }
        "set_vault_message_pinned" => {
            let id: String = arg(&args, "id")?;
            let pinned: bool = arg(&args, "pinned")?;
            let user_id: String = arg(&args, "userId")?;
            ok(vault::set_vault_message_pinned(id, pinned, user_id, &state()?).await?)
        }
        "delete_vault_message" => {
            let id: String = arg(&args, "id")?;
            let user_id: String = arg(&args, "userId")?;
            vault::delete_vault_message(id, user_id, &state()?).await?;
            ok(())
        }
        "search_vault_messages" => {
            let query: String = arg(&args, "query")?;
            let user_id: String = arg(&args, "userId")?;
            ok(vault::search_vault_messages(query, user_id, &state()?).await?)
        }

        // ----- dm -----
        "list_dm_channels" => {
            let user_id: String = arg(&args, "userId")?;
            ok(dm::list_dm_channels(user_id, &state()?).await?)
        }
        // ----- search -----
        "search_messages" => {
            let q: String = arg(&args, "query")?;
            let conversation_id: Option<String> = arg_opt(&args, "conversationId")?;
            let sort: Option<messages::SearchSort> = arg_opt(&args, "sort")?;
            let limit: Option<i64> = arg_opt(&args, "limit")?;
            let cursor: Option<messages::SearchCursor> = arg_opt(&args, "cursor")?;
            ok(messages::search_messages(q, conversation_id, sort, limit, cursor, &state()?).await?)
        }
        "rebuild_search_index" => {
            messages::rebuild_search_index(&state()?).await?;
            ok(())
        }
        "read_messages_around" => {
            let conversation_id: String = arg(&args, "conversationId")?;
            let message_id: String = arg(&args, "messageId")?;
            let limit: Option<i64> = arg_opt(&args, "limit")?;
            ok(messages::read_messages_around(conversation_id, message_id, limit, &state()?).await?)
        }
        "read_messages_after" => {
            let conversation_id: String = arg(&args, "conversationId")?;
            let cursor: messages::MessageCursor = arg(&args, "cursor")?;
            let limit: Option<i64> = arg_opt(&args, "limit")?;
            ok(messages::read_messages_after(conversation_id, cursor, limit, &state()?).await?)
        }

        // ----- blocks -----
        "block_user" => {
            let blocker_id: String = arg(&args, "blockerId")?;
            let blocked_id: String = arg(&args, "blockedId")?;
            blocks::block_user(blocker_id, blocked_id, &state()?).await?;
            ok(())
        }
        "unblock_user" => {
            let blocker_id: String = arg(&args, "blockerId")?;
            let blocked_id: String = arg(&args, "blockedId")?;
            blocks::unblock_user(blocker_id, blocked_id, &state()?).await?;
            ok(())
        }
        "list_blocked_users" => {
            let user_id: String = arg(&args, "userId")?;
            ok(blocks::list_blocked_users(user_id, &state()?).await?)
        }

        // ----- safety -----
        "get_safety_number" => {
            let my_user_id: String = arg(&args, "myUserId")?;
            let peer_user_id: String = arg(&args, "peerUserId")?;
            ok(safety::get_safety_number(my_user_id, peer_user_id, &state()?).await?)
        }
        "set_contact_verified" => {
            let peer_user_id: String = arg(&args, "peerUserId")?;
            let verified: bool = arg(&args, "verified")?;
            safety::set_contact_verified(peer_user_id, verified, &state()?).await?;
            ok(())
        }
        "list_peer_verifications" => ok(safety::list_peer_verifications(&state()?).await?),

        "list_dm_requests" => {
            let user_id: String = arg(&args, "userId")?;
            ok(dm::list_dm_requests(user_id, &state()?).await?)
        }
        "get_dm_channel" => {
            let dm_channel_id: String = arg(&args, "dmChannelId")?;
            ok(dm::get_dm_channel(dm_channel_id, &state()?).await?)
        }
        "leave_dm_channel" => {
            let dm_channel_id: String = arg(&args, "dmChannelId")?;
            let user_id: String = arg(&args, "userId")?;
            dm::leave_dm_channel(dm_channel_id, user_id, &state()?).await?;
            ok(())
        }
        "add_user_to_dm_channel" => {
            let dm_channel_id: String = arg(&args, "dmChannelId")?;
            let user_id: String = arg(&args, "userId")?;
            let added_by: String = arg(&args, "addedBy")?;
            dm::add_user_to_dm_channel(dm_channel_id, user_id, added_by, &state()?).await?;
            ok(())
        }
        "remove_user_from_dm_channel" => {
            let dm_channel_id: String = arg(&args, "dmChannelId")?;
            let user_id: String = arg(&args, "userId")?;
            let requester_id: String = arg(&args, "requesterId")?;
            dm::remove_user_from_dm_channel(
                dm_channel_id,
                user_id,
                requester_id,
                &state()?,
            )
            .await?;
            ok(())
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

        // ----- push notifications -----
        // Mobile registers its Expo push token so senders can wake a
        // backgrounded/closed app with a content-free notification. The send
        // side lives in send_message's background fanout (commands::push).
        "register_push_token" => {
            let user_id: String = arg(&args, "userId")?;
            let token: String = arg(&args, "token")?;
            let platform: String = arg(&args, "platform")?;
            crate::commands::push::register_push_token(user_id, token, platform, &state()?).await?;
            ok(())
        }

        // ----- livekit (realtime token) -----
        // Mobile joins the same SFU rooms as desktop via the JS LiveKit SDK
        // (data-only, see mobile/lib/realtime/). It only passes the room name;
        // the DS mints the token, deriving identity server-side from this
        // device's verified signature — matching desktop's `connect_rooms`
        // scheme (`{user_id}:{device_id}`), so multiple devices coexist. The
        // LiveKit API secret is no longer on the client (#393).
        // Returns `{token, url}`, not a bare token. Mobile used to pair the
        // token with a build-time `EXPO_PUBLIC_LIVEKIT_URL`, which pinned the SFU
        // address into the app bundle exactly as the desktop config did. The URL
        // the DS minted the token for is authoritative, so it ships with it.
        "get_livekit_token" => {
            let room: String = arg(&args, "room")?;
            let st = state()?;
            let (token, url) =
                crate::commands::mls::ds_livekit_token(&st, &room, "realtime").await?;
            ok(serde_json::json!({ "token": token, "url": url }))
        }

        // ----- custom emoji (#890, #940) -----
        "list_usable_emoji" => {
            let user_id: String = arg(&args, "userId")?;
            ok(emoji::list_usable_emoji(user_id, &state()?).await?)
        }
        "list_group_emoji" => {
            let group_id: String = arg(&args, "groupId")?;
            let requester_id: String = arg(&args, "requesterId")?;
            ok(emoji::list_group_emoji(group_id, requester_id, &state()?).await?)
        }
        "upload_group_emoji" => {
            let group_id: String = arg(&args, "groupId")?;
            let shortcode: String = arg(&args, "shortcode")?;
            let path: String = arg(&args, "path")?;
            // Expo hands out `file://` URIs; the Rust side reads a bare path
            // (same strip as `get_media_path`'s destDir).
            let path = path.strip_prefix("file://").unwrap_or(&path).to_string();
            ok(emoji::upload_group_emoji(group_id, shortcode, path, &state()?).await?)
        }
        "remove_group_emoji" => {
            let group_id: String = arg(&args, "groupId")?;
            let shortcode: String = arg(&args, "shortcode")?;
            emoji::remove_group_emoji(group_id, shortcode, &state()?).await?;
            ok(())
        }
        "prepare_emoji_text" => {
            let user_id: String = arg(&args, "userId")?;
            let text: String = arg(&args, "text")?;
            ok(emoji::prepare_emoji_text(user_id, text, &state()?).await?)
        }
        // Mobile counterpart of desktop's `get_emoji_url`: no loopback media
        // server exists inside a sandboxed RN app, so instead of an
        // http://127.0.0.1 URL this materialises the hash-verified emoji bytes
        // to a file in the app's sandbox and hands back a `file://` path that
        // expo-image can load — the same shape as `get_media_path` below, and
        // the JS side manages the file's lifetime the same way.
        "get_emoji_path" => {
            let content_hash: String = arg(&args, "contentHash")?;
            let dest_dir: String = arg(&args, "destDir")?;
            let dir = dest_dir.strip_prefix("file://").unwrap_or(&dest_dir);
            let (bytes, _content_type) =
                emoji::download_verified_emoji(&content_hash, &state()?).await?;
            crate::private_fs::create_dir_all(std::path::Path::new(dir))
                .map_err(|e| BridgeError::Bridge(format!("create emoji dir: {e}")))?;
            let path = std::path::Path::new(dir).join(&content_hash);
            crate::private_fs::write_async(&path, bytes)
                .await
                .map_err(|e| BridgeError::Bridge(format!("write emoji file: {e}")))?;
            ok(format!("file://{}", path.display()))
        }

        // ----- media -----
        // The attachment send path: reads the file from the sandbox path (no
        // bytes over the JSON bridge), convergent-encrypts, uploads via a
        // DS-minted presign, and returns the metadata the JS side folds into
        // the message's `_att` envelope — identical to desktop.
        "upload_media" => {
            let path: String = arg(&args, "path")?;
            let filename: String = arg(&args, "filename")?;
            let content_type: String = arg(&args, "contentType")?;
            // Expo pickers hand out `file://` URIs; strip to a bare path.
            let path = path.strip_prefix("file://").unwrap_or(&path).to_string();
            ok(crate::commands::r2::upload_media(path, filename, content_type, &state()?)
                .await?)
        }
        // Mobile can't run the desktop's loopback media server inside a
        // sandboxed RN app, so instead of returning an http://127.0.0.1 URL
        // we decrypt the R2 object straight to a file in the app's sandbox
        // cache dir and hand back a `file://` path that expo-image can load.
        // Reuses `download_media` (R2 fetch + AES-GCM decrypt) verbatim; the
        // only mobile-specific bit is materialising plaintext to `destDir`.
        // The JS side (mobile/lib/media/cache.ts) ref-counts and unlinks the
        // file on last release, so plaintext never lingers past use.
        "get_media_path" => {
            let r2_key: String = arg(&args, "r2Key")?;
            let content_hash: String = arg(&args, "contentHash")?;
            let dest_dir: String = arg(&args, "destDir")?;
            // expo's cacheDirectory comes through as a `file://` URI; Rust's
            // PathBuf needs a bare path (same strip as init's data_dir).
            let dir = dest_dir.strip_prefix("file://").unwrap_or(&dest_dir);
            let bytes =
                crate::commands::r2::download_media(r2_key, content_hash.clone(), &state()?)
                    .await?;
            crate::private_fs::create_dir_all(std::path::Path::new(dir))
                .map_err(|e| BridgeError::Bridge(format!("create media dir: {e}")))?;
            let path = std::path::Path::new(dir).join(&content_hash);
            crate::private_fs::write_async(&path, bytes)
                .await
                .map_err(|e| BridgeError::Bridge(format!("write media file: {e}")))?;
            ok(format!("file://{}", path.display()))
        }

        _ => Err(BridgeError::Bridge(format!(
            "unknown command: {cmd} (add it to pollis-core/src/bridge.rs)"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Distinguishes "the dispatcher has an arm for this name" from "it fell
    /// through to the unknown-command default". These tests never call
    /// `init_pollis`, so a registered arm fails with the not-initialized
    /// error (or an arg error for arms that parse args first) — anything but
    /// the unknown-command message — which pins arm registration without
    /// needing a live AppState.
    async fn is_registered(cmd: &str) -> bool {
        match invoke(cmd.to_string(), "{}".to_string()).await {
            Ok(_) => true,
            Err(BridgeError::Bridge(msg)) => !msg.starts_with("unknown command:"),
        }
    }

    #[tokio::test]
    async fn new_parity_arms_are_registered() {
        for cmd in [
            "create_group_invite_link",
            "list_group_invite_links",
            "revoke_group_invite_link",
            "redeem_group_invite_link",
            "list_saved_messages",
            "save_message",
            "toggle_saved_message",
            "unsave_message",
            "resolve_message_permalink",
            "list_usable_emoji",
            "list_group_emoji",
            "upload_group_emoji",
            "remove_group_emoji",
            "get_emoji_path",
            "prepare_emoji_text",
            "get_conversation_receipts",
            "mark_messages_read",
            "read_thread_messages",
            "list_thread_summaries",
            "read_channel_messages",
            "read_dm_messages",
            "read_last_messages",
            "list_security_events",
            "delete_account",
            "wipe_local_data",
            "upload_media",
            "pin_message",
            "unpin_message",
            "list_pinned_messages",
            "get_vault_messages",
            "send_vault_message",
            "edit_vault_message",
            "set_vault_message_pinned",
            "delete_vault_message",
            "search_vault_messages",
        ] {
            assert!(is_registered(cmd).await, "no bridge arm for {cmd}");
        }
    }

    #[tokio::test]
    async fn unknown_command_still_falls_through() {
        assert!(!is_registered("definitely_not_a_command").await);
    }
}
