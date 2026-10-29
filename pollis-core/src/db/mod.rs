pub mod chunk;
pub mod local;
pub mod search_text;

// The remote schema lives in `pollis-schema` so `pollis-delivery` can build test
// databases from the same definitions; re-exported to keep existing import paths.
pub use pollis_schema::{
    BASELINE_SQL, LOG_DB_SCHEMA, POST_BASELINE_LOG_MIGRATIONS, POST_BASELINE_MIGRATIONS,
};

