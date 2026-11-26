//! Sharing into Pollis from other apps' share sheets (#2100).
//!
//! - [`targets`]: the one query behind every picker: which conversations a share may go to, and
//!   in what order.
//! - [`send`]: one draft to several conversations, every row stored before any is delivered,
//!   with stable ids so nothing is sent twice.
//! - [`session`]: the iOS share extension's own headless session (PIN or biometric unlock, the
//!   App Group database, the cross-process lock).
//! - [`ffi`]: the C ABI the extension calls; header at `pollis-core/include/pollis_share.h`.
//!
//! The Android app runs the same [`targets`] and [`send`] through the bridge
//! (`share_targets`, `share_enqueue`, `share_deliver`).

pub mod ffi;
pub mod send;
pub mod session;
pub mod targets;
