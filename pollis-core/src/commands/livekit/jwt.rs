//! Participant JWT helpers for the desktop LiveKit integration.
//!
//! The actual minting is pure `jsonwebtoken` and lives in the always-compiled
//! `crate::commands::livekit_jwt` module so mobile (which compiles out this
//! whole `livekit` module) can reuse it for its `get_livekit_token` bridge
//! command. This file just re-exports those helpers so the desktop call sites
//! (`super::jwt::make_token`, `make_view_token`) keep resolving unchanged.

pub(crate) use crate::commands::livekit_jwt::{make_token, make_view_token};
