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
/// (`voice-{user}:{device}` / `{user}:{device}`). Returns None if the identity
/// has no recognised user — which since #836 also covers a wire pseudonym that
/// could not be resolved — or if the lookup fails.
pub(crate) async fn lookup_avatar_url_for_identity(
    state: &Arc<AppState>,
    identity: &str,
) -> Option<String> {
    let user_id = user_id_from_identity(identity)?;
    lookup_avatar_url(state, user_id).await
}

/// The distinct user ids an avatar batch should actually ask the DS about.
///
/// Split out of [`lookup_avatar_urls_for_identities`] because it is the half
/// that can silently go wrong: an identity with no recognised user (an
/// unresolved `v-…`/`w-…` pseudonym, or an internal participant like `server`)
/// must be dropped rather than forwarded as if it were a user id, and two
/// devices of the same person must collapse to one id instead of widening the
/// request. Both are invisible in a passing happy path.
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
/// identity that was passed in. ONE request, whatever the size of the room.
///
/// The join path seeds a whole roster at once, and calling
/// [`lookup_avatar_url_for_identity`] in a loop there cost one DS round trip per
/// person already in the channel — sequential, and squarely between the user and
/// a usable call. Identities with no recognised user, and users with no avatar,
/// are simply absent from the map.
///
/// Best-effort: on a failed lookup every caller gets `None`, because a missing
/// avatar must never cost the roster.
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
                eprintln!("[voice] batched avatar lookup failed: {e}");
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

/// Fill in `avatar_url` for each participant. ONE request per call, not per
/// participant. Best-effort — if the lookup fails the participants are returned
/// with `avatar_url = None`, because a missing avatar must never cost the
/// roster.
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
                eprintln!("[voice] avatar enrich: lookup failed: {e}");
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
        // The join path seeds the local participant plus every remote one, and
        // one person on two devices is two identities. The request must not
        // grow because of it.
        let got = avatar_lookup_user_ids(&ids(&["voice-u1:dev-a", "voice-u1:dev-b"]));
        assert_eq!(got, ids(&["u1"]));
    }

    #[test]
    fn avatar_batch_drops_identities_that_name_no_user() {
        // Unresolved wire pseudonyms and the DS's own participants are not
        // people. Forwarding them would ask the directory about ids that cannot
        // exist — a failure the per-identity helper used to absorb by returning
        // None before any request was built.
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
