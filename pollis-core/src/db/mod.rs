pub mod chunk;
pub mod local;
pub mod remote;

// The remote schema lives in `pollis-schema` (its `migrations/` and
// `migrations-log/` directories are the production source of truth that
// `scripts/db-apply.sh` applies). It is its own crate so `pollis-delivery` —
// which must not depend on `pollis-core` — can build its test databases from
// the SAME definitions instead of hand-rolling partial ones. Re-exported here
// so every existing caller (`src-tauri/src/test_harness.rs`,
// `pollis-tui/tests/common`) keeps its import path.
pub use pollis_schema::{
    BASELINE_SQL, LOG_DB_SCHEMA, POST_BASELINE_LOG_MIGRATIONS, POST_BASELINE_MIGRATIONS,
};

pub mod queries {
    pub const MESSAGES_BY_SENDER: &str = include_str!("queries/messages_by_sender.sql");
    pub const CHANNEL_PREVIEWS: &str = include_str!("queries/channel_previews.sql");
}
