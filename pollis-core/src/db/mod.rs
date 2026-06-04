pub mod local;
pub mod remote;

/// Frozen baseline schema for the remote Turso DB. Embedded at compile time
/// so the integration test harness can stamp a fresh database without an
/// out-of-band migration step.
pub const BASELINE_SQL: &str = include_str!("migrations/000000_baseline.sql");

/// Migrations applied on top of the baseline, in version order. CI's
/// `db-apply.sh` is the production source of truth; this list mirrors it so
/// the integration-test harness ends up with the same schema.
pub const POST_BASELINE_MIGRATIONS: &[(u32, &str, &str)] = &[
    (
        1,
        "user_preferred_name",
        include_str!("migrations/000001_user_preferred_name.sql"),
    ),
    (
        2,
        "index_gm_user_and_channels_group",
        include_str!("migrations/000002_index_gm_user_and_channels_group.sql"),
    ),
    (
        3,
        "mls_commit_log_unique_epoch",
        include_str!("migrations/000003_mls_commit_log_unique_epoch.sql"),
    ),
];

pub mod queries {
    pub const MESSAGES_BY_SENDER: &str = include_str!("queries/messages_by_sender.sql");
    pub const CHANNEL_PREVIEWS: &str = include_str!("queries/channel_previews.sql");
}
