use crate::state::AppState;

use super::participants::VoiceParticipantInfo;

/// Extract a Pollis user_id from a LiveKit participant identity. Voice room
/// participants use `voice-{user_id}`; realtime room participants use
/// `{user_id}:{device_id}`. Returns `None` for internal participants
/// ("server", "pollis-backend") and any other unrecognised shape.
pub(crate) fn user_id_from_identity(identity: &str) -> Option<&str> {
    if let Some(rest) = identity.strip_prefix("voice-") {
        return Some(rest);
    }
    if let Some((user_id, _device)) = identity.split_once(':') {
        return Some(user_id);
    }
    // Bare user_id (no device id) is also valid — the realtime path falls
    // back to that when there's no device id available yet.
    if identity == "server" || identity == "pollis-backend" {
        return None;
    }
    Some(identity)
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

/// Look up the avatar_url for a participant identity (voice-{user_id} or
/// {user_id}:{device_id}). Returns None if the identity has no recognised
/// user or the lookup fails.
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
    let placeholders = std::iter::repeat("?")
        .take(user_ids.len())
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
