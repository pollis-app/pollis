//! Message send / receive / edit / delete / reactions commands — split into
//! cohesive submodules. Public surface is preserved via the `pub use`
//! re-exports below so every external caller (Tauri shims, sibling
//! `commands::*` modules, integration tests) keeps resolving names at
//! `pollis_core::commands::messages::*`.

mod edit_delete;
pub(crate) mod framing;
mod ingest;
mod reactions;
mod read;
mod retention;
mod send;
mod types;
// `watermark` (the `next_watermark` pure fn + `EnvKind`) is `pub` — not because
// any runtime caller needs the module path (they go through `pub use` below), but
// so the out-of-workspace `fuzz/` crate (Track B, #481) can fuzz the SAME
// production function Kani proves. Only `next_watermark` / `EnvKind` are `pub`
// inside it; `is_handled` stays private, so this widens no real API surface.
pub mod watermark;

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
    catch_up_mls_group_interleaved, ingest_channel_envelopes, ingest_channel_envelopes_inner,
    ingest_dm_envelopes, ingest_dm_envelopes_inner,
};

// ── Edit / delete ────────────────────────────────────────────────────────────
pub use edit_delete::{delete_message, edit_message};

// ── Reactions ────────────────────────────────────────────────────────────────
pub use reactions::{add_reaction, get_reactions, remove_reaction, Reaction};

// ── Retention / local eviction ───────────────────────────────────────────────
pub use retention::{get_message_retention, run_message_eviction, set_message_retention};

#[cfg(test)]
mod tests;
