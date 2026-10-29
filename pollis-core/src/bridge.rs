//! Generic uniffi-exposed command dispatcher.
//!
//! Lets a non-Tauri host (`pollis-native` on mobile) drive the same command
//! surface desktop reaches via Tauri's `invoke()`: `init_pollis(config_json)`
//! once, then `invoke("cmd_name", args_json)`. Adding a command means adding a
//! `match` arm that reads args with `arg` / `arg_opt` and returns via `ok(...)`.

use serde_json::Value;
use std::sync::{Arc, OnceLock};
use tokio::sync::OnceCell;

use crate::config::Config;
use crate::state::AppState;

static APP_STATE: OnceCell<Arc<AppState>> = OnceCell::const_new();

/// A multi-threaded Tokio runtime with large worker stacks that runs all
/// bridge command work.
///
/// uniffi-bindgen-react-native polls exported futures directly on the JS
/// thread, whose ~1 MB stack (less on Android) the SQLCipher parser and MLS
/// operations overflow (SIGBUS at the guard page). Only the join handle is
/// awaited on the JS thread.
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

/// Run a command future on the big-stack [`worker_runtime`]; the closure builds
/// the future on the worker so nothing stack-hungry runs on the JS thread.
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
/// Unknown keys are ignored, so a mobile build still sending the pre-#987
/// `EXPO_PUBLIC_TURSO_*` values starts cleanly.
#[derive(serde::Deserialize)]
struct InitConfig {
    /// Writable per-app directory. Required on Android (CA bundle and
    /// `POLLIS_DATA_DIR` live there); elsewhere `db::dirs_path()` defaults it.
    #[serde(default)]
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    data_dir: Option<String>,
    #[serde(default)]
    r2_endpoint: String,
    #[serde(default)]
    r2_public_url: String,
    #[serde(default)]
    livekit_url: String,
    /// Delivery Service base URL.
    #[serde(default)]
    pollis_delivery_url: Option<String>,
    /// The dev DS gate key (#1242). Dev builds only; never sent to prod.
    #[serde(default)]
    dev_access_key: Option<String>,
}

/// Initialize the process-global `AppState`; only the first call does work.
/// `config_json` mirrors [`Config`]. `r2_endpoint` and `r2_public_url` are
/// required, since a presigned media URL may point only at them.
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
            #[cfg(target_os = "ios")]
            ios_bootstrap()?;
            // A diagnostics-log failure leaves logging on stderr only (#1305).
            let log_dir = crate::diag::log_dir(&crate::db::local::dirs_path());
            if let Err(e) = crate::diag::init(&log_dir) {
                crate::diag!("[diag] could not open the diagnostics log: {e}");
            }
            // Refuse a build without both R2 origins by name, rather than
            // render every attachment as unavailable.
            if let Some(problem) =
                crate::commands::r2::r2_config_problem(&parsed.r2_endpoint, &parsed.r2_public_url)
            {
                crate::diag!("[init] {problem}");
                return Err(BridgeError::Bridge(problem));
            }
            let config = Config {
                r2_endpoint: parsed.r2_endpoint,
                r2_public_url: parsed.r2_public_url,
                livekit_url: parsed.livekit_url,
                pollis_delivery_url: parsed.pollis_delivery_url.filter(|s| !s.is_empty()),
                // The overlay is desktop-only for now.
                overlay_mode: pollis_relay::OverlayMode::Off,
                overlay_relay_url: None,
                overlay_relay_cert: None,
                overlay_directory_url: None,
                overlay_directory_key: None,
                dev_access_key: parsed.dev_access_key.filter(|s| !s.trim().is_empty()),
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

/// Create the data directory and exclude it from backup before anything writes
/// there. Failing hard beats starting on a directory iCloud would copy.
#[cfg(target_os = "ios")]
fn ios_bootstrap() -> Result<(), BridgeError> {
    let dir = crate::db::local::dirs_path();
    crate::ios_storage::prepare(&dir)
        .map_err(|e| BridgeError::Bridge(format!("ios data dir at {}: {e}", dir.display())))
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

/// Dispatch a command by name. `args_json` must be a JSON object (`"{}"` or
/// `""` for nullary commands). Returns the result as a JSON string, which
/// `mobile/lib/native/bridge.ts` parses.
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
        "get_diagnostics" => {
            let app_version: Option<String> = arg_opt(&args, "appVersion")?;
            ok(crate::commands::diagnostics::get_diagnostics(app_version))
        }

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
        // #1571: mobile's half of desktop's sweep backstop — re-add a device
        // missing from a conversation's tree (or evict a stale leaf). Run after
        // the app's landing / resume ingest pass.
        "reconcile_backstop_all" => {
            let user_id: String = arg(&args, "userId")?;
            ok(crate::commands::mls::reconcile_backstop_all(&state()?, &user_id).await?)
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
            let current_code: String = arg(&args, "currentCode")?;
            auth::verify_email_change(&state()?, user_id, new_email, code, current_code).await?;
            ok(())
        }
        "start_device_enrollment" => {
            let user_id: String = arg(&args, "userId")?;
            ok(device_enrollment::start_device_enrollment(&state()?, user_id).await?)
        }
        "poll_enrollment_status" => {
            let request_id: String = arg(&args, "requestId")?;
            ok(device_enrollment::poll_enrollment_status(&state()?, request_id).await?)
        }
        // QR device link (#1207).
        "create_device_link" => {
            let user_id: String = arg(&args, "userId")?;
            let pin: String = arg(&args, "pin")?;
            ok(crate::commands::device_link::create_device_link(&state()?, user_id, pin).await?)
        }
        "poll_device_link" => {
            let user_id: String = arg(&args, "userId")?;
            let link_id: String = arg(&args, "linkId")?;
            ok(crate::commands::device_link::poll_device_link(&state()?, user_id, link_id).await?)
        }
        "await_device_link" => {
            let user_id: String = arg(&args, "userId")?;
            let link_id: String = arg(&args, "linkId")?;
            let since: String = arg(&args, "since")?;
            ok(crate::commands::device_link::await_device_link(&state()?, user_id, link_id, since).await?)
        }
        "approve_device_link" => {
            let user_id: String = arg(&args, "userId")?;
            let link_id: String = arg(&args, "linkId")?;
            crate::commands::device_link::approve_device_link(&state()?, user_id, link_id).await?;
            ok(())
        }
        "cancel_device_link" => {
            let link_id: String = arg(&args, "linkId")?;
            crate::commands::device_link::cancel_device_link(&state()?, link_id).await?;
            ok(())
        }
        // QR sign-in shown by the NEW device (#1361).
        "create_device_offer" => {
            let device_name: Option<String> = arg_opt(&args, "deviceName")?;
            ok(crate::commands::device_link::create_device_offer(&state()?, device_name).await?)
        }
        "await_device_offer" => {
            let link_id: String = arg(&args, "linkId")?;
            ok(crate::commands::device_link::await_device_offer(&state()?, link_id).await?)
        }
        "cancel_device_offer" => {
            crate::commands::device_link::cancel_device_offer(&state()?).await?;
            ok(())
        }
        "accept_device_offer" => {
            let user_id: String = arg(&args, "userId")?;
            let pin: String = arg(&args, "pin")?;
            let payload: String = arg(&args, "payload")?;
            ok(crate::commands::device_link::accept_device_offer(&state()?, user_id, pin, payload).await?)
        }
        "finish_device_offer" => {
            let user_id: String = arg(&args, "userId")?;
            let link_id: String = arg(&args, "linkId")?;
            ok(crate::commands::device_link::finish_device_offer(&state()?, user_id, link_id).await?)
        }
        "claim_device_link" => {
            let payload: String = arg(&args, "payload")?;
            let device_name: Option<String> = arg_opt(&args, "deviceName")?;
            ok(crate::commands::device_link::claim_device_link(&state()?, payload, device_name).await?)
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
        // Spoof-resistant check that THIS device is still registered, before
        // self-sign-out on a `device_revoked` nudge.
        "is_current_device_registered" => {
            let user_id: String = arg(&args, "userId")?;
            ok(auth::is_current_device_registered(&state()?, user_id).await?)
        }
        // #1338: whether THIS device is the one the DS answered `device_revoked`
        // for. The app asks before acting on a `device_revoked` error.
        "is_current_device_revoked" => ok(auth::is_current_device_revoked(&state()?).await?),
        "list_security_events" => {
            let user_id: String = arg(&args, "userId")?;
            let limit: Option<i64> = arg_opt(&args, "limit")?;
            let before: Option<device_enrollment::SecurityEventCursor> =
                arg_opt(&args, "before")?;
            let kinds: Option<Vec<String>> = arg_opt(&args, "kinds")?;
            ok(device_enrollment::list_security_events(&state()?, user_id, limit, before, kinds)
                .await?)
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
            // A JSON-encoded string; the TS hook unwraps the outer encoding.
            ok(user::get_preferences(user_id, &state()?).await?)
        }
        "save_preferences" => {
            let user_id: String = arg(&args, "userId")?;
            let preferences_json: String = arg(&args, "preferencesJson")?;
            user::save_preferences(user_id, preferences_json, &state()?).await?;
            ok(())
        }

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
        // Per-channel mute (#1431), stored on the DS for the whole account.
        "list_muted_channels" => {
            let user_id: String = arg(&args, "userId")?;
            ok(groups::list_muted_channels(user_id, &state()?).await?)
        }
        "set_channel_muted" => {
            let user_id: String = arg(&args, "userId")?;
            let channel_id: String = arg(&args, "channelId")?;
            let muted: bool = arg(&args, "muted")?;
            groups::set_channel_muted(user_id, channel_id, muted, &state()?).await?;
            ok(())
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
        "list_group_invites" => {
            let group_id: String = arg(&args, "groupId")?;
            let user_id: String = arg(&args, "userId")?;
            ok(groups::list_group_invites(group_id, user_id, &state()?).await?)
        }
        "revoke_group_invite" => {
            let invite_id: String = arg(&args, "inviteId")?;
            let group_id: String = arg(&args, "groupId")?;
            let user_id: String = arg(&args, "userId")?;
            groups::revoke_group_invite(invite_id, group_id, user_id, &state()?).await?;
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
            // Optional (#1335): absent → the core mints one.
            let message_id: Option<String> = arg_opt(&args, "messageId")?;
            // Expo pickers hand out `file://` URIs; the core reads bare paths.
            let attachments: Vec<messages::OutgoingAttachment> =
                arg_opt::<Vec<messages::OutgoingAttachment>>(&args, "attachments")?
                    .unwrap_or_default()
                    .into_iter()
                    .map(|mut a| {
                        a.path = a
                            .path
                            .map(|p| p.strip_prefix("file://").unwrap_or(&p).to_string());
                        a
                    })
                    .collect();
            ok(messages::send_message_with_id(
                message_id,
                conversation_id,
                sender_id,
                content,
                attachments,
                reply_to_id,
                thread_id,
                sender_username,
                &state()?,
            )
            .await?)
        }
        // Durable outbox (#1335).
        "new_message_id" => ok(messages::new_message_id()),
        "retry_message" => {
            let message_id: String = arg(&args, "messageId")?;
            ok(messages::retry_message(message_id, &state()?).await?)
        }
        "discard_unsent_message" => {
            let message_id: String = arg(&args, "messageId")?;
            ok(messages::discard_unsent_message(message_id, &state()?).await?)
        }
        "flush_outbox" => ok(messages::flush_outbox(&state()?).await?),
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
        // Local-first reads (#936); the caller fires ingest separately.
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
        // Persisted read cursors (#844, #1422), so backgrounded arrivals still badge.
        "get_unread_counts" => {
            let user_id: String = arg(&args, "userId")?;
            ok(messages::get_unread_counts(user_id, &state()?).await?)
        }
        "mark_conversation_read" => {
            let conversation_id: String = arg(&args, "conversationId")?;
            let user_id: String = arg(&args, "userId")?;
            ok(messages::mark_conversation_read(conversation_id, user_id, &state()?).await?)
        }
        "sync_read_cursors" => {
            let user_id: String = arg(&args, "userId")?;
            ok(messages::sync_read_cursors(user_id, &state()?).await?)
        }
        "read_thread_messages" => {
            let thread_id: String = arg(&args, "threadId")?;
            ok(messages::read_thread_messages(thread_id, &state()?).await?)
        }
        "list_thread_summaries" => {
            let conversation_id: String = arg(&args, "conversationId")?;
            ok(messages::list_thread_summaries(conversation_id, &state()?).await?)
        }
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

        // Saved messages + permalinks (#854): device-local, no DS endpoint.
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

        // On-device export (#856). A sandboxed app has no "save as", so the
        // archive is written under the cache dir and zipped for the share sheet.
        "export_archive" => {
            let path: String = arg(&args, "path")?;
            let conversation_id: Option<String> = arg_opt(&args, "conversationId")?;
            let path = path.strip_prefix("file://").unwrap_or(&path).to_string();
            ok(crate::commands::export::export_archive(path, conversation_id, &state()?).await?)
        }
        "fetch_export_attachments" => {
            let files_dir: String = arg(&args, "filesDir")?;
            let attachments: Vec<crate::commands::export::MissingAttachment> =
                arg(&args, "attachments")?;
            let files_dir = files_dir.strip_prefix("file://").unwrap_or(&files_dir).to_string();
            ok(crate::commands::export_fetch::fetch_export_attachments(
                files_dir,
                attachments,
                &state()?,
            )
            .await?)
        }
        "bundle_export" => {
            let path: String = arg(&args, "path")?;
            let path = path.strip_prefix("file://").unwrap_or(&path);
            let zip = crate::commands::export::bundle_archive(std::path::Path::new(path))?;
            ok(format!("file://{}", zip.display()))
        }

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

        "list_dm_channels" => {
            let user_id: String = arg(&args, "userId")?;
            ok(dm::list_dm_channels(user_id, &state()?).await?)
        }
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

        "block_user" => {
            let blocker_id: String = arg(&args, "blockerId")?;
            let blocked_id: String = arg(&args, "blockedId")?;
            blocks::block_user(blocker_id, blocked_id, &state()?).await?;
            ok(())
        }
        "report_user" => {
            let reported_id: String = arg(&args, "reportedId")?;
            let reason: String = arg(&args, "reason")?;
            let conversation_id: Option<String> = arg_opt(&args, "conversationId")?;
            let message_id: Option<String> = arg_opt(&args, "messageId")?;
            let also_block: bool = arg_opt(&args, "alsoBlock")?.unwrap_or(false);
            blocks::report_user(reported_id, reason, conversation_id, message_id, also_block, &state()?).await?;
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

        // Senders wake a backgrounded app with a content-free push (`commands::push`).
        "register_push_token" => {
            let user_id: String = arg(&args, "userId")?;
            let token: String = arg(&args, "token")?;
            let platform: String = arg(&args, "platform")?;
            crate::commands::push::register_push_token(user_id, token, platform, &state()?).await?;
            ok(())
        }

        // #1122: pushes carry an opaque handle, not the conversation id. Null
        // when unknown, expired or not ours; the caller just opens the app.
        "resolve_push_handle" => {
            let handle: String = arg(&args, "handle")?;
            let resolved = crate::commands::push::resolve_push_handle(handle, &state()?).await?;
            ok(resolved.map(|(conversation_id, kind)| {
                serde_json::json!({ "conversationId": conversation_id, "kind": kind })
            }))
        }

        // #1508: tray notifications read since on any device, asked on
        // foreground so a missed dismissal push still clears them.
        "dismissed_push_handles" => {
            let user_id: String = arg(&args, "userId")?;
            let handles: Vec<String> = arg(&args, "handles")?;
            ok(crate::commands::push::dismissed_push_handles(user_id, handles, &state()?).await?)
        }

        // The DS mints the token from this device's verified signature (#393)
        // and returns the SFU URL with it, which is authoritative.
        "get_livekit_token" => {
            let room: String = arg(&args, "room")?;
            let st = state()?;
            let (token, url) =
                crate::commands::mls::ds_livekit_token(&st, &room, "realtime").await?;
            ok(serde_json::json!({ "token": token, "url": url }))
        }

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
            // Expo hands out `file://` URIs; the Rust side reads a bare path.
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
        // Mobile's `get_emoji_url`: there is no loopback server in a sandboxed
        // RN app, so the verified bytes go to a sandbox file and a `file://`
        // path comes back, like `get_media_path`.
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

        // The Vault's upload path (chat attachments use the outbox): the file is
        // read from its sandbox path, sealed under the Vault file key (#1382) and
        // uploaded via a DS-minted presign.
        "upload_media" => {
            let path: String = arg(&args, "path")?;
            let filename: String = arg(&args, "filename")?;
            let content_type: String = arg(&args, "contentType")?;
            // Expo pickers hand out `file://` URIs; strip to a bare path.
            let path = path.strip_prefix("file://").unwrap_or(&path).to_string();
            ok(crate::commands::r2::upload_media(path, filename, content_type, &state()?)
                .await?)
        }
        // No loopback server on mobile: decrypt straight to a sandbox cache file
        // and return a `file://` path. `mobile/lib/media/cache.ts` ref-counts
        // and unlinks it on last release.
        "get_media_path" => {
            let r2_key: String = arg(&args, "r2Key")?;
            let content_hash: String = arg(&args, "contentHash")?;
            let sha256: String = arg(&args, "sha256")?;
            let key_scope: crate::commands::r2::AttachmentKeyScope = arg(&args, "keyScope")?;
            let dest_dir: String = arg(&args, "destDir")?;
            let dir = dest_dir.strip_prefix("file://").unwrap_or(&dest_dir);
            // The app shows only "Image unavailable", so the reason goes to the
            // diagnostics log.
            let bytes =
                match crate::commands::r2::download_media(
                    r2_key,
                    content_hash.clone(),
                    sha256,
                    key_scope,
                    &state()?,
                )
                .await
                {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        crate::diag!("[media] get_media_path {content_hash} failed: {e}");
                        return Err(e.into());
                    }
                };
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

    /// True unless the call fell through to the unknown-command default. The
    /// tests never call `init_pollis`, so a registered arm fails some other way.
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
            "get_unread_counts",
            "mark_conversation_read",
            "sync_read_cursors",
            "list_security_events",
            "delete_account",
            "wipe_local_data",
            "upload_media",
            "new_message_id",
            "retry_message",
            "discard_unsent_message",
            "flush_outbox",
            "pin_message",
            "unpin_message",
            "list_pinned_messages",
            "get_vault_messages",
            "send_vault_message",
            "edit_vault_message",
            "set_vault_message_pinned",
            "delete_vault_message",
            "search_vault_messages",
            "export_archive",
            "fetch_export_attachments",
            "bundle_export",
            "dismissed_push_handles",
        ] {
            assert!(is_registered(cmd).await, "no bridge arm for {cmd}");
        }
    }

    /// The unread arms (#1422) take desktop's camelCase arg names; a missing
    /// one names itself, since args parse before `state()`.
    #[tokio::test]
    async fn unread_arms_take_the_desktop_arg_names() {
        async fn missing(cmd: &str, args: &str) -> String {
            match invoke(cmd.to_string(), args.to_string()).await {
                Ok(_) => panic!("{cmd} should fail without its args"),
                Err(BridgeError::Bridge(msg)) => msg,
            }
        }
        assert_eq!(missing("get_unread_counts", "{}").await, "missing arg: userId");
        assert_eq!(missing("sync_read_cursors", "{}").await, "missing arg: userId");
        assert_eq!(
            missing("mark_conversation_read", "{}").await,
            "missing arg: conversationId"
        );
        assert_eq!(
            missing("mark_conversation_read", r#"{"conversationId":"c1"}"#).await,
            "missing arg: userId"
        );
    }

    #[tokio::test]
    async fn unknown_command_still_falls_through() {
        assert!(!is_registered("definitely_not_a_command").await);
    }
}
