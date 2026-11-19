//! Import a community from another chat platform into Pollis.
//!
//! This crate is the pure half of an import: it reads an export from disk,
//! normalises it into [`model`], and turns that into a dry-run [`plan::ImportPlan`]
//! that says exactly what would be created, what would be dropped, and why.
//! It never touches the network, a key, or Pollis state. Applying a plan
//! (creating the group and channels, uploading emoji and media, sealing history)
//! is `pollis-core`'s job, because every write there is a device-signed DS call.
//!
//! Discord is the first source, read from a DiscordChatExporter JSON export
//! (one file per channel, optionally with `--media` downloads beside it).
//! See `.codesight/wiki/import.md` for the design and the open decisions.

pub mod discord;
pub mod model;
pub mod plan;
pub mod report;
pub mod shortcode;
pub mod snowflake;
