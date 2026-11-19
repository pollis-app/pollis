//! The workspace snapshot (`slack-workspace.json`): the three Web API
//! responses a standard export lacks, verbatim. See the module docs of
//! [`super`] for the shape.

use std::collections::BTreeMap;

use serde::Deserialize;

use super::archive::{Archive, JSON_MAX_BYTES};

pub const SNAPSHOT_FILE: &str = "slack-workspace.json";

/// Image extensions looked for beside the snapshot, in order.
pub const IMAGE_EXTS: [&str; 5] = ["png", "gif", "jpg", "jpeg", "webp"];

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub team: ApiTeam,
    /// `emoji.list`'s `emoji`: name → image URL, or `alias:<other name>`.
    pub emoji: BTreeMap<String, String>,
    pub usergroups: Vec<ApiUserGroup>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ApiTeam {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ApiUserGroup {
    pub id: String,
    pub handle: String,
    pub name: String,
    pub description: String,
    /// Present when listed with `include_users=true`.
    pub users: Vec<String>,
    /// Non-zero for a disabled group.
    pub date_delete: i64,
}

/// The snapshot, if there is one; a broken one is reported by the caller as
/// a warning and treated as absent.
pub fn read(archive: &mut Archive) -> Result<Option<Snapshot>, String> {
    match archive.read(SNAPSHOT_FILE, JSON_MAX_BYTES) {
        Ok(None) => Ok(None),
        Ok(Some(bytes)) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| format!("{SNAPSHOT_FILE}: not a workspace snapshot ({e})")),
        Err(e) => Err(format!("{SNAPSHOT_FILE}: could not read ({e})")),
    }
}

/// The first `<stem>.<ext>` present among `names`.
pub fn image_named<'a>(names: &'a [String], stem: &str) -> Option<&'a String> {
    IMAGE_EXTS.iter().find_map(|ext| {
        let wanted = format!("{stem}.{ext}");
        names.iter().find(|n| n.eq_ignore_ascii_case(&wanted))
    })
}
