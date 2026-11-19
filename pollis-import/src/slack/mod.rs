//! Slack as an import source (#1179).
//!
//! **The workspace export is the primary path.** A workspace admin downloads
//! it from Slack's admin pages (Settings → Import/Export Data → Export) as a
//! ZIP, with no token and no app install; Pollis reads the ZIP or the folder
//! it unpacks to. What it holds:
//!
//! ```text
//! channels.json        public channels (name, purpose, topic, is_archived, members)
//! groups.json          private channels (only in exports that include them)
//! users.json           everyone (name, profile.email, is_admin/is_owner, deleted, is_bot)
//! dms.json, mpims.json direct messages (only some plans); reported, never imported
//! <channel>/<day>.json a channel's messages for one day: measured, never imported
//! ```
//!
//! A standard export has no custom emoji, no user groups and not even the
//! workspace's name. **The workspace snapshot** fills that gap, beside the
//! export files (in the folder or the ZIP):
//!
//! ```text
//! slack-workspace.json { "team":       team.info → team              (id, name)
//!                        "emoji":      emoji.list → emoji            (name → URL or "alias:other")
//!                        "usergroups": usergroups.list → usergroups  (handle, name, description, users) }
//! emoji/<name>.png|gif|jpg|webp   (optional; the emoji images)
//! icon.png|jpg|gif|webp           (optional; the workspace icon)
//! ```
//!
//! The snapshot is exactly what a direct reader with an OAuth token would
//! write (not built), so that reader only fetches, never plans. Without one,
//! the workspace is named from the export's file name.

mod archive;
mod export;
mod snapshot;

use std::io;
use std::path::Path;

pub use snapshot::SNAPSHOT_FILE;

/// Read a Slack workspace export: a `.zip` as downloaded, or the folder it
/// unpacks to. A bad file inside is a warning on the export; an `Err` means
/// this is not a Slack export at all (no `channels.json` or `groups.json`).
pub fn read_export(path: &Path) -> io::Result<crate::model::Export> {
    let mut archive = archive::Archive::open(path)?;
    if !archive.has("channels.json") && !archive.has("groups.json") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a Slack workspace export (no channels.json or groups.json)",
        ));
    }
    let snapshot = snapshot::read(&mut archive);
    export::read(&mut archive, snapshot, &workspace_name_from_path(path))
}

/// "Northwind Studio Slack export Jan 1 2023 - Nov 18 2026.zip" → "Northwind Studio":
/// the name Slack gives the download, which is the only place a standard
/// export names its workspace (`users.json` carries the team's id, never its
/// name). Underscores count as spaces, and a leading `xxxxxxxx-` upload prefix
/// (8 hex digits) is dropped, as copies passed through an uploader carry them.
/// A name without " Slack export " in it is used as it is.
fn workspace_name_from_path(path: &Path) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let spaced = stem.replace('_', " ");
    let Some(i) = spaced.to_ascii_lowercase().find(" slack export ") else {
        return stem.trim().to_string();
    };
    let before = &spaced[..i];
    let unprefixed = match (before.get(..8), before.get(8..9)) {
        (Some(hex), Some("-")) if hex.bytes().all(|b| b.is_ascii_hexdigit()) => &before[9..],
        _ => before,
    };
    let name = unprefixed.trim();
    if name.is_empty() {
        return stem.trim().to_string();
    }
    name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_download_name_names_the_workspace() {
        let p = Path::new("/x/Northwind Studio Slack export Jan 1 2023 - Nov 18 2026.zip");
        assert_eq!(workspace_name_from_path(p), "Northwind Studio");
        let p = Path::new("/x/TestExportPollis Slack export Nov 19 2026 - Nov 19 2026.zip");
        assert_eq!(workspace_name_from_path(p), "TestExportPollis");
    }

    #[test]
    fn underscores_count_as_spaces() {
        let p = Path::new("/x/TestExportPollis_Slack_export_Nov_19_2026_-_Nov_19_2026.zip");
        assert_eq!(workspace_name_from_path(p), "TestExportPollis");
        let p = Path::new("/x/Northwind_Studio_SLACK_EXPORT_Jan_1_2023_-_Nov_18_2026.zip");
        assert_eq!(workspace_name_from_path(p), "Northwind Studio");
    }

    #[test]
    fn an_upload_prefix_is_dropped() {
        let p = Path::new("/x/79e62724-TestExportPollis_Slack_export_Nov_19_2026_-_Nov_19_2026.zip");
        assert_eq!(workspace_name_from_path(p), "TestExportPollis");
        let p = Path::new("/x/79E62724-Northwind Studio Slack export Jan 1 2023 - Nov 18 2026.zip");
        assert_eq!(workspace_name_from_path(p), "Northwind Studio");
        // Not 8 hex digits, so it is part of the name.
        let p = Path::new("/x/acme-co Slack export Jan 2024.zip");
        assert_eq!(workspace_name_from_path(p), "acme-co");
    }

    #[test]
    fn other_names_are_used_as_they_are() {
        assert_eq!(workspace_name_from_path(Path::new("/x/acme")), "acme");
        assert_eq!(workspace_name_from_path(Path::new("/x/my_team_backup.zip")), "my_team_backup");
        assert_eq!(workspace_name_from_path(Path::new("/x/Slack export Jan 2024.zip")), "Slack export Jan 2024");
        let p = Path::new("/x/79e62724-acme_export.zip");
        assert_eq!(workspace_name_from_path(p), "79e62724-acme_export");
    }
}
