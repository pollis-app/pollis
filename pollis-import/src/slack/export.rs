//! The standard workspace export: `channels.json`, `groups.json`,
//! `users.json`, the DM lists and each channel's per-day message files.
//!
//! Wire structs are lenient (every field defaulted, unknown fields ignored),
//! as Slack's export format has changed over the years; a bad file is a
//! warning, never a failed import.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use super::archive::{Archive, JSON_MAX_BYTES};
use super::snapshot::{self, Snapshot};
use crate::model::{
    Attachment, Channel, ChannelKind, Community, CustomEmoji, Export, Media, Message, MessageKind,
    Person, Reaction, Source, UserGroup,
};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SlackChannel {
    id: String,
    name: String,
    is_archived: bool,
    is_general: bool,
    is_private: bool,
    members: Vec<String>,
    topic: TextField,
    purpose: TextField,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct TextField {
    value: String,
}

/// An entry of `dms.json` or `mpims.json`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SlackDirect {
    id: String,
    name: String,
    members: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SlackUser {
    id: String,
    team_id: String,
    name: String,
    deleted: bool,
    real_name: String,
    is_bot: bool,
    is_app_user: bool,
    is_admin: bool,
    is_owner: bool,
    is_primary_owner: bool,
    profile: SlackProfile,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SlackProfile {
    display_name: String,
    real_name: String,
    email: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SlackMessage {
    ts: String,
    user: Option<String>,
    bot_id: Option<String>,
    subtype: Option<String>,
    text: String,
    thread_ts: Option<String>,
    edited: Option<serde_json::Value>,
    pinned_to: Vec<String>,
    reactions: Vec<SlackReaction>,
    files: Vec<SlackFile>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SlackReaction {
    name: String,
    count: u64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SlackFile {
    id: String,
    name: String,
    size: u64,
    url_private: String,
}

/// Subtypes that are something a person (or bot) wrote; every other
/// subtype is a platform notice (joins, renames, pins).
const CONTENT_SUBTYPES: [&str; 5] = ["thread_broadcast", "me_message", "file_share", "bot_message", "file_comment"];

/// Slack's own bot user, listed in `users.json` as a person.
const SLACKBOT: &str = "USLACKBOT";

pub fn read(
    archive: &mut Archive,
    snapshot: Result<Option<Snapshot>, String>,
    name_from_path: &str,
) -> std::io::Result<Export> {
    let mut warnings = Vec::new();
    let snapshot = snapshot.unwrap_or_else(|w| {
        warnings.push(w);
        None
    });
    let names = archive.names();

    let users: Vec<SlackUser> = read_list(archive, "users.json", &mut warnings);
    let mut channels: Vec<(SlackChannel, bool)> = Vec::new();
    for c in read_list::<SlackChannel>(archive, "channels.json", &mut warnings) {
        let private = c.is_private;
        channels.push((c, private));
    }
    for c in read_list::<SlackChannel>(archive, "groups.json", &mut warnings) {
        channels.push((c, true));
    }
    channels.retain(|(c, _)| !c.id.is_empty() && !c.name.is_empty());

    // The workspace: the snapshot's team, else the users' team and the download's name.
    let mut team_votes: BTreeMap<&str, usize> = BTreeMap::new();
    for u in &users {
        if !u.team_id.is_empty() {
            *team_votes.entry(u.team_id.as_str()).or_default() += 1;
        }
    }
    let team_id = snapshot
        .as_ref()
        .map(|s| s.team.id.clone())
        .filter(|id| !id.is_empty())
        .or_else(|| team_votes.iter().max_by_key(|(_, n)| **n).map(|(id, _)| id.to_string()))
        .unwrap_or_else(|| "slack".to_string());
    let team_name = snapshot
        .as_ref()
        .map(|s| s.team.name.clone())
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| name_from_path.to_string());
    let icon = match snapshot::image_named(&names, "icon").cloned() {
        Some(n) if snapshot.is_some() => local(archive, &n, &mut warnings),
        _ => None,
    };

    // Emoji first, so messages can count their uses.
    let mut emoji: BTreeMap<String, CustomEmoji> = BTreeMap::new();
    if let Some(s) = &snapshot {
        for (name, value) in &s.emoji {
            if let Some(target) = value.strip_prefix("alias:") {
                emoji.insert(
                    name.clone(),
                    CustomEmoji {
                        id: name.clone(),
                        name: name.clone(),
                        animated: false,
                        image: None,
                        uses: 0,
                        alias_of: Some(target.to_string()),
                    },
                );
                continue;
            }
            let file = snapshot::image_named(&names, &format!("emoji/{name}")).cloned();
            let image = match file {
                Some(f) => local(archive, &f, &mut warnings),
                None => (!value.is_empty()).then(|| Media::Remote(value.clone())),
            };
            let animated = match &image {
                Some(Media::Local(p)) => p.extension().is_some_and(|e| e.eq_ignore_ascii_case("gif")),
                _ => value.to_ascii_lowercase().ends_with(".gif"),
            };
            emoji.insert(
                name.clone(),
                CustomEmoji { id: name.clone(), name: name.clone(), animated, image, uses: 0, alias_of: None },
            );
        }
        // An alias shows its target's picture.
        let targets: BTreeMap<String, (Option<Media>, bool)> = emoji
            .values()
            .filter(|e| e.alias_of.is_none())
            .map(|e| (e.id.clone(), (e.image.clone(), e.animated)))
            .collect();
        for e in emoji.values_mut() {
            if let Some((image, animated)) = e.alias_of.as_ref().and_then(|t| targets.get(t)) {
                e.image = image.clone();
                e.animated = *animated;
            }
        }
    }

    let mut people: BTreeMap<String, Person> = BTreeMap::new();
    let mut owner_id = None;
    for u in users {
        if u.id.is_empty() {
            continue;
        }
        if u.is_primary_owner && !u.deleted {
            owner_id = Some(u.id.clone());
        }
        let nickname = [&u.profile.display_name, &u.profile.real_name, &u.real_name]
            .into_iter()
            .map(|n| n.trim())
            .find(|n| !n.is_empty())
            .map(str::to_string);
        let email = Some(u.profile.email.trim().to_string()).filter(|e| e.contains('@'));
        people.insert(
            u.id.clone(),
            Person {
                id: u.id.clone(),
                username: if u.name.is_empty() { u.id.clone() } else { u.name.clone() },
                nickname,
                is_bot: u.is_bot || u.is_app_user || u.id == SLACKBOT,
                avatar: None,
                role_ids: Vec::new(),
                listed_member: !u.deleted,
                email,
                source_admin: u.is_admin || u.is_owner || u.is_primary_owner,
                deactivated: u.deleted,
            },
        );
    }

    // Slack's sidebar is alphabetical; the default channel goes first.
    channels.sort_by(|(a, _), (b, _)| b.is_general.cmp(&a.is_general).then_with(|| a.name.cmp(&b.name)));
    let mut out_channels = Vec::new();
    for (i, (c, private)) in channels.into_iter().enumerate() {
        let messages = read_days(archive, &names, &c.name, &mut emoji, &mut warnings);
        let topic = [c.purpose.value.trim(), c.topic.value.trim()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>();
        // Purpose says what the channel is for; a different topic is what's on now.
        let description = match topic.as_slice() {
            [] => None,
            [one] => Some(one.to_string()),
            [purpose, topic, ..] if purpose == topic => Some(purpose.to_string()),
            [purpose, topic, ..] => Some(format!("{purpose}\n{topic}")),
        };
        out_channels.push(Channel {
            id: c.id,
            name: c.name,
            kind: ChannelKind::Text,
            topic: description,
            category: None,
            parent_id: None,
            position: Some(i as i64),
            overwrites: Vec::new(),
            private,
            archived: c.is_archived,
            members: c.members,
            messages,
            file: None,
        });
    }

    // DMs are never imported; listed so the report says so.
    for (file, label) in [("dms.json", "Direct message"), ("mpims.json", "Group DM")] {
        for d in read_list::<SlackDirect>(archive, file, &mut warnings) {
            let who: Vec<&str> = d
                .members
                .iter()
                .map(|id| people.get(id).map_or(id.as_str(), |p| p.username.as_str()))
                .collect();
            let name = if who.is_empty() { d.name.clone() } else { format!("{label}: {}", who.join(", ")) };
            out_channels.push(Channel {
                id: d.id,
                name,
                kind: ChannelKind::Direct,
                topic: None,
                category: None,
                parent_id: None,
                position: None,
                overwrites: Vec::new(),
                private: true,
                archived: false,
                members: Vec::new(),
                messages: Vec::new(),
                file: None,
            });
        }
    }

    let user_groups = snapshot
        .as_ref()
        .map(|s| {
            s.usergroups
                .iter()
                .filter(|g| g.date_delete == 0 && !g.id.is_empty())
                .map(|g| UserGroup {
                    id: g.id.clone(),
                    handle: g.handle.clone(),
                    name: g.name.clone(),
                    description: Some(g.description.trim().to_string()).filter(|d| !d.is_empty()),
                    member_ids: g.users.clone(),
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(Export {
        source: Source::SlackExport,
        community: Community { id: team_id, name: team_name, icon },
        channels: out_channels,
        categories: Vec::new(),
        people,
        emoji,
        roles: Vec::new(),
        owner_id,
        user_groups,
        has_snapshot: snapshot.is_some(),
        warnings,
    })
}

fn read_list<T: for<'de> Deserialize<'de>>(archive: &mut Archive, name: &str, warnings: &mut Vec<String>) -> Vec<T> {
    match archive.read(name, JSON_MAX_BYTES) {
        Ok(None) => Vec::new(),
        Ok(Some(bytes)) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            warnings.push(format!("{name}: not a Slack export file ({e})"));
            Vec::new()
        }),
        Err(e) => {
            warnings.push(format!("{name}: could not read ({e})"));
            Vec::new()
        }
    }
}

fn local(archive: &mut Archive, name: &str, warnings: &mut Vec<String>) -> Option<Media> {
    match archive.local_file(name) {
        Ok(p) => p.map(Media::Local),
        Err(e) => {
            warnings.push(format!("{name}: could not read ({e})"));
            None
        }
    }
}

/// A channel's messages, from `<channel>/<YYYY-MM-DD>.json`, oldest first.
/// Measured only: the text is scanned for custom emoji uses and dropped.
fn read_days(
    archive: &mut Archive,
    names: &[String],
    channel: &str,
    emoji: &mut BTreeMap<String, CustomEmoji>,
    warnings: &mut Vec<String>,
) -> Vec<Message> {
    let prefix = format!("{channel}/");
    let days: Vec<String> = names
        .iter()
        .filter(|n| n.strip_prefix(&prefix).is_some_and(|rest| !rest.contains('/') && rest.ends_with(".json")))
        .cloned()
        .collect();
    let mut out = Vec::new();
    for day in days {
        for m in read_list::<SlackMessage>(archive, &day, warnings) {
            let Some(sent_at_ms) = ts_ms(&m.ts) else {
                continue;
            };
            let content = m.subtype.as_deref().is_none_or(|s| CONTENT_SUBTYPES.contains(&s));
            let mut reactions = Vec::new();
            for r in &m.reactions {
                let custom = emoji.get_mut(&r.name);
                let is_custom = custom.is_some();
                if let Some(e) = custom {
                    e.uses += r.count;
                }
                reactions.push(Reaction {
                    emoji_id: is_custom.then(|| r.name.clone()),
                    emoji: r.name.clone(),
                    count: r.count,
                });
            }
            for name in shortcodes(&m.text) {
                if let Some(e) = emoji.get_mut(name) {
                    e.uses += 1;
                }
            }
            out.push(Message {
                id: m.ts.clone(),
                sent_at_ms,
                timestamp: m.ts.clone(),
                edited_at: m.edited.as_ref().map(|_| String::new()),
                kind: if content {
                    MessageKind::Normal
                } else {
                    MessageKind::System(m.subtype.clone().unwrap_or_default())
                },
                author_id: m.user.clone().or(m.bot_id.clone()).unwrap_or_default(),
                content: String::new(),
                reply_to: m.thread_ts.clone().filter(|t| *t != m.ts),
                pinned: !m.pinned_to.is_empty(),
                attachments: m
                    .files
                    .iter()
                    .map(|f| Attachment {
                        id: f.id.clone(),
                        file_name: f.name.clone(),
                        size_bytes: f.size,
                        media: Media::Remote(f.url_private.clone()),
                    })
                    .collect(),
                reactions,
                mention_ids: Vec::new(),
                embed_count: 0,
                stickers: Vec::new(),
            });
        }
    }
    out.sort_by(|a, b| a.sent_at_ms.cmp(&b.sent_at_ms).then_with(|| a.id.cmp(&b.id)));
    out
}

/// `"1673000000.000100"` → Unix milliseconds.
fn ts_ms(ts: &str) -> Option<u64> {
    let (secs, frac) = ts.split_once('.').unwrap_or((ts, "0"));
    let secs: u64 = secs.parse().ok()?;
    let millis: u64 = frac.get(..3).unwrap_or(frac).parse().unwrap_or(0);
    Some(secs * 1000 + millis)
}

/// `:name:` runs in message text (Slack's emoji syntax).
fn shortcodes(text: &str) -> BTreeSet<&str> {
    let mut out = BTreeSet::new();
    let mut parts = text.split(':');
    // Text before the first colon is never a name.
    parts.next();
    let mut prev_was_name = false;
    for part in parts {
        let is_name = !prev_was_name
            && !part.is_empty()
            && part.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_+-'".contains(c));
        if is_name {
            out.insert(part);
        }
        prev_was_name = is_name;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slack_timestamps_are_seconds_with_microseconds() {
        assert_eq!(ts_ms("1673000000.000100"), Some(1_673_000_000_000));
        assert_eq!(ts_ms("1673000000.123456"), Some(1_673_000_000_123));
        assert_eq!(ts_ms("nope"), None);
    }

    #[test]
    fn finds_emoji_names_in_text() {
        let found: Vec<_> = shortcodes("ship it :shipit: :party-parrot::skin-tone-2: at 10:30").into_iter().collect();
        assert!(found.contains(&"shipit"));
        assert!(found.contains(&"party-parrot"));
    }
}
