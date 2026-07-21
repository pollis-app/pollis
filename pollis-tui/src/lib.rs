//! `pollis_tui` — the reusable, shell-free core of the Pollis terminal client.
//!
//! Everything here is UI-agnostic: it calls `pollis_core::commands::*` directly
//! (no Tauri, no IPC — see `docs/pollis-tui-spec.md` §2) and returns plain typed
//! data. The ratatui binary (`src/main.rs` and its `app`/`ui`/`terminal`
//! modules) is a thin presentation layer on top; the in-box smoke tests
//! (`tests/`) link this library and drive the same functions the UI will.
//!
//! Data/sync/auth modules:
//! - [`auth`] — the order-enforcing signup/unlock wrappers (M1).
//! - [`data`] — the conversation + message READ layer (M2, §8 command→screen map).
//! - [`enroll`] — multi-device enrollment + Secret-Key recovery wrappers (M4).
//! - [`send`] — the conversation + group WRITE layer (M3, §8 command→screen map).
//! - [`sync`] — the §6 polling sync loop that keeps a client caught up (M2).
//!
//! UI state-machine modules — promoted from binary-only into the library so the
//! headless in-process e2e tests (`tests/ui_e2e.rs`) can drive the real ratatui
//! state machine (keystrokes → [`app::App::on_key`] → [`app::App::run`] →
//! [`ui::render`]) against a `TestBackend`. The `pollis` binary reaches them via
//! the `pollis_tui::` path; only `terminal` (the crossterm raw-mode guard) stays
//! binary-only in `main.rs`.
//! - [`app`] — the UI state machine (screens, actions, key handling).
//! - [`ui`] — the immediate-mode renderer (`render` + layout helpers).
//! - [`home`] — the pure three-pane Home model.
//! - [`enroll_flow`] — the pure multi-device enrollment/recovery screen model.

pub mod auth;
pub mod data;
pub mod enroll;
pub mod send;
pub mod sync;

pub mod app;
pub mod enroll_flow;
pub mod home;
pub mod ui;
