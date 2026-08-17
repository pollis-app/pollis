use std::collections::HashMap;
use std::sync::Arc;

use crate::state::AppState;

use super::participants::VoiceParticipantInfo;

pub(crate) use crate::commands::livekit_identity::{
    internal_identity, is_view_identity, user_id_from_identity, ParticipantKind,
};

/// Translate the opaque LiveKit identities in `wire` into the internal
/// identities the rest of the app speaks (#836).
///
/// Returns a map from wire identity to `(internal_identity, display_name)`, with
/// an entry for EVERY input. Identities already translated this process are
/// answered from `state.livekit_identities`; the rest go to the DS in one
/// batched request, because it holds the per-room key and no client does.
///
/// Resolution is best-effort by design. If the DS is unreachable the caller
/// still gets a usable identity for every input — the pseudonym itself, which is
/// stable, unique and perfectly good as a UI key — so an unresolvable peer shows
/// up as an unattributed tile rather than not showing up at all. That ordering
/// matters: this sits on the participant-joined path, and "you cannot see who
/// that is" is a far better failure than "they did not join".
pub(crate) async fn resolve_participants(
    state: &Arc<AppState>,
    logical_room: &str,
    wire: &[String],
) -> HashMap<String, (String, String)> {
    let missing: Vec<String> = {
        let cache = state.livekit_identities.lock().await;
        wire.iter()
            .filter(|id| !cache.contains_key(&(logical_room.to_string(), (*id).clone())))
            .cloned()
            .collect()
    };

    if !missing.is_empty() {
        match crate::commands::mls::ds_livekit_identities(state, logical_room, missing).await {
            Ok(resolved) => {
                let mut cache = state.livekit_identities.lock().await;
                for r in resolved {
                    let kind = ParticipantKind::from_wire(r.kind.as_deref().unwrap_or("voice"));
                    cache.insert(
                        (logical_room.to_string(), r.identity.clone()),
                        (internal_identity(&r.user_id, &r.identity, kind), r.name),
                    );
                }
            }
            Err(e) => {
                eprintln!("[livekit] identity resolve for {logical_room} failed: {e}");
            }
        }
    }

    let cache = state.livekit_identities.lock().await;
    wire.iter()
        .map(|id| {
            let translated = cache
                .get(&(logical_room.to_string(), id.clone()))
                .cloned()
                .unwrap_or_else(|| (id.clone(), String::new()));
            (id.clone(), translated)
        })
        .collect()
}

/// Single-identity convenience over [`resolve_participants`].
pub(crate) async fn resolve_participant(
    state: &Arc<AppState>,
    logical_room: &str,
    wire: &str,
) -> (String, String) {
    resolve_participants(state, logical_room, std::slice::from_ref(&wire.to_string()))
        .await
        .remove(wire)
        .unwrap_or_else(|| (wire.to_string(), String::new()))
}

/// Pin the LOCAL participant's translation for `logical_room`, reading its wire
/// identity straight out of the token the DS just minted for us.
///
/// LiveKit reports our own participant's events under its wire pseudonym —
/// `TrackMuted` fires on every push-to-talk keypress — and those must land on the
/// local tile, which the renderer keys by the `voice-{user}:{device}` string it
/// builds from `get_device_id`. Resolving our own pseudonym through the DS would
/// hand back a *synthetic*-device identity instead (the device id is deliberately
/// not recoverable from a pseudonym), quietly detaching the local tile from its
/// own mute state. So the mapping is pinned, not derived.
///
/// No-ops if the token has no readable `sub`, in which case the local
/// participant simply resolves like anyone else.
pub(crate) async fn pin_local_identity(
    state: &Arc<AppState>,
    logical_room: &str,
    token: &str,
    local_identity: &str,
    display_name: &str,
) {
    let Some(wire) = crate::commands::livekit_identity::jwt_subject(token) else {
        return;
    };
    let mut cache = state.livekit_identities.lock().await;
    cache.insert(
        (logical_room.to_string(), wire),
        (local_identity.to_string(), display_name.to_string()),
    );
}

/// Look up the avatar_url for a single user_id from the remote DB.
/// Best-effort — returns None on any failure (offline, no row, etc.).
pub(crate) async fn lookup_avatar_url(state: &AppState, user_id: &str) -> Option<String> {
    let conn = match state.remote_db.conn().await {
        Ok(c) => c,
        Err(_) => return None,
    };
    let mut rows = match conn
        .query(
            "SELECT avatar_url FROM users WHERE id = ?1",
            libsql::params![user_id.to_string()],
        )
        .await
    {
        Ok(r) => r,
        Err(_) => return None,
    };
    let row = rows.next().await.ok().flatten()?;
    row.get::<Option<String>>(0).ok().flatten()
}

/// Look up the avatar_url for an INTERNAL participant identity
/// (`voice-{user}:{device}` / `{user}:{device}`). Returns None if the identity
/// has no recognised user — which since #836 also covers a wire pseudonym that
/// could not be resolved — or if the lookup fails.
pub(crate) async fn lookup_avatar_url_for_identity(
    state: &AppState,
    identity: &str,
) -> Option<String> {
    let user_id = user_id_from_identity(identity)?;
    lookup_avatar_url(state, user_id).await
}

/// Fill in `avatar_url` for each participant by looking up the user in the
/// remote DB. One query per call (not per participant). Best-effort — if the
/// lookup fails the participants are returned with `avatar_url = None`.
pub(super) async fn enrich_participants_with_avatars(
    state: &AppState,
    mut participants: Vec<VoiceParticipantInfo>,
) -> Vec<VoiceParticipantInfo> {
    if participants.is_empty() {
        return participants;
    }
    let user_ids: Vec<String> = participants
        .iter()
        .filter_map(|p| user_id_from_identity(&p.identity).map(|s| s.to_string()))
        .collect();
    if user_ids.is_empty() {
        return participants;
    }
    // Build a parameterised IN clause: `?,?,?,...`.
    let placeholders = std::iter::repeat_n("?", user_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT id, avatar_url FROM users WHERE id IN ({placeholders})"
    );
    let conn = match state.remote_db.conn().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[voice] avatar enrich: remote conn failed: {e}");
            return participants;
        }
    };
    let params: Vec<libsql::Value> = user_ids
        .iter()
        .map(|id| libsql::Value::Text(id.clone()))
        .collect();
    let mut rows = match conn.query(&sql, params).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[voice] avatar enrich: query failed: {e}");
            return participants;
        }
    };
    let mut by_id: std::collections::HashMap<String, Option<String>> =
        std::collections::HashMap::new();
    while let Ok(Some(row)) = rows.next().await {
        let id: String = match row.get(0) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let url: Option<String> = row.get(1).ok();
        by_id.insert(id, url);
    }
    for p in participants.iter_mut() {
        if let Some(uid) = user_id_from_identity(&p.identity) {
            if let Some(url) = by_id.get(uid).cloned().flatten() {
                p.avatar_url = Some(url);
            }
        }
    }
    participants
}
