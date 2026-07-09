pub mod local;
pub mod remote;

/// Frozen baseline schema for the remote Turso DB. Embedded at compile time
/// so the integration test harness can stamp a fresh database without an
/// out-of-band migration step.
pub const BASELINE_SQL: &str = include_str!("migrations/000000_baseline.sql");

/// Schema for the SEPARATE commit-log DB (`LOG_DB_URL`): the three MLS
/// control-plane tables (`mls_commit_log` / `mls_welcome` / `mls_group_info`)
/// and their indexes, no FKs to the main DB. Embedded so the integration test
/// harness can bootstrap a genuinely separate log DB — mirroring the #420
/// production split — and so a misrouted query (a main-DB read on the log
/// connection, or vice versa) fails loudly instead of silently finding every
/// table on one shared file.
pub const LOG_DB_SCHEMA: &str = include_str!("migrations-log/000001_commit_log_db.sql");

/// Migrations applied on top of the commit-log DB schema, in version order.
/// Mirrors CI's `db-apply.sh` second apply (MIGRATIONS_DIR=migrations-log) so
/// the integration-test harness's log DB ends up with the same schema as prod.
pub const POST_BASELINE_LOG_MIGRATIONS: &[(u32, &str, &str)] = &[
    (
        2,
        "mls_welcome_unique_recipient",
        include_str!("migrations-log/000002_mls_welcome_unique_recipient.sql"),
    ),
];

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
    (
        4,
        "user_device_revoked_at",
        include_str!("migrations/000004_user_device_revoked_at.sql"),
    ),
    (
        5,
        "account_key_log",
        include_str!("migrations/000005_account_key_log.sql"),
    ),
    (
        6,
        "push_token",
        include_str!("migrations/000006_push_token.sql"),
    ),
    // Note: version 7 (000007) is intentionally skipped — it was a
    // previously-reverted DS-trigger / commit-log-DB migration (see
    // docs/goal-a-deploy-runbook.md "000007 hazard"). Reusing it would collide.
    (
        8,
        "message_envelope_sealed_sender",
        include_str!("migrations/000008_message_envelope_sealed_sender.sql"),
    ),
    (
        9,
        "directory_index",
        include_str!("migrations/000009_directory_index.sql"),
    ),
];

pub mod queries {
    pub const MESSAGES_BY_SENDER: &str = include_str!("queries/messages_by_sender.sql");
    pub const CHANNEL_PREVIEWS: &str = include_str!("queries/channel_previews.sql");
}
