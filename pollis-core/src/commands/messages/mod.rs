//! Message send / receive / edit / delete / reactions commands — split into
//! cohesive submodules. Public surface is preserved via the `pub use`
//! re-exports below so every external caller (Tauri shims, sibling
//! `commands::*` modules, integration tests) keeps resolving names at
//! `pollis_core::commands::messages::*`.

mod edit_delete;
mod ingest;
mod reactions;
mod read;
mod send;
mod types;

// ── Types ────────────────────────────────────────────────────────────────────
pub use types::{
    ChannelMessage, ChannelPreview, Message, MessageCursor, MessagePage, MessageWithContext,
    SearchResult,
};

// ── Send ─────────────────────────────────────────────────────────────────────
pub use send::send_message;

// ── Read / list / search ─────────────────────────────────────────────────────
pub use read::{
    get_channel_messages, get_dm_messages, list_channel_previews, list_messages,
    list_messages_by_sender, read_channel_messages, read_dm_messages, search_messages,
};

// ── Ingest (envelope pull + watermark + cleanup) ─────────────────────────────
pub use ingest::{
    ingest_channel_envelopes, ingest_channel_envelopes_inner, ingest_dm_envelopes,
    ingest_dm_envelopes_inner,
};

// ── Edit / delete ────────────────────────────────────────────────────────────
pub use edit_delete::{delete_message, edit_message};

// ── Reactions ────────────────────────────────────────────────────────────────
pub use reactions::{add_reaction, get_reactions, remove_reaction, Reaction};

#[cfg(test)]
mod tests;
