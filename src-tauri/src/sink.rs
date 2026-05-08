//! Tauri-side adapter that lets `pollis-core` push events through a
//! `tauri::ipc::Channel<T>` without depending on Tauri itself.

use serde::Serialize;
use tauri::ipc::Channel;

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
