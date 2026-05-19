//! Generic event-channel abstraction.
//!
//! Replaces direct dependence on `tauri::ipc::Channel<T>` inside command
//! and state code so `pollis-core` stays Tauri-runtime-free. The desktop
//! crate provides a `ChannelSink<T>` adapter that wraps Tauri's Channel.
//! A future CLI / TUI / mobile binary can supply its own implementation
//! (e.g. an mpsc-backed sink) without touching the core.

/// Push events from the backend toward whatever frontend / consumer is
/// listening. Implementations must be `Send + Sync` because event
/// production happens from spawned tasks owned by command state.
pub trait EventSink<T>: Send + Sync {
    /// Forward one event. Returns `Err` only when the consumer is gone /
    /// detached — callers typically log and drop the error since events
    /// are advisory (the canonical state lives in the database).
    fn send(&self, event: T) -> Result<(), String>;
}

/// Push raw bytes toward the frontend with no serde encoding. The desktop
/// crate adapts a `tauri::ipc::Channel<InvokeResponseBody>` into this so
/// payloads ride the binary IPC path (`InvokeResponseBody::Raw`) instead of
/// being JSON-encoded as a number array (~4-6x bloat + a JS-side parse).
/// Used by screenshare frame delivery and the terminal PTY output stream.
pub trait RawSink: Send + Sync {
    fn send(&self, bytes: Vec<u8>) -> crate::error::Result<()>;
}

/// No-op sink. Useful for the integration-test harness, or any code path
/// that drives command logic without a connected frontend.
pub struct NoopSink;

impl<T> EventSink<T> for NoopSink {
    fn send(&self, _event: T) -> Result<(), String> {
        Ok(())
    }
}
