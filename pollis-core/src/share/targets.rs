//! Which conversations a share may go to, and in what order (#2100).
//!
//! One query for every picker: the iOS share extension's native list and the Android app's
//! picker read the same rows in the same order, so the two cannot drift.
//!
//! A target is a conversation the user can post in: an accepted DM whose other members the user
//! has not blocked, or a text channel of a group they belong to. Never a DM request they have
//! not accepted, and never a voice channel (no message composer). Order: newest local activity
//! first (sent or received, as the DM list sorts, #1838), then conversations with none, DMs by
//! newest made first, then by name; ties break on the id, so the order is total.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::Serialize;

use crate::error::Result;
use crate::state::AppState;
use pollis_api::directory::DirectoryBootstrapResponse;

/// Most conversations one share may go to. Each target re-uploads every file, so this bounds
/// the fan-out.
pub const MAX_TARGETS: usize = 5;

/// Rows a picker gets when it asks for no particular number.
pub const DEFAULT_LIMIT: usize = 50;

/// Hard ceiling on rows per query.
const MAX_LIMIT: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    Dm,
    Channel,
}

/// One row of the picker. Field names are the C ABI's and the bridge's contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShareTarget {
    pub conversation_id: String,
    pub kind: TargetKind,
    /// A DM: the other member's display name (several, comma-separated, for a group DM). A
    /// channel: its name.
    pub title: String,
    /// A 1:1 DM: the other member's `@username`. A channel: its group's name.
    pub subtitle: Option<String>,
    /// A 1:1 DM: the other member, so a host can draw the cached profile photo.
    pub avatar_user_id: Option<String>,
    /// The newest local message's `sent_at`, if any.
    pub last_activity_at: Option<String>,
    /// For a group's channel: the group, so a host can group or open it.
    pub group_id: Option<String>,
}

/// Every conversation `user_id` may share to, unranked, from one bootstrap answer.
pub(crate) fn eligible(user_id: &str, boot: &DirectoryBootstrapResponse) -> Vec<Candidate> {
    let blocked: HashSet<&str> = boot.blocks.iter().map(|b| b.user_id.as_str()).collect();
    let mut out = Vec::new();
    for dm in &boot.dms {
        let others: Vec<_> = dm.members.iter().filter(|m| m.user_id != user_id).collect();
        // A DM with nobody else in it, or with anyone the user has blocked: no target. The DS
        // already leaves blocked DMs out; this holds the rule if it ever does not.
        if others.is_empty() || others.iter().any(|m| blocked.contains(m.user_id.as_str())) {
            continue;
        }
        let names: Vec<String> = others
            .iter()
            .map(|m| {
                m.preferred_name
                    .clone()
                    .filter(|n| !n.trim().is_empty())
                    .or_else(|| m.username.clone())
                    .unwrap_or_default()
            })
            .collect();
        let one = (others.len() == 1).then(|| others[0]);
        out.push(Candidate {
            target: ShareTarget {
                conversation_id: dm.id.clone(),
                kind: TargetKind::Dm,
                title: names.join(", "),
                subtitle: one.and_then(|m| m.username.clone()).map(|u| format!("@{u}")),
                avatar_user_id: one.map(|m| m.user_id.clone()),
                last_activity_at: None,
                group_id: None,
            },
            created_at: Some(dm.created_at.clone()),
            words: others
                .iter()
                .flat_map(|m| [m.preferred_name.clone(), m.username.clone()])
                .flatten()
                .collect(),
        });
    }
    for group in &boot.groups {
        for channel in &group.channels {
            // A voice channel has no composer: nothing can be posted there.
            if channel.channel_type != "text" {
                continue;
            }
            out.push(Candidate {
                target: ShareTarget {
                    conversation_id: channel.id.clone(),
                    kind: TargetKind::Channel,
                    title: channel.name.clone(),
                    subtitle: Some(group.group.name.clone()),
                    avatar_user_id: None,
                    last_activity_at: None,
                    group_id: Some(group.group.id.clone()),
                },
                created_at: None,
                words: vec![channel.name.clone(), group.group.name.clone()],
            });
        }
    }
    out
}

/// A target before ranking, with what ranking and matching read.
#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    pub target: ShareTarget,
    created_at: Option<String>,
    /// Names a query may match: display names and usernames, or channel and group names.
    words: Vec<String>,
}

/// Lowercased words of `s`, split on anything that is not a letter or digit.
fn words_of(s: &str) -> impl Iterator<Item = String> + '_ {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
}

/// Whether every term of `query` starts some word of the candidate's names (`ali do` finds
/// "Alice Doe"). A leading `#` or `@` is ignored, so `#gen` and `@ali` work as typed.
fn matches(candidate: &Candidate, query: &str) -> bool {
    let mut words: Vec<String> = candidate.words.iter().flat_map(|w| words_of(w)).collect();
    words.extend(words_of(&candidate.target.title));
    words_of(query).all(|term| words.iter().any(|w| w.starts_with(&term)))
}

/// Order `candidates` (see the module docs), keep those matching `query`, take `limit`.
pub(crate) fn rank(
    candidates: Vec<Candidate>,
    latest: &HashMap<String, String>,
    query: Option<&str>,
    limit: usize,
) -> Vec<ShareTarget> {
    let query = query.map(str::trim).filter(|q| !q.is_empty());
    let mut kept: Vec<(Option<chrono::DateTime<chrono::Utc>>, Option<chrono::DateTime<chrono::Utc>>, ShareTarget)> =
        candidates
            .into_iter()
            .filter(|c| query.is_none_or(|q| matches(c, q)))
            .map(|c| {
                let mut target = c.target;
                target.last_activity_at = latest.get(&target.conversation_id).cloned();
                let active = target
                    .last_activity_at
                    .as_deref()
                    .and_then(crate::commands::dm::parse_instant);
                let made = c.created_at.as_deref().and_then(crate::commands::dm::parse_instant);
                (active, made, target)
            })
            .collect();
    kept.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| a.2.title.to_lowercase().cmp(&b.2.title.to_lowercase()))
            .then_with(|| a.2.conversation_id.cmp(&b.2.conversation_id))
    });
    kept.into_iter()
        .take(limit.clamp(1, MAX_LIMIT))
        .map(|(_, _, t)| t)
        .collect()
}

/// The conversations `state`'s user may share to, best first. One DS round trip (the sidebar's
/// bootstrap) plus one local read for the activity order.
pub async fn share_targets(
    state: &Arc<AppState>,
    query: Option<String>,
    limit: Option<u32>,
) -> Result<Vec<ShareTarget>> {
    let user_id = crate::commands::mls::current_user_id(state).await?;
    let boot = crate::commands::ds_reads::bootstrap(state, &user_id).await?;
    let candidates = eligible(&user_id, &boot);
    let ids: Vec<String> = candidates.iter().map(|c| c.target.conversation_id.clone()).collect();
    let latest = {
        let guard = state.local_db.lock().await;
        match guard.as_ref().map(|db| crate::commands::dm::latest_sent_at_on(db.conn(), &ids)) {
            Some(Ok(latest)) => latest,
            Some(Err(e)) => {
                crate::diag!("[share] activity order: local read failed: {e}");
                HashMap::new()
            }
            None => HashMap::new(),
        }
    };
    Ok(rank(
        candidates,
        &latest,
        query.as_deref(),
        limit.map(|l| l as usize).unwrap_or(DEFAULT_LIMIT),
    ))
}

/// The ids among `wanted` that `user_id` may NOT share to, per `boot`. Empty when all may.
pub(crate) fn ineligible(user_id: &str, boot: &DirectoryBootstrapResponse, wanted: &[String]) -> Vec<String> {
    let ok: HashSet<String> = eligible(user_id, boot)
        .into_iter()
        .map(|c| c.target.conversation_id)
        .collect();
    wanted.iter().filter(|id| !ok.contains(*id)).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pollis_api::directory::{
        BlockedUserWire, ChannelWire, DmChannelWire, DmMemberWire, GroupWire, GroupWithChannelsWire,
    };

    fn member(id: &str, username: &str, name: Option<&str>) -> DmMemberWire {
        DmMemberWire {
            user_id: id.into(),
            username: Some(username.into()),
            preferred_name: name.map(str::to_string),
            avatar_url: None,
            added_by: None,
            added_at: None,
            accepted_at: Some("2026-01-01T00:00:00Z".into()),
        }
    }

    fn dm(id: &str, created_at: &str, others: Vec<DmMemberWire>) -> DmChannelWire {
        let mut members = vec![member("me", "me", None)];
        members.extend(others);
        DmChannelWire {
            id: id.into(),
            created_by: "me".into(),
            created_at: created_at.into(),
            members,
        }
    }

    fn channel(id: &str, group: &str, name: &str, kind: &str) -> ChannelWire {
        ChannelWire {
            id: id.into(),
            group_id: group.into(),
            name: name.into(),
            description: None,
            channel_type: kind.into(),
            category_id: None,
        }
    }

    fn boot() -> DirectoryBootstrapResponse {
        DirectoryBootstrapResponse {
            groups: vec![GroupWithChannelsWire {
                group: GroupWire {
                    id: "g1".into(),
                    name: "Book club".into(),
                    description: None,
                    owner_id: "me".into(),
                    created_at: "2026-01-01T00:00:00Z".into(),
                },
                channels: vec![
                    channel("c-general", "g1", "general", "text"),
                    channel("c-voice", "g1", "lounge", "voice"),
                    channel("c-books", "g1", "books", "text"),
                ],
                role: "member".into(),
                categories: Vec::new(),
            }],
            dms: vec![
                dm("d-alice", "2026-02-01T00:00:00Z", vec![member("alice", "alice", Some("Alice Doe"))]),
                dm("d-bob", "2026-03-01T00:00:00Z", vec![member("bob", "bob", None)]),
                dm("d-mallory", "2026-04-01T00:00:00Z", vec![member("mallory", "mallory", None)]),
                dm(
                    "d-group",
                    "2026-01-15T00:00:00Z",
                    vec![member("alice", "alice", Some("Alice Doe")), member("carol", "carol", Some("Carol"))],
                ),
            ],
            dm_requests: vec![dm("d-request", "2026-05-01T00:00:00Z", vec![member("eve", "eve", None)])],
            pending_invites: Vec::new(),
            blocks: vec![BlockedUserWire {
                user_id: "mallory".into(),
                username: Some("mallory".into()),
                blocked_at: "2026-01-01T00:00:00Z".into(),
            }],
            preferences: None,
        }
    }

    fn ids(targets: &[ShareTarget]) -> Vec<&str> {
        targets.iter().map(|t| t.conversation_id.as_str()).collect()
    }

    #[test]
    fn requests_blocked_dms_and_voice_channels_are_never_targets() {
        let all = rank(eligible("me", &boot()), &HashMap::new(), None, 100);
        let got = ids(&all);
        assert!(!got.contains(&"d-request"), "an unaccepted request: {got:?}");
        assert!(!got.contains(&"d-mallory"), "a blocked DM: {got:?}");
        assert!(!got.contains(&"c-voice"), "a voice channel: {got:?}");
        for id in ["d-alice", "d-bob", "d-group", "c-general", "c-books"] {
            assert!(got.contains(&id), "{id} is a target: {got:?}");
        }
        assert_eq!(
            ineligible("me", &boot(), &["d-alice".into(), "d-request".into(), "c-voice".into(), "nope".into()]),
            vec!["d-request".to_string(), "c-voice".into(), "nope".into()]
        );
    }

    #[test]
    fn recent_activity_comes_first_then_dms_by_age_then_the_rest() {
        let latest = HashMap::from([
            ("c-books".to_string(), "2026-06-02T00:00:00+00:00".to_string()),
            ("d-alice".to_string(), "2026-06-01T00:00:00+00:00".to_string()),
        ]);
        let all = rank(eligible("me", &boot()), &latest, None, 100);
        assert_eq!(ids(&all)[..2], ["c-books", "d-alice"]);
        // No activity: DMs newest-made first, then channels (no creation time), by id.
        assert_eq!(ids(&all)[2..], ["d-bob", "d-group", "c-general"]);
        assert_eq!(all[1].last_activity_at.as_deref(), Some("2026-06-01T00:00:00+00:00"));
    }

    #[test]
    fn titles_name_people_and_channels() {
        let all = rank(eligible("me", &boot()), &HashMap::new(), None, 100);
        let by = |id: &str| all.iter().find(|t| t.conversation_id == id).unwrap().clone();
        let alice = by("d-alice");
        assert_eq!(alice.title, "Alice Doe");
        assert_eq!(alice.subtitle.as_deref(), Some("@alice"));
        assert_eq!(alice.avatar_user_id.as_deref(), Some("alice"));
        assert_eq!(by("d-bob").title, "bob");
        let group = by("d-group");
        assert_eq!(group.title, "Alice Doe, Carol");
        assert_eq!(group.subtitle, None);
        assert_eq!(group.avatar_user_id, None);
        let general = by("c-general");
        assert_eq!((general.kind, general.title.as_str()), (TargetKind::Channel, "general"));
        assert_eq!(general.subtitle.as_deref(), Some("Book club"));
        assert_eq!(general.group_id.as_deref(), Some("g1"));
    }

    #[test]
    fn a_query_matches_word_prefixes_of_every_name() {
        let find = |q: &str| ids(&rank(eligible("me", &boot()), &HashMap::new(), Some(q), 100))
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert_eq!(find("doe"), ["d-alice", "d-group"]);
        assert_eq!(find("ali do"), ["d-alice", "d-group"]);
        assert_eq!(find("@bob"), ["d-bob"]);
        assert_eq!(find("#gen"), ["c-general"]);
        assert_eq!(find("book"), ["c-books", "c-general"]);
        assert_eq!(find("BOOK club gen"), ["c-general"]);
        assert!(find("mallory").is_empty(), "a blocked DM never matches");
        assert!(find("lounge").is_empty(), "a voice channel never matches");
        assert_eq!(find("   ").len(), 5, "a blank query is no filter");
    }

    #[test]
    fn the_limit_is_bounded() {
        let all = rank(eligible("me", &boot()), &HashMap::new(), None, 2);
        assert_eq!(all.len(), 2);
        let one = rank(eligible("me", &boot()), &HashMap::new(), None, 0);
        assert_eq!(one.len(), 1, "a zero limit still returns the best row");
    }
}
