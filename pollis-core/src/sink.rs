//! Event-channel abstraction that keeps `pollis-core` free of the Tauri runtime.

/// Push events from the backend toward whatever consumer is listening.
pub trait EventSink<T>: Send + Sync {
    /// Forward one event. `Err` only when the consumer is gone; events are advisory.
    fn send(&self, event: T) -> Result<(), String>;
}

/// Push raw bytes over the binary IPC path, avoiding JSON number-array bloat.
pub trait RawSink: Send + Sync {
    fn send(&self, bytes: Vec<u8>) -> crate::error::Result<()>;
}

/// No-op sink, for code paths with no connected frontend.
pub struct NoopSink;

impl<T> EventSink<T> for NoopSink {
    fn send(&self, _event: T) -> Result<(), String> {
        Ok(())
    }
}
