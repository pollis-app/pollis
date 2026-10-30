//! Rich push notifications decrypted on the device (#1599).
//!
//! - [`ffi`]: the plain C ABI a Notification Service Extension (iOS) or an FCM data-push handler
//!   (Android) calls; header at `pollis-core/include/pollis_notify.h`.
//! - [`fetch`]: the headless fetch behind it, which reuses the app's own ingest path.
//! - [`lock`]: the cross-process state lock every MLS-mutating step holds.
//! - [`escrow`]: the notification setting and the DB-key escrow it controls (security-relevant).
//! - [`host`]: the data dir, the saved connection config and the iOS App Group migration.
//! - [`preview`]: what a notification shows of a message body.

pub mod escrow;
pub mod ffi;
pub mod fetch;
pub mod host;
pub mod lock;
pub mod preview;
