//! Import a community from another chat platform into Pollis.
//!
//! This crate is the pure half of an import: it reads an export from disk,
//! normalises it into [`model`], and turns that into a dry-run [`plan::ImportPlan`]
//! that says exactly what would be created, what would be dropped, and why.
//! It never touches the network, a key, or Pollis state. Applying a plan
//! (creating the group and channels, uploading emoji, inviting people) is
//! `pollis-core`'s job, because every write there is a device-signed DS call.
//!
//! Two sources, one pipeline: a DiscordChatExporter JSON export ([`discord`])
//! and a Slack workspace export ([`slack`]). Each reader produces the same
//! [`model::Export`]; everything after that is source-neutral.
//! See `.codesight/wiki/import.md` for the design and the open decisions.

use std::path::Path;

use serde::{Deserialize, Serialize};

pub mod discord;
pub mod model;
pub mod plan;
pub mod report;
pub mod shortcode;
pub mod slack;
pub mod snowflake;

/// Which platform an export comes from: what the apps pass to pick a reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Discord,
    Slack,
}

impl SourceKind {
    pub fn of(source: model::Source) -> Self {
        match source {
            model::Source::DiscordChatExporter => SourceKind::Discord,
            model::Source::SlackExport => SourceKind::Slack,
        }
    }

    /// The platform's name, for messages.
    pub fn name(self) -> &'static str {
        match self {
            SourceKind::Discord => "Discord",
            SourceKind::Slack => "Slack",
        }
    }
}

/// Read an export with the reader for `kind`. `path` is a folder (Discord,
/// Slack) or a ZIP (Slack). A bad file inside is a warning on the export;
/// an `Err` means nothing could be read at all.
pub fn read_export(kind: SourceKind, path: &Path) -> std::io::Result<model::Export> {
    match kind {
        SourceKind::Discord => discord::read_export_dir(path),
        SourceKind::Slack => slack::read_export(path),
    }
}
