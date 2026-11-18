//! Development-only admin grants, loaded from a local JSON file.
//!
//! Until #2005 lands, nothing signs an admin's grants, so a developer running
//! the runtime locally gives them in a file. This module exists only when
//! `debug_assertions` is on: a release build of the runtime has no way to read
//! grants from anywhere but a signed approval. `tests/dev_grants_release.rs`
//! proves the module is absent from a release build.

use std::path::Path;

use crate::{AdminGrants, ManifestError};

/// The environment variable a development runtime reads the file path from.
pub const DEV_GRANTS_ENV: &str = "POLLIS_APP_DEV_GRANTS";

/// Read admin grants from a development grants file.
pub fn load(path: &Path) -> Result<AdminGrants, ManifestError> {
    let bytes = std::fs::read(path)
        .map_err(|e| ManifestError::Parse(format!("dev grants file {}: {e}", path.display())))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| ManifestError::Parse(format!("dev grants file {}: {e}", path.display())))
}
