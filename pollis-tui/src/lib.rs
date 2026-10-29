//! `pollis_tui` — the shell-free core of the Pollis terminal client.
//!
//! Calls `pollis_core::commands::*` directly (no Tauri, no IPC; see
//! `docs/pollis-tui-spec.md` §2). The UI state machine (`app`, `ui`, `home`,
//! `timeline`, `enroll_flow`) lives here too so the headless e2e tests
//! (`tests/ui_e2e.rs`) can drive it against a `TestBackend`; only `terminal`
//! stays binary-only. Layout: `.codesight/wiki/pollis-tui.md`.

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
