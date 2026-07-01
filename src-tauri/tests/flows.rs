//! End-to-end integration harness for Pollis.
//!
//! Per-feature test modules live in `tests/flows/*.rs`; the shared
//! `TestClient` / world setup lives in `tests/flows/harness.rs`.
//!
//! Run with:
//! ```
//! cargo test --features test-harness --test flows
//! ```

#[path = "flows/harness.rs"]
mod harness;

#[path = "flows/adversarial.rs"]
mod adversarial;
#[path = "flows/auth.rs"]
mod auth;
#[path = "flows/dms.rs"]
mod dms;
#[path = "flows/groups.rs"]
mod groups;
#[path = "flows/heavy_churn.rs"]
mod heavy_churn;
#[path = "flows/messages.rs"]
mod messages;
#[path = "flows/rejoin.rs"]
mod rejoin;
#[path = "flows/security.rs"]
mod security;
#[path = "flows/voice.rs"]
mod voice;
