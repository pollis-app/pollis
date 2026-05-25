// Phase 2: port of `src-tauri/src/commands/device_enrollment.rs` into the napi dispatch
// pattern. See pollis-node/src/lib.rs for the invoke() entry point. Each arm
// here corresponds to a single tauri::command from the legacy shim.

use napi::bindgen_prelude::*;

use crate::state::{core_err, ensure_state, json_err};

pub async fn dispatch(
    cmd: &str,
    args: &serde_json::Value,
) -> Option<Result<serde_json::Value>> {
    match cmd {
        "start_device_enrollment" => Some(start_device_enrollment(args).await),
        "poll_enrollment_status" => Some(poll_enrollment_status(args).await),
        "list_pending_enrollment_requests" => {
            Some(list_pending_enrollment_requests(args).await)
        }
        "approve_device_enrollment" => Some(approve_device_enrollment(args).await),
        "reject_device_enrollment" => Some(reject_device_enrollment(args).await),
        "recover_with_secret_key" => Some(recover_with_secret_key(args).await),
        "reset_identity_and_recover" => Some(reset_identity_and_recover(args).await),
        "finalize_device_enrollment" => Some(finalize_device_enrollment(args).await),
        "list_security_events" => Some(list_security_events(args).await),
        _ => None,
    }
}

async fn start_device_enrollment(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
    }
    let Args { user_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::device_enrollment::start_device_enrollment(&state, user_id)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn poll_enrollment_status(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        request_id: String,
    }
    let Args { request_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::device_enrollment::poll_enrollment_status(&state, request_id)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn list_pending_enrollment_requests(
    args: &serde_json::Value,
) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
    }
    let Args { user_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::device_enrollment::list_pending_enrollment_requests(
        &state, user_id,
    )
    .await
    .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn approve_device_enrollment(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        request_id: String,
        verification_code: String,
    }
    let Args {
        request_id,
        verification_code,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::device_enrollment::approve_device_enrollment(
        &state,
        request_id,
        verification_code,
    )
    .await
    .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn reject_device_enrollment(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        request_id: String,
    }
    let Args { request_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::device_enrollment::reject_device_enrollment(&state, request_id)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn recover_with_secret_key(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
        secret_key: String,
    }
    let Args { user_id, secret_key } =
        serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::device_enrollment::recover_with_secret_key(
        &state, user_id, secret_key,
    )
    .await
    .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn reset_identity_and_recover(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
        confirm_email: String,
    }
    let Args {
        user_id,
        confirm_email,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::device_enrollment::reset_identity_and_recover(
        &state,
        user_id,
        confirm_email,
    )
    .await
    .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn finalize_device_enrollment(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
    }
    let Args { user_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::device_enrollment::finalize_device_enrollment(&state, user_id)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn list_security_events(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
        limit: Option<i64>,
    }
    let Args { user_id, limit } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::device_enrollment::list_security_events(
        &state, user_id, limit,
    )
    .await
    .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}
