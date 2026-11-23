//! Who is in a LiveKit room: resolving wire pseudonyms (#836) and the people they show.
//!
//! Always compiled. Desktop's realtime loop (`commands::livekit`) and the mobile bridge, whose
//! rooms are joined by the JS LiveKit SDK, resolve through the one cache in `AppState`, and both
//! apply the same presence rule (#2059).

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use crate::commands::livekit_identity::{internal_identity, user_id_from_identity, ParticipantKind};
use crate::error::Result;
use crate::state::AppState;

/// Translate the opaque LiveKit identities in `wire` into the internal
/// identities the rest of the app speaks (#836).
///
/// Returns `wire -> (internal_identity, display_name)` for EVERY input; cached or one DS batch.
///
/// Best-effort: unresolved peers keep their pseudonym (unattributed, not missing).
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

/// Give every cached participant of `user_id` the name `name` (#2084): a pseudonym never
/// changes, but the name it resolves to does when the user renames themselves.
pub(crate) async fn rename_user(state: &Arc<AppState>, user_id: &str, name: &str) {
    let mut cache = state.livekit_identities.lock().await;
    for (internal, shown) in cache.values_mut() {
        if user_id_from_identity(internal) == Some(user_id) {
            *shown = name.to_string();
        }
    }
}

/// The presence rule, shared by desktop's realtime loop and mobile (#2059): the other user one
/// participant's internal identity shows, if any. DS participants, unresolved pseudonyms and
/// `self_user_id` are no one.
pub fn present_user<'a>(internal: &'a str, self_user_id: &str) -> Option<&'a str> {
    user_id_from_identity(internal).filter(|uid| !uid.is_empty() && *uid != self_user_id)
}

/// The people a room's participants (internal identities) show, once each, sorted, by
/// [`present_user`]: a user is in the room while any session of theirs is (a device, a call, a
/// viewer, #1919).
pub fn present_user_ids<'a>(
    internal: impl IntoIterator<Item = &'a str>,
    self_user_id: &str,
) -> Vec<String> {
    internal
        .into_iter()
        .filter_map(|identity| present_user(identity, self_user_id))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// The users present in `logical_room`, given every remote participant's wire identity there:
/// a snapshot the caller replaces its previous one with (#2059, mobile's presence).
pub async fn room_presence(
    state: &Arc<AppState>,
    logical_room: &str,
    wire: &[String],
) -> Result<Vec<String>> {
    let self_user_id = crate::commands::mls::current_user_id(state).await?;
    let resolved = resolve_participants(state, logical_room, wire).await;
    Ok(present_user_ids(
        resolved.values().map(|(internal, _)| internal.as_str()),
        &self_user_id,
    ))
}

#[cfg(test)]
mod tests {
    use super::present_user_ids;

    #[test]
    fn each_person_once_whatever_their_sessions() {
        let present = present_user_ids(
            ["bob:dev1", "bob:dev2", "voice-carol:dev3", "dave:dev4:view", "bob:dev1:view"],
            "me",
        );
        assert_eq!(present, ["bob", "carol", "dave"]);
    }

    #[test]
    fn self_is_never_reported() {
        assert_eq!(present_user_ids(["me:laptop", "bob:phone"], "me"), ["bob"]);
    }

    /// An unresolved pseudonym or the DS is no one: a wrong dot is worse than none.
    #[test]
    fn unresolved_and_service_participants_are_no_one() {
        let present = present_user_ids(["d-AbCdEf", "v-xyz", "w-q", "pollis-ds", "server"], "me");
        assert!(present.is_empty());
    }

    #[test]
    fn an_empty_room_shows_no_one() {
        assert!(present_user_ids([], "me").is_empty());
    }
}
