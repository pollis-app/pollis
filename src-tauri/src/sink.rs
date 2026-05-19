//! Tauri-side adapter that lets `pollis-core` push events through a
//! `tauri::ipc::Channel<T>` without depending on Tauri itself.

use serde::Serialize;
use tauri::ipc::{Channel, InvokeResponseBody};

use pollis_core::commands::screenshare::RawSink;
use pollis_core::sink::EventSink;

pub struct ChannelSink<E>(pub Channel<E>)
where
    E: Send + Sync + Clone + Serialize + 'static;

impl<E> EventSink<E> for ChannelSink<E>
where
    E: Send + Sync + Clone + Serialize + 'static,
{
    fn send(&self, event: E) -> Result<(), String> {
        self.0.send(event).map_err(|e| e.to_string())
    }
}

/// Adapter for the binary frame channel. Wraps a `Channel<InvokeResponseBody>`
/// so pollis-core can push raw `Vec<u8>` (zero-copy frame payloads) without
/// depending on tauri.
pub struct RawChannelSink(pub Channel<InvokeResponseBody>);

impl RawSink for RawChannelSink {
    fn send(&self, bytes: Vec<u8>) -> pollis_core::error::Result<()> {
        self.0
            .send(InvokeResponseBody::Raw(bytes))
            .map_err(|e| pollis_core::error::Error::Other(anyhow::anyhow!("frame channel: {e}")))
    }
}
