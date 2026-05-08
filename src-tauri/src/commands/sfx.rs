// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::sfx::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::sfx::*;

#[tauri::command]
pub fn play_sfx(sound: &str) {
    pollis_core::commands::sfx::play_sfx(sound)
}

#[tauri::command]
pub fn start_ring() {
    pollis_core::commands::sfx::start_ring()
}

#[tauri::command]
pub fn stop_ring() {
    pollis_core::commands::sfx::stop_ring()
}
