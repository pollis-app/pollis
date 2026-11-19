//! Discord as an import source.
//!
//! Read from a DiscordChatExporter (DCE) JSON export, made by the guild's
//! owner with a bot token (a user token is a self-bot, against Discord's
//! terms). Run DCE with `--media` so attachments, avatars and emoji are
//! downloaded beside the JSON: Discord's CDN links are signed and expire,
//! so an export without media keeps only dead URLs.

pub mod dce;
pub mod snapshot;

pub use dce::read_export_dir;
