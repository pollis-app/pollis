//! Who is in a LiveKit room: resolving wire pseudonyms (#836) and the people they show.
//!
//! Always compiled. Desktop's realtime loop (`commands::livekit`) and the mobile bridge, whose
//! rooms are joined by the JS LiveKit SDK, resolve through the one cache in `AppState`, and both
//! apply the same presence rule (#2059).

use std::collections::{BTreeMap, BTreeSet, HashMap};
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

/// The LiveKit participant attribute a session sets to say whether its user is at the app (#2110):
/// `"1"` in use, `"0"` running but not in use (a hidden window, an idle desktop, a backgrounded
/// phone). Set on change only, never on a timer.
pub const ACTIVE_ATTRIBUTE: &str = "active";

/// The attribute's value for `active`.
pub fn active_attribute_value(active: bool) -> &'static str {
    if active {
        "1"
    } else {
        "0"
    }
}

/// Whether one session counts as in use, from its `active` attribute. Only an explicit `"0"` is
/// inactive: a build that predates the attribute sets none and must still show online.
pub fn session_active(value: Option<&str>) -> bool {
    value != Some("0")
}

/// The people a room's sessions (internal identity, in use) show, once each, sorted, by
/// [`present_user`], each with whether any of their sessions there is in use: a user is in the
/// room while any session of theirs is (a device, a call, a viewer, #1919), and active while any
/// of those is (#2110).
pub fn presence_by_user<'a>(
    sessions: impl IntoIterator<Item = (&'a str, bool)>,
    self_user_id: &str,
) -> BTreeMap<String, bool> {
    let mut out: BTreeMap<String, bool> = BTreeMap::new();
    for (identity, active) in sessions {
        if let Some(uid) = present_user(identity, self_user_id) {
            *out.entry(uid.to_string()).or_insert(false) |= active;
        }
    }
    out
}

/// Who one room shows (#2059, #2110): the users with a session in use there, and the users whose
/// every session there is not in use. Mobile's snapshot of the room.
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct RoomPresence {
    pub online: Vec<String>,
    pub away: Vec<String>,
}

impl RoomPresence {
    pub fn from_users(users: BTreeMap<String, bool>) -> Self {
        let mut out = Self::default();
        for (uid, active) in users {
            if active {
                out.online.push(uid);
            } else {
                out.away.push(uid);
            }
        }
        out
    }
}

/// The users present in `logical_room`, given every remote participant's wire identity there and
/// the ones whose `active` attribute says not in use: a snapshot the caller replaces its previous
/// one with (#2059, #2110; mobile's presence).
pub async fn room_presence(
    state: &Arc<AppState>,
    logical_room: &str,
    wire: &[String],
    inactive: &[String],
) -> Result<RoomPresence> {
    let self_user_id = crate::commands::mls::current_user_id(state).await?;
    let resolved = resolve_participants(state, logical_room, wire).await;
    let inactive: BTreeSet<&str> = inactive.iter().map(String::as_str).collect();
    Ok(RoomPresence::from_users(presence_by_user(
        resolved.iter().map(|(wire_id, (internal, _))| {
            (internal.as_str(), !inactive.contains(wire_id.as_str()))
        }),
        &self_user_id,
    )))
}

#[cfg(test)]
mod tests {
    use super::{presence_by_user, session_active, RoomPresence};

    fn in_use(ids: &[&'static str]) -> Vec<(&'static str, bool)> {
        ids.iter().map(|id| (*id, true)).collect()
    }

    #[test]
    fn each_person_once_whatever_their_sessions() {
        let present = presence_by_user(
            in_use(&["bob:dev1", "bob:dev2", "voice-carol:dev3", "dave:dev4:view", "bob:dev1:view"]),
            "me",
        );
        assert_eq!(present.keys().collect::<Vec<_>>(), ["bob", "carol", "dave"]);
    }

    #[test]
    fn self_is_never_reported() {
        let present = presence_by_user(in_use(&["me:laptop", "bob:phone"]), "me");
        assert_eq!(present.keys().collect::<Vec<_>>(), ["bob"]);
    }

    /// An unresolved pseudonym or the DS is no one: a wrong dot is worse than none.
    #[test]
    fn unresolved_and_service_participants_are_no_one() {
        let present =
            presence_by_user(in_use(&["d-AbCdEf", "v-xyz", "w-q", "pollis-ds", "server"]), "me");
        assert!(present.is_empty());
    }

    #[test]
    fn an_empty_room_shows_no_one() {
        assert!(presence_by_user([], "me").is_empty());
    }

    /// #2110: a build that predates the attribute sets none, and still shows online.
    #[test]
    fn only_an_explicit_zero_is_inactive() {
        assert!(session_active(None));
        assert!(session_active(Some("1")));
        assert!(session_active(Some("")));
        assert!(!session_active(Some("0")));
    }

    /// Online while any session there is in use; away only when every one is not; offline when
    /// the room shows none.
    #[test]
    fn online_away_and_offline() {
        let users = presence_by_user(
            [
                ("bob:laptop", false),
                ("bob:phone", true),
                ("carol:laptop", false),
                ("carol:phone", false),
            ],
            "me",
        );
        assert_eq!(
            RoomPresence::from_users(users),
            RoomPresence { online: vec!["bob".into()], away: vec!["carol".into()] }
        );
        assert_eq!(RoomPresence::from_users(presence_by_user([], "me")), RoomPresence::default());
    }
}
