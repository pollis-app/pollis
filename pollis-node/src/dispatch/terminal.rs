// Port of `src-tauri/src/commands/terminal.rs`. Two commands stay stubbed
// pending Phase 3:
//   - `terminal_open` takes a raw-bytes Channel for PTY output (NapiSink)
//   - `terminal_write` consumes a raw IPC body (binary keystroke path), the
//     symmetric input side. Both need binary-args / binary-events plumbing.

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
        "terminal_write" => Some(Err(Error::from_reason(
            "Phase 3: raw-bytes IPC body not yet wired for terminal_write".to_string(),
        ))),
        "terminal_resize" => Some(terminal_resize(args).await),
        "terminal_close" => Some(terminal_close(args).await),
        "terminal_ack" => Some(terminal_ack(args).await),
        _ => None,
    }
}

async fn terminal_open(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        rows: u16,
        cols: u16,
    }
    let Args { rows, cols } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let channel_id = extract_channel_id(args, "onOutput")?;
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
