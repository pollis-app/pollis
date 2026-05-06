pub mod local;
pub mod remote;

/// Frozen baseline schema for the remote Turso DB. Embedded at compile time
/// so the integration test harness can stamp a fresh database without an
/// out-of-band migration step.
pub const BASELINE_SQL: &str = include_str!("migrations/000000_baseline.sql");

pub mod queries {
    pub const MESSAGES_BY_SENDER: &str = include_str!("queries/messages_by_sender.sql");
    pub const CHANNEL_PREVIEWS: &str = include_str!("queries/channel_previews.sql");
}
