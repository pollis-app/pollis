// Phase 2: port of `src-tauri/src/commands/r2.rs` into the napi dispatch
// pattern. See pollis-node/src/lib.rs for the invoke() entry point. Each arm
// here corresponds to a single tauri::command from the legacy shim.
//
// Phase 2 agent: replace the stub with match arms for every command in
// docs/electron-migration-inventory.md under the `r2` section. Channel-
// based commands stay stubbed (returning a Phase 3 TODO) — they need the
// NapiSink work in Phase 3.

use napi::bindgen_prelude::*;

use crate::state::{core_err, ensure_state, json_err};

pub async fn dispatch(
    cmd: &str,
    args: &serde_json::Value,
) -> Option<Result<serde_json::Value>> {
    match cmd {
        "upload_file" => Some(upload_file(args).await),
        "upload_media" => Some(upload_media(args).await),
        "download_file" => Some(download_file(args).await),
        "download_media" => Some(download_media(args).await),
        "get_media_url" => Some(get_media_url(args).await),
        _ => None,
    }
}

async fn upload_file(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        key: String,
        data: Vec<u8>,
        content_type: String,
    }
    let Args {
        key,
        data,
        content_type,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::r2::upload_file(key, data, content_type, &state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn upload_media(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        path: String,
        filename: String,
        content_type: String,
    }
    let Args {
        path,
        filename,
        content_type,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::r2::upload_media(path, filename, content_type, &state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn download_file(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        key: String,
    }
    let Args { key } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::r2::download_file(key, &state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn download_media(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        r2_key: String,
        content_hash: String,
    }
    let Args {
        r2_key,
        content_hash,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::r2::download_media(r2_key, content_hash, &state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn get_media_url(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        r2_key: String,
        content_hash: String,
        content_type: String,
    }
    let Args {
        r2_key,
        content_hash,
        content_type,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::r2::get_media_url(r2_key, content_hash, content_type, &state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}
