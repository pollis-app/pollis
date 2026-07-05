//! `pollis_tui` — the reusable, shell-free core of the Pollis terminal client.
//!
//! Everything here is UI-agnostic: it calls `pollis_core::commands::*` directly
//! (no Tauri, no IPC — see `docs/pollis-tui-spec.md` §2) and returns plain typed
//! data. The ratatui binary (`src/main.rs` and its `app`/`ui`/`terminal`
//! modules) is a thin presentation layer on top; the in-box smoke tests
//! (`tests/`) link this library and drive the same functions the UI will.
//!
//! Modules:
//! - [`auth`] — the order-enforcing signup/unlock wrappers (M1).
//! - [`data`] — the conversation + message READ layer (M2, §8 command→screen map).
//! - [`send`] — the conversation + group WRITE layer (M3, §8 command→screen map).
//! - [`sync`] — the §6 polling sync loop that keeps a client caught up (M2).

pub mod auth;
pub mod data;
pub mod send;
pub mod sync;
