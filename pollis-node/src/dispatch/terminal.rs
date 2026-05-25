// Port of `src-tauri/src/commands/terminal.rs`. PTY output streams via
// `terminal_open`'s `RawNapiSink` (Channel<Buffer>); PTY input arrives via
// `terminal_write`'s binary IPC body — both binary paths to keep the
// typing-latency win from commits 2b877d0 + 850661b intact.

use std::sync::Arc;

use napi::bindgen_prelude::*;

use crate::events::{extract_channel_id, RawNapiSink};
use crate::state::{core_err, ensure_state, json_err};
use pollis_core::sink::RawSink;

pub async fn dispatch(
    cmd: &str,
    args: &serde_json::Value,
) -> Option<Result<serde_json::Value>> {
    match cmd {
        "terminal_open" => Some(terminal_open(args).await),
        // terminal_write goes through the binary `route_raw` path in
        // dispatch/mod.rs — its body arrives as `Buffer` (zero-copy from
        // the JS Uint8Array), terminal_id rides in the `x-terminal-id`
        // header. If a caller mis-routes it through invoke() (JSON path)
        // this arm errors with a clear message instead of silently
        // base64-decoding and re-encoding.
        "terminal_write" => Some(Err(Error::from_reason(
            "terminal_write: send via invoke_raw (Uint8Array args), not invoke (JSON args)"
                .to_string(),
        ))),
        "terminal_resize" => Some(terminal_resize(args).await),
        "terminal_close" => Some(terminal_close(args).await),
        "terminal_ack" => Some(terminal_ack(args).await),
        _ => None,
    }
}

/// Binary keystroke path. Called from `dispatch::route_raw` when the
/// renderer fires `invoke("terminal_write", Uint8Array, { headers: {…} })`.
/// `body` is the byte slice the user typed; `terminal_id` rides in the
/// `x-terminal-id` header so we don't allocate a JSON envelope per keypress.
pub(super) async fn write_raw(
    body: &[u8],
    headers: &serde_json::Value,
) -> Result<serde_json::Value> {
    let terminal_id = headers
        .get("x-terminal-id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            Error::from_reason(
                "terminal_write: missing x-terminal-id header".to_string(),
            )
        })?;
    let state = ensure_state().await?;
    pollis_core::commands::terminal::terminal_write(terminal_id, body, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn terminal_open(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        rows: u16,
        cols: u16,
    }
    let Args { rows, cols } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let channel_id = extract_channel_id(args, "on_output")?;
    let sink: Arc<dyn RawSink> = Arc::new(RawNapiSink::new(channel_id));
    let state = ensure_state().await?;
    let id = pollis_core::commands::terminal::terminal_open(rows, cols, sink, &state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(id).map_err(json_err)
}

async fn terminal_resize(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        terminal_id: String,
        rows: u16,
        cols: u16,
    }
    let Args {
        terminal_id,
        rows,
        cols,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::terminal::terminal_resize(terminal_id, rows, cols, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn terminal_close(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        terminal_id: String,
    }
    let Args { terminal_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::terminal::terminal_close(terminal_id, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn terminal_ack(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        terminal_id: String,
        bytes: usize,
    }
    let Args { terminal_id, bytes } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::terminal::terminal_ack(terminal_id, bytes, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}
