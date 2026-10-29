//! `pollis_tui` — the reusable, shell-free core of the Pollis terminal client.
//!
//! UI-agnostic: calls `pollis_core::commands::*` directly and returns plain typed data.
//!
//! Data/sync/auth modules:
//! - [`auth`] — the order-enforcing signup/unlock wrappers (M1).
//! - [`cli`] — `--help` / `--version` and the no-terminal message (#1337).
//! - [`content`] — the `_att`/`_txt` content envelope, mentions, reaction
//!   shortcodes (#1337).
//! - [`data`] — the conversation + message READ layer (M2, §8 command→screen map).
//! - [`enroll`] — multi-device enrollment + Secret-Key recovery wrappers (M4).
//! - [`send`] — the conversation + group WRITE layer (M3, §8 command→screen map).
//! - [`sync`] — the §6 polling sync loop that keeps a client caught up (M2).
//!
//! UI state-machine modules, in the library so `tests/ui_e2e.rs` can drive them headless:
//! - [`app`] — the UI state machine (screens, actions, key handling).
//! - [`ui`] — the immediate-mode renderer (`render` + layout helpers).
//! - [`home`] — the pure three-pane Home model.
//! - [`timeline`] — the message pane's wrapped, styled layout (#1337).
//! - [`enroll_flow`] — the pure multi-device enrollment/recovery screen model.

pub mod auth;
pub mod cli;
pub mod content;
pub mod data;
pub mod enroll;
pub mod send;
pub mod sync;

pub mod app;
pub mod enroll_flow;
pub mod home;
pub mod timeline;
pub mod ui;
