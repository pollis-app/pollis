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
/// Returns an entry for every input. Cached translations answer first; the
/// rest go to the DS in one request (it holds the per-room key). Best effort:
/// if the DS is unreachable the pseudonym itself is returned, so an
/// unresolvable peer shows as an unattributed tile rather than not joining.
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
                crate::diag!("[livekit] identity resolve for {logical_room} failed: {e}");
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
/// Our own events arrive under our wire pseudonym (`TrackMuted` on every
/// push-to-talk press) and must land on the local tile, keyed
/// `voice-{user}:{device}`. Resolving our pseudonym through the DS would give a
/// synthetic-device identity instead, so the mapping is pinned. A no-op if the
/// token has no readable `sub`.
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

/// Look up the avatar_url for a single user_id. Best effort.
pub(crate) async fn lookup_avatar_url(
    state: &Arc<AppState>,
    user_id: &str,
) -> Option<String> {
    crate::commands::ds_reads::users(state, vec![user_id.to_string()], None)
        .await
        .ok()?
        .into_iter()
        .next()
        .and_then(|u| u.avatar_url)
}

/// Look up the avatar_url for an INTERNAL participant identity
/// (`voice-{user}:{device}` / `{user}:{device}`). `None` for an identity with
/// no recognised user
/// (including an unresolved pseudonym) or a failed lookup.
pub(crate) async fn lookup_avatar_url_for_identity(
    state: &Arc<AppState>,
    identity: &str,
) -> Option<String> {
    let user_id = user_id_from_identity(identity)?;
    lookup_avatar_url(state, user_id).await
}

/// The distinct user ids an avatar batch should actually ask the DS about.
///
/// Identities naming no user (unresolved `v-…`/`w-…` pseudonyms, `server`)
/// are dropped, and one person's devices collapse to one id.
fn avatar_lookup_user_ids(identities: &[String]) -> Vec<String> {
    let mut user_ids: Vec<String> = identities
        .iter()
        .filter_map(|i| user_id_from_identity(i).map(|s| s.to_string()))
        .collect();
    user_ids.sort();
    user_ids.dedup();
    user_ids
}

/// Avatar URLs for a BATCH of internal participant identities, keyed by the
/// identity passed in, in one request (the join path seeds a whole roster).
/// Best effort: a missing avatar must never cost the roster.
pub(crate) async fn lookup_avatar_urls_for_identities(
    state: &Arc<AppState>,
    identities: &[String],
) -> HashMap<String, String> {
    let user_ids = avatar_lookup_user_ids(identities);
    if user_ids.is_empty() {
        return HashMap::new();
    }
    let by_user: HashMap<String, String> =
        match crate::commands::ds_reads::users(state, user_ids, None).await {
            Ok(users) => users
                .into_iter()
                .filter_map(|u| u.avatar_url.map(|url| (u.id, url)))
                .collect(),
            Err(e) => {
                crate::diag!("[voice] batched avatar lookup failed: {e}");
                return HashMap::new();
            }
        };
    identities
        .iter()
        .filter_map(|identity| {
            let uid = user_id_from_identity(identity)?;
            let url = by_user.get(uid)?.clone();
            Some((identity.clone(), url))
        })
        .collect()
}

/// Fill in `avatar_url` for each participant, in one request. Best effort.
pub(super) async fn enrich_participants_with_avatars(
    state: &Arc<AppState>,
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
    let by_id: HashMap<String, Option<String>> =
        match crate::commands::ds_reads::users(state, user_ids, None).await {
            Ok(users) => users.into_iter().map(|u| (u.id, u.avatar_url)).collect(),
            Err(e) => {
                crate::diag!("[voice] avatar enrich: lookup failed: {e}");
                return participants;
            }
        };
    for p in participants.iter_mut() {
        if let Some(uid) = user_id_from_identity(&p.identity) {
            if let Some(url) = by_id.get(uid).cloned().flatten() {
                p.avatar_url = Some(url);
            }
        }
    }
    participants
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn avatar_batch_collapses_a_users_two_devices_into_one_id() {
        let got = avatar_lookup_user_ids(&ids(&["voice-u1:dev-a", "voice-u1:dev-b"]));
        assert_eq!(got, ids(&["u1"]));
    }

    #[test]
    fn avatar_batch_drops_identities_that_name_no_user() {
        let got = avatar_lookup_user_ids(&ids(&[
            "v-abcdef",
            "w-abcdef",
            "server",
            "pollis-ds",
            "voice-u1:dev-a",
        ]));
        assert_eq!(got, ids(&["u1"]));
    }

    #[test]
    fn avatar_batch_of_nothing_asks_for_nothing() {
        assert!(avatar_lookup_user_ids(&[]).is_empty());
        assert!(avatar_lookup_user_ids(&ids(&["v-abcdef"])).is_empty());
    }

    #[test]
    fn avatar_batch_keeps_every_distinct_user() {
        let got = avatar_lookup_user_ids(&ids(&["voice-u2:dev-a", "voice-u1:dev-a", "u3:dev-a"]));
        assert_eq!(got, ids(&["u1", "u2", "u3"]));
    }
}
