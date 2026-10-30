//! Headless notification fetch (#1599): the C ABI a Notification Service
//! Extension (iOS) or a data-push handler (Android) calls to fetch, decrypt
//! and preview pending messages without starting the app.

pub mod ffi;
