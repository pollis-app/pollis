//! The remote schema, as data.
//!
//! Two databases ship (#420): the **main** Turso DB (users, groups, channels,
//! envelopes, key packages …) and the **commit-log** DB (`mls_commit_log`,
//! `mls_group_info`, `mls_welcome`, `mls_commit_since`). Each has its own
//! numbered migration directory, applied in version order by
//! `scripts/db-apply.sh`; this crate embeds both directories at compile time and
//! exposes them in that same order.
//!
//! ## Why this is its own crate
//!
//! `pollis-delivery` deliberately does **not** depend on `pollis-core` — the DS
//! is the sole writer to the MLS control plane and must not drag the client core
//! (OpenMLS, keystore, the whole command layer) into its build. Before this
//! crate existed, that independence was paid for by hand-rolling a partial
//! `CREATE TABLE` block per test module, and those copies drifted: eleven
//! different `user_device` definitions, `group_member` with and without its
//! primary key, `mls_key_package` with a column production does not have. A
//! missing primary key does not merely look untidy — it lets a test insert a
//! duplicate membership row that production rejects, so the test proves less
//! than it claims.
//!
//! `pollis-device-cert` is the precedent: a dependency-free crate that both
//! sides share so there is exactly one definition of a shared artefact. Same
//! shape here, and the same rule applies — nothing that is not schema goes in.
//!
//! ## Adding a migration
//!
//! Drop the numbered `.sql` file in `migrations/` (or `migrations-log/`) and add
//! it to [`POST_BASELINE_MIGRATIONS`] (or [`POST_BASELINE_LOG_MIGRATIONS`]).
//! `migrations/000000_baseline.sql` is frozen and must never be edited.
//! `schema_is_complete` in this crate's tests fails if a file on disk is missing
//! from the list, so the two cannot silently diverge.

/// Frozen baseline schema for the main remote DB. Embedded at compile time so a
/// test harness can stamp a fresh database without an out-of-band migration
/// step.
pub const BASELINE_SQL: &str = include_str!("../migrations/000000_baseline.sql");

/// Schema for the SEPARATE commit-log DB (`LOG_DB_URL`): the MLS control-plane
/// tables and their indexes, no FKs to the main DB. Embedded so a test harness
/// can bootstrap a genuinely separate log DB — mirroring the #420 production
/// split — and so a misrouted query (a main-DB read on the log connection, or
/// vice versa) fails loudly instead of silently finding every table on one
/// shared file.
pub const LOG_DB_SCHEMA: &str = include_str!("../migrations-log/000001_commit_log_db.sql");

/// Migrations applied on top of the commit-log DB schema, in version order.
/// Mirrors CI's `db-apply.sh` second apply (`MIGRATIONS_DIR=migrations-log`).
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
];

/// Migrations applied on top of the baseline, in version order. CI's
/// `db-apply.sh` is the production source of truth; this list mirrors it.
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
    // Note: version 7 (000007) is intentionally skipped — it was a
    // previously-reverted DS-trigger / commit-log-DB migration (see
    // docs/goal-a-deploy-runbook.md "000007 hazard"). Reusing it would collide.
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
];

/// Every script for the MAIN DB, in apply order (baseline first). One element
/// per file: each must be handed to a batch executor whole, because
/// `migrations-log/000005` contains `CREATE TRIGGER … BEGIN … ; END` and a naive
/// split on `;` would cut it in half.
pub fn main_scripts() -> Vec<&'static str> {
    let mut out = vec![BASELINE_SQL];
    out.extend(POST_BASELINE_MIGRATIONS.iter().map(|(_, _, sql)| *sql));
    out
}

/// Every script for the COMMIT-LOG DB, in apply order.
pub fn log_scripts() -> Vec<&'static str> {
    let mut out = vec![LOG_DB_SCHEMA];
    out.extend(POST_BASELINE_LOG_MIGRATIONS.iter().map(|(_, _, sql)| *sql));
    out
}

/// Every script for a SINGLE-DB deploy — main scripts then log scripts against
/// one URL, exactly the two `db-apply.sh` invocations a single-DB environment
/// runs. Order matters and is load-bearing: the frozen baseline still carries
/// pre-#420 copies of the three log tables without their `generation` column,
/// and it is `migrations-log/000004` (applied second) that adds the column and
/// swaps `idx_mls_commit_conv_epoch` for the generation-aware unique index.
/// Applying the log scripts first would leave a `CREATE TABLE IF NOT EXISTS`
/// no-op and a schema that no deploy has.
pub fn single_db_scripts() -> Vec<&'static str> {
    let mut out = main_scripts();
    out.extend(log_scripts());
    out
}

/// Stamp a real connection with the real schema. Behind the `libsql` feature so
/// the constants above stay dependency-free; test targets turn it on.
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

    /// The main Turso DB as production has it.
    pub async fn main_db(conn: &Connection) -> Result<(), Error> {
        run(conn, super::main_scripts()).await
    }

    /// The separate commit-log DB as production has it.
    pub async fn log_db(conn: &Connection) -> Result<(), Error> {
        run(conn, super::log_scripts()).await
    }

    /// Both, on one connection — the single-DB deploy shape.
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

    /// A migration file that exists on disk but is missing from the list above
    /// would be applied by `db-apply.sh` in production and NOT by any test
    /// harness — the exact split that lets a test pass against a schema no
    /// deploy has. Fail the build instead.
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
