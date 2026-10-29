//! Generic event-channel abstraction.
//!
//! Keeps `pollis-core` free of `tauri::ipc::Channel<T>`; each host supplies
//! its own adapter.

/// Push events from the backend toward whatever consumer is listening.
pub trait EventSink<T>: Send + Sync {
    /// Forward one event. Returns `Err` only when the consumer is gone; events
    /// are advisory (the canonical state lives in the database).
    fn send(&self, event: T) -> Result<(), String>;
}

/// Push raw bytes toward the frontend with no serde encoding, so payloads ride
/// Tauri's binary IPC path instead of a JSON number array.
pub trait RawSink: Send + Sync {
    fn send(&self, bytes: Vec<u8>) -> crate::error::Result<()>;
}

/// No-op sink, for the test harness or code paths without a frontend.
pub struct NoopSink;

impl<T> EventSink<T> for NoopSink {
    fn send(&self, _event: T) -> Result<(), String> {
        Ok(())
    }
}
