//! Lets `pollis-core` push events through a Tauri `Channel`.

use serde::Serialize;
use tauri::ipc::{Channel, InvokeResponseBody};

use pollis_core::sink::{EventSink, RawSink};

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

/// Binary frame channel: raw `Vec<u8>` payloads.
pub struct RawChannelSink(pub Channel<InvokeResponseBody>);

impl RawSink for RawChannelSink {
    fn send(&self, bytes: Vec<u8>) -> pollis_core::error::Result<()> {
        self.0
            .send(InvokeResponseBody::Raw(bytes))
            .map_err(|e| pollis_core::error::Error::Other(anyhow::anyhow!("frame channel: {e}")))
    }
}
