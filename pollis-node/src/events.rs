// Phase 3: napi-side adapters for pollis-core's `EventSink<T>` and
// `RawSink` traits. Lets backend-pushed events ride a single
// `ThreadsafeFunction` registered once at startup, which the Electron main
// process forwards onto each renderer via `webContents.send("channel:<id>",
// payload)`. The frontend bridge's `ElectronChannel` polyfill is already
// listening on that name.
//
// Two emitters: one for JSON-encoded events, one for raw byte buffers
// (screenshare frames, terminal output). The raw path stays zero-copy via
// napi's `Buffer` type — exactly the perf bargain `RawSink` exists for.

use std::sync::OnceLock;

use napi::bindgen_prelude::*;
use napi::threadsafe_function::{
    ErrorStrategy, ThreadsafeFunction, ThreadsafeFunctionCallMode,
};

use pollis_core::sink::{EventSink, RawSink};

static EMIT_JSON: OnceLock<ThreadsafeFunction<serde_json::Value, ErrorStrategy::Fatal>> =
    OnceLock::new();
static EMIT_RAW: OnceLock<ThreadsafeFunction<JsRawFrame, ErrorStrategy::Fatal>> =
    OnceLock::new();

/// Envelope for raw frame delivery — keeps the `Buffer` zero-copy.
#[napi(object)]
pub struct JsRawFrame {
    pub channel_id: String,
    pub payload: Buffer,
}

/// Register the two callbacks Electron main uses to forward backend events
/// to the renderer. Called once at app startup right after `init()`.
#[napi]
pub fn register_event_emitters(
    json_callback: ThreadsafeFunction<serde_json::Value, ErrorStrategy::Fatal>,
    raw_callback: ThreadsafeFunction<JsRawFrame, ErrorStrategy::Fatal>,
) -> Result<()> {
    EMIT_JSON
        .set(json_callback)
        .map_err(|_| Error::from_reason("event emitters already registered"))?;
    EMIT_RAW
        .set(raw_callback)
        .map_err(|_| Error::from_reason("event emitters already registered"))?;
    Ok(())
}

/// Pull `__CHANNEL__:<id>` out of a string-valued arg field. Matches the
/// shape the frontend bridge's `ElectronChannel.toJSON()` produces.
pub fn extract_channel_id(args: &serde_json::Value, field: &str) -> Result<String> {
    let raw = args
        .get(field)
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::from_reason(format!("missing channel arg: {field}")))?;
    raw.strip_prefix("__CHANNEL__:")
        .map(|s| s.to_string())
        .ok_or_else(|| {
            Error::from_reason(format!("invalid channel marker in {field}: {raw}"))
        })
}

/// JSON event sink. One per subscribe_* call; the same `OnceLock`-stored
/// `ThreadsafeFunction` services every channel — the channel id is
/// packed into the envelope so the JS side knows which renderer-side
/// listener to fire.
pub struct NapiSink {
    channel_id: String,
}

impl NapiSink {
    pub fn new(channel_id: String) -> Self {
        Self { channel_id }
    }
}

impl<E> EventSink<E> for NapiSink
where
    E: serde::Serialize + Send + Sync + 'static,
{
    fn send(&self, event: E) -> std::result::Result<(), String> {
        let tsfn = EMIT_JSON.get().ok_or("emitter not registered")?;
        let payload = serde_json::to_value(event).map_err(|e| e.to_string())?;
        let envelope = serde_json::json!({
            "channelId": self.channel_id,
            "payload": payload,
        });
        tsfn.call(envelope, ThreadsafeFunctionCallMode::NonBlocking);
        Ok(())
    }
}

/// Raw frame sink — for screenshare frames and terminal PTY output. The
/// `Buffer` constructor takes a `Vec<u8>` by value, so the only allocation
/// is the napi-side Buffer wrapper (the underlying bytes are moved).
pub struct RawNapiSink {
    channel_id: String,
}

impl RawNapiSink {
    pub fn new(channel_id: String) -> Self {
        Self { channel_id }
    }
}

impl RawSink for RawNapiSink {
    fn send(&self, bytes: Vec<u8>) -> pollis_core::error::Result<()> {
        let tsfn = EMIT_RAW.get().ok_or_else(|| {
            pollis_core::error::Error::Config("raw emitter not registered".into())
        })?;
        tsfn.call(
            JsRawFrame {
                channel_id: self.channel_id.clone(),
                payload: bytes.into(),
            },
            ThreadsafeFunctionCallMode::NonBlocking,
        );
        Ok(())
    }
}
