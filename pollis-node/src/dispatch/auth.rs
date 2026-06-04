// Phase 2: port of `src-tauri/src/commands/auth.rs` into the napi dispatch
// pattern. See pollis-node/src/lib.rs for the invoke() entry point. Each arm
// here corresponds to a single tauri::command from the legacy shim.

use napi::bindgen_prelude::*;

use crate::state::{core_err, ensure_state, json_err};

pub async fn dispatch(
    cmd: &str,
    args: &serde_json::Value,
) -> Option<Result<serde_json::Value>> {
    match cmd {
        "initialize_identity" => Some(initialize_identity(args).await),
        "get_identity" => Some(get_identity(args).await),
        "request_otp" => Some(request_otp(args).await),
        "verify_otp" => Some(verify_otp(args).await),
        "request_email_change_otp" => Some(request_email_change_otp(args).await),
        "verify_email_change" => Some(verify_email_change(args).await),
        "dev_login" => Some(dev_login(args).await),
        "get_session" => Some(get_session(args).await),
        "get_device_id" => Some(get_device_id(args).await),
        "logout" => Some(logout(args).await),
        "delete_account" => Some(delete_account(args).await),
        "list_known_accounts" => Some(list_known_accounts(args).await),
        "wipe_local_data" => Some(wipe_local_data(args).await),
        "list_user_devices" => Some(list_user_devices(args).await),
        "revoke_device" => Some(revoke_device(args).await),
        "is_current_device_registered" => Some(is_current_device_registered(args).await),
        _ => None,
    }
}

async fn initialize_identity(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
    }
    let Args { user_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::auth::initialize_identity(&state, user_id)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn get_identity(_args: &serde_json::Value) -> Result<serde_json::Value> {
    let out = pollis_core::commands::auth::get_identity()
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn request_otp(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        email: String,
    }
    let Args { email } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::auth::request_otp(&state, email)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn verify_otp(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        email: String,
        code: String,
    }
    let Args { email, code } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::auth::verify_otp(&state, email, code)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn request_email_change_otp(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
        new_email: String,
    }
    let Args { user_id, new_email } =
        serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::auth::request_email_change_otp(&state, user_id, new_email)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn verify_email_change(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
        new_email: String,
        code: String,
    }
    let Args {
        user_id,
        new_email,
        code,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::auth::verify_email_change(&state, user_id, new_email, code)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn dev_login(args: &serde_json::Value) -> Result<serde_json::Value> {
    // Matches the Tauri shim's `_email: String` arg name.
    #[derive(serde::Deserialize)]
    struct Args {
        #[serde(rename = "_email", alias = "email")]
        email: String,
    }
    let Args { email } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::auth::dev_login(&state, email)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn get_session(_args: &serde_json::Value) -> Result<serde_json::Value> {
    let state = ensure_state().await?;
    let out = pollis_core::commands::auth::get_session(&state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn get_device_id(_args: &serde_json::Value) -> Result<serde_json::Value> {
    let state = ensure_state().await?;
    let out = pollis_core::commands::auth::get_device_id(&state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn logout(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        delete_data: bool,
    }
    let Args { delete_data } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::auth::logout(&state, delete_data)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn delete_account(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
    }
    let Args { user_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::auth::delete_account(&state, user_id)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn list_known_accounts(_args: &serde_json::Value) -> Result<serde_json::Value> {
    let out = pollis_core::commands::auth::list_known_accounts().map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn wipe_local_data(_args: &serde_json::Value) -> Result<serde_json::Value> {
    let state = ensure_state().await?;
    pollis_core::commands::auth::wipe_local_data(&state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn list_user_devices(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
    }
    let Args { user_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::auth::list_user_devices(&state, user_id)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn revoke_device(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
        device_id: String,
    }
    let Args { user_id, device_id } =
        serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::auth::revoke_device(&state, user_id, device_id)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn is_current_device_registered(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
    }
    let Args { user_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::auth::is_current_device_registered(&state, user_id)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}
