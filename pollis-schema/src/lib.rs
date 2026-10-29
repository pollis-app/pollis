//! The remote schema, as data.
//!
//! Two databases ship (#420): the main Turso DB and the commit-log DB. Both
//! migration directories are embedded here in the order `scripts/db-apply.sh`
//! applies them, so the DS (which must not depend on `pollis-core`) and test
//! harnesses share one schema instead of drifting hand-rolled copies. Nothing
//! that is not schema goes in.
//!
//! To add a migration, drop the numbered `.sql` file in `migrations/` (or
//! `migrations-log/`) and list it in [`POST_BASELINE_MIGRATIONS`] (or
//! [`POST_BASELINE_LOG_MIGRATIONS`]). Never edit `000000_baseline.sql`.

/// The membership predicates shared by the DS (the enforcing copy,
/// `pollis_delivery::groups`) and the client preflight
/// (`pollis_core::commands::groups::authz`), so the two cannot drift (#942).
/// Parity: `pollis-delivery/tests/join_requests.rs::preflight_parity`.
pub mod authz {
    /// The caller's role in a group, or no row when they are not a member.
    /// Parameters: `?1` group id, `?2` user id.
    ///
    /// `role` is read, not tested in SQL, so an undecodable value errors rather
    /// than reading as "not an admin" (#875).
    pub const GROUP_ROLE_SQL: &str =
        "SELECT role FROM group_member WHERE group_id = ?1 AND user_id = ?2";

    /// Whether a group id names a real group. Parameter: `?1` group id.
    ///
    /// Not redundant with `REFERENCES groups(id)`: every deployment runs with
    /// `foreign_keys=OFF`.
    pub const GROUP_EXISTS_SQL: &str = "SELECT 1 FROM groups WHERE id = ?1";
}

/// Frozen baseline schema for the main remote DB.
pub const BASELINE_SQL: &str = include_str!("../migrations/000000_baseline.sql");

/// Schema for the separate commit-log DB (`LOG_DB_URL`). A separate test DB
/// makes a misrouted query fail loudly (#420).
pub const LOG_DB_SCHEMA: &str = include_str!("../migrations-log/000001_commit_log_db.sql");

/// Commit-log DB migrations, in version order (`MIGRATIONS_DIR=migrations-log`).
pub const POST_BASELINE_LOG_MIGRATIONS: &[(u32, &str, &str)] = &[
    (
        2,
        "mls_welcome_unique_recipient",
        include_str!("../migrations-log/000002_mls_welcome_unique_recipient.sql"),
    ),
    (
        3,
        "mls_commit_since",
        include_str!("../migrations-log/000003_mls_commit_since.sql"),
    ),
    (
        4,
        "commit_generation",
        include_str!("../migrations-log/000004_commit_generation.sql"),
    ),
    (
        5,
        "mls_commit_log_triggers",
        include_str!("../migrations-log/000005_mls_commit_log_triggers.sql"),
    ),
    (
        6,
        "mls_welcome_submitted_by",
        include_str!("../migrations-log/000006_mls_welcome_submitted_by.sql"),
    ),
];

/// Main-DB migrations on top of the baseline, in version order.
pub const POST_BASELINE_MIGRATIONS: &[(u32, &str, &str)] = &[
    (
        1,
        "user_preferred_name",
        include_str!("../migrations/000001_user_preferred_name.sql"),
    ),
    (
        2,
        "index_gm_user_and_channels_group",
        include_str!("../migrations/000002_index_gm_user_and_channels_group.sql"),
    ),
    (
        3,
        "mls_commit_log_unique_epoch",
        include_str!("../migrations/000003_mls_commit_log_unique_epoch.sql"),
    ),
    (
        4,
        "user_device_revoked_at",
        include_str!("../migrations/000004_user_device_revoked_at.sql"),
    ),
    (
        5,
        "account_key_log",
        include_str!("../migrations/000005_account_key_log.sql"),
    ),
    (
        6,
        "push_token",
        include_str!("../migrations/000006_push_token.sql"),
    ),
    // Version 7 is skipped: a reverted migration (docs/goal-a-deploy-runbook.md
    // "000007 hazard"). Reusing it would collide.
    (
        8,
        "message_envelope_sealed_sender",
        include_str!("../migrations/000008_message_envelope_sealed_sender.sql"),
    ),
    (
        9,
        "directory_index",
        include_str!("../migrations/000009_directory_index.sql"),
    ),
    (
        10,
        "key_package_ciphersuite",
        include_str!("../migrations/000010_key_package_ciphersuite.sql"),
    ),
    (
        11,
        "device_pq_signature_pub",
        include_str!("../migrations/000011_device_pq_signature_pub.sql"),
    ),
    (
        12,
        "attachment_ref",
        include_str!("../migrations/000012_attachment_ref.sql"),
    ),
    (
        13,
        "conversation_watermark_reported_at",
        include_str!("../migrations/000013_conversation_watermark_reported_at.sql"),
    ),
    (
        14,
        "group_invite_link",
        include_str!("../migrations/000014_group_invite_link.sql"),
    ),
    (
        15,
        "custom_emoji",
        include_str!("../migrations/000015_custom_emoji.sql"),
    ),
    (
        16,
        "conversation_namespace",
        include_str!("../migrations/000016_conversation_namespace.sql"),
    ),
    (
        17,
        "conversation_guard_triggers",
        include_str!("../migrations/000017_conversation_guard_triggers.sql"),
    ),
    (
        18,
        "read_cursor_sync",
        include_str!("../migrations/000018_read_cursor_sync.sql"),
    ),
    (
        19,
        "pinned_messages",
        include_str!("../migrations/000019_pinned_messages.sql"),
    ),
    (
        20,
        "vault_message",
        include_str!("../migrations/000020_vault_message.sql"),
    ),
    (
        21,
        "username_shape",
        include_str!("../migrations/000021_username_shape.sql"),
    ),
    (
        22,
        "key_package_claim_audit",
        include_str!("../migrations/000022_key_package_claim_audit.sql"),
    ),
    (
        23,
        "directory_lookup_budget",
        include_str!("../migrations/000023_directory_lookup_budget.sql"),
    ),
    (
        24,
        "push_token_device_binding",
        include_str!("../migrations/000024_push_token_device_binding.sql"),
    ),
    (
        25,
        "retire_directory_mirror",
        include_str!("../migrations/000025_retire_directory_mirror.sql"),
    ),
    (
        26,
        "email_case_insensitive",
        include_str!("../migrations/000026_email_case_insensitive.sql"),
    ),
    (
        27,
        "envelope_delete_capability",
        include_str!("../migrations/000027_envelope_delete_capability.sql"),
    ),
    (
        28,
        "delivery_sequence",
        include_str!("../migrations/000028_delivery_sequence.sql"),
    ),
    (
        29,
        "drop_retired_directory_mirror",
        include_str!("../migrations/000029_drop_retired_directory_mirror.sql"),
    ),
    (
        30,
        "push_handle",
        include_str!("../migrations/000030_push_handle.sql"),
    ),
    (
        31,
        "attachment_ownership",
        include_str!("../migrations/000031_attachment_ownership.sql"),
    ),
    (
        32,
        "attachment_ownership_retired",
        include_str!("../migrations/000032_attachment_ownership_retired.sql"),
    ),
    (
        33,
        "user_report",
        include_str!("../migrations/000033_user_report.sql"),
    ),
    (
        34,
        "scrub_ip_shaped_device_names",
        include_str!("../migrations/000034_scrub_ip_shaped_device_names.sql"),
    ),
    (
        35,
        "ds_error_log",
        include_str!("../migrations/000035_ds_error_log.sql"),
    ),
    (
        36,
        "file_keystate",
        include_str!("../migrations/000036_file_keystate.sql"),
    ),
    (
        37,
        "push_ticket",
        include_str!("../migrations/000037_push_ticket.sql"),
    ),
    (
        38,
        "channel_mute",
        include_str!("../migrations/000038_channel_mute.sql"),
    ),
    (
        39,
        "message_reaction_conversation",
        include_str!("../migrations/000039_message_reaction_conversation.sql"),
    ),
    (
        40,
        "push_handle_dismissed",
        include_str!("../migrations/000040_push_handle_dismissed.sql"),
    ),
];

/// Every main-DB script, baseline first. Hand each to a batch executor whole:
/// trigger bodies break a naive `;` split.
pub fn main_scripts() -> Vec<&'static str> {
    let mut out = vec![BASELINE_SQL];
    out.extend(POST_BASELINE_MIGRATIONS.iter().map(|(_, _, sql)| *sql));
    out
}

pub fn log_scripts() -> Vec<&'static str> {
    let mut out = vec![LOG_DB_SCHEMA];
    out.extend(POST_BASELINE_LOG_MIGRATIONS.iter().map(|(_, _, sql)| *sql));
    out
}

/// Every script for a single-DB deploy: main then log. The order is
/// load-bearing: the baseline carries pre-#420 log tables that
/// `migrations-log/000004` then upgrades.
pub fn single_db_scripts() -> Vec<&'static str> {
    let mut out = main_scripts();
    out.extend(log_scripts());
    out
}

/// Stamp a connection with the real schema; behind `libsql` so the constants
/// stay dependency-free.
#[cfg(feature = "libsql")]
pub mod apply {
    use libsql::{Connection, Error};

    async fn run(conn: &Connection, scripts: Vec<&'static str>) -> Result<(), Error> {
        for sql in scripts {
            // Native batch: parses trigger bodies correctly, unlike a `;` split.
            conn.execute_batch(sql).await?;
        }
        Ok(())
    }

    pub async fn main_db(conn: &Connection) -> Result<(), Error> {
        run(conn, super::main_scripts()).await
    }

    pub async fn log_db(conn: &Connection) -> Result<(), Error> {
        run(conn, super::log_scripts()).await
    }

    pub async fn single_db(conn: &Connection) -> Result<(), Error> {
        run(conn, super::single_db_scripts()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::Path;

    fn sql_files(dir: &str) -> Vec<String> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(dir);
        let mut out: Vec<String> = std::fs::read_dir(root)
            .expect("migration dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".sql"))
            .collect();
        out.sort();
        out
    }

    /// An unlisted file would reach production but no test harness.
    #[test]
    fn every_migration_file_is_listed() {
        let main = sql_files("migrations");
        assert_eq!(
            main.len(),
            POST_BASELINE_MIGRATIONS.len() + 1,
            "migrations/ has {} files but POST_BASELINE_MIGRATIONS lists {} (+ baseline): {main:?}",
            main.len(),
            POST_BASELINE_MIGRATIONS.len(),
        );
        let log = sql_files("migrations-log");
        assert_eq!(
            log.len(),
            POST_BASELINE_LOG_MIGRATIONS.len() + 1,
            "migrations-log/ has {} files but POST_BASELINE_LOG_MIGRATIONS lists {}: {log:?}",
            log.len(),
            POST_BASELINE_LOG_MIGRATIONS.len(),
        );
    }

    #[test]
    fn versions_are_unique_and_ascending() {
        for list in [POST_BASELINE_MIGRATIONS, POST_BASELINE_LOG_MIGRATIONS] {
            let versions: Vec<u32> = list.iter().map(|(v, _, _)| *v).collect();
            let unique: BTreeSet<u32> = versions.iter().copied().collect();
            assert_eq!(versions.len(), unique.len(), "duplicate migration version");
            let mut sorted = versions.clone();
            sorted.sort_unstable();
            assert_eq!(versions, sorted, "migrations are not in version order");
        }
    }

    #[test]
    fn script_lists_cover_every_migration() {
        assert_eq!(main_scripts().len(), POST_BASELINE_MIGRATIONS.len() + 1);
        assert_eq!(log_scripts().len(), POST_BASELINE_LOG_MIGRATIONS.len() + 1);
        assert_eq!(
            single_db_scripts().len(),
            main_scripts().len() + log_scripts().len()
        );
        // The single-DB order is load-bearing (see `single_db_scripts`).
        assert_eq!(single_db_scripts()[0], BASELINE_SQL);
        assert_eq!(single_db_scripts()[main_scripts().len()], LOG_DB_SCHEMA);
    }
}
