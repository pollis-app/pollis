//! A guild snapshot: the server's structure as Discord's REST API returns it.
//!
//! A DiscordChatExporter export is per channel and carries no channel
//! positions, no empty categories, no role permissions, no per-channel
//! overwrites and no emoji list. Those live in three API responses, which a
//! snapshot holds verbatim:
//!
//! ```text
//! discord-guild.json
//! {
//!   "guild":    GET /guilds/{id}            (name, icon, owner_id, roles, emojis)
//!   "channels": GET /guilds/{id}/channels   (type, position, parent_id, topic, permission_overwrites)
//!   "members":  GET /guilds/{id}/members    (optional; needs the GUILD_MEMBERS intent)
//! }
//! emoji/<emoji id>.png|gif|webp            (optional; the emoji images)
//! icon.png|jpg|gif|webp                    (optional; the server icon)
//! ```
//!
//! Put beside the DCE files, it is merged into the [`Export`]: the snapshot
//! decides structure, the channel files supply history. A snapshot alone
//! (no channel files) is a complete structure import with no history. It is
//! the same shape a direct bot-token reader would write, so that reader only
//! has to fetch, never to plan.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer};

use crate::model::{
    Category, Channel, ChannelKind, Community, CustomEmoji, Export, Media, Overwrite,
    OverwriteTarget, Person, Role,
};

/// The snapshot's file name inside an export folder.
pub const SNAPSHOT_FILE: &str = "discord-guild.json";

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct Snapshot {
    guild: ApiGuild,
    channels: Vec<ApiChannel>,
    members: Vec<ApiMember>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct ApiGuild {
    id: String,
    name: String,
    owner_id: Option<String>,
    roles: Vec<ApiRole>,
    emojis: Vec<ApiEmoji>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct ApiRole {
    id: String,
    name: String,
    /// A decimal string in API v8+ (a number before); both parse.
    #[serde(deserialize_with = "bits")]
    permissions: u64,
    position: i64,
    managed: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct ApiEmoji {
    id: Option<String>,
    name: Option<String>,
    animated: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct ApiChannel {
    id: String,
    #[serde(rename = "type")]
    kind: i64,
    name: String,
    topic: Option<String>,
    position: i64,
    parent_id: Option<String>,
    permission_overwrites: Vec<ApiOverwrite>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct ApiOverwrite {
    id: String,
    /// 0 = role, 1 = member.
    #[serde(rename = "type")]
    kind: i64,
    #[serde(deserialize_with = "bits")]
    allow: u64,
    #[serde(deserialize_with = "bits")]
    deny: u64,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct ApiMember {
    user: ApiUser,
    nick: Option<String>,
    roles: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct ApiUser {
    id: String,
    username: String,
    global_name: Option<String>,
    bot: bool,
}

fn bits<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Bits {
        S(String),
        N(u64),
    }
    Ok(match Option::<Bits>::deserialize(d)? {
        Some(Bits::S(s)) => s.parse().unwrap_or(0),
        Some(Bits::N(n)) => n,
        None => 0,
    })
}

const CATEGORY: i64 = 4;

fn channel_kind(t: i64) -> Option<ChannelKind> {
    Some(match t {
        0 => ChannelKind::Text,
        2 => ChannelKind::Voice,
        5 => ChannelKind::Announcement,
        13 => ChannelKind::Stage,
        // Forum and media channels hold posts; both land as text.
        15 | 16 => ChannelKind::Forum,
        10..=12 => ChannelKind::Thread,
        CATEGORY => return None,
        other => ChannelKind::Other(format!("type {other}")),
    })
}

/// Merge `dir/discord-guild.json`, if there is one, into `export`.
pub fn merge_from_dir(dir: &Path, export: &mut Export) {
    let path = dir.join(SNAPSHOT_FILE);
    if !path.is_file() {
        return;
    }
    let snapshot = match fs::read(&path).map_err(|e| e.to_string()).and_then(|b| {
        serde_json::from_slice::<Snapshot>(&b).map_err(|e| e.to_string())
    }) {
        Ok(s) => s,
        Err(e) => {
            export.warnings.push(format!("{}: not a guild snapshot ({e}); ignored", path.display()));
            return;
        }
    };
    if snapshot.guild.id.is_empty() {
        export.warnings.push(format!("{}: no guild id; ignored", path.display()));
        return;
    }
    if !export.community.id.is_empty() && export.community.id != snapshot.guild.id {
        export.warnings.push(format!(
            "{}: describes another server (\"{}\") than the channel files; ignored",
            path.display(),
            snapshot.guild.name
        ));
        return;
    }
    merge(dir, snapshot, export);
}

fn merge(dir: &Path, s: Snapshot, export: &mut Export) {
    // The channel files' warning about finding none is moot with a snapshot.
    export.warnings.retain(|w| w != "no DiscordChatExporter JSON files found");
    export.has_snapshot = true;
    export.owner_id = s.guild.owner_id.clone();
    if export.community.id.is_empty() {
        export.community = Community {
            id: s.guild.id.clone(),
            name: s.guild.name.clone(),
            icon: None,
        };
    }
    let icon_present = matches!(&export.community.icon, Some(Media::Local(p)) if p.is_file());
    if !icon_present {
        if let Some(icon) = first_existing(dir, "icon", &["png", "jpg", "jpeg", "gif", "webp"]) {
            export.community.icon = Some(Media::Local(icon));
        }
    }

    // Categories, every one including the empty.
    let categories: BTreeMap<&str, Category> = s
        .channels
        .iter()
        .filter(|c| c.kind == CATEGORY)
        .map(|c| {
            (
                c.id.as_str(),
                Category {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    position: Some(c.position),
                },
            )
        })
        .collect();
    export.categories = categories.values().cloned().collect();

    for api in s.channels.iter().filter(|c| c.kind != CATEGORY) {
        let Some(kind) = channel_kind(api.kind) else {
            continue;
        };
        let category = api.parent_id.as_deref().and_then(|p| categories.get(p)).cloned();
        let overwrites = api
            .permission_overwrites
            .iter()
            .map(|o| Overwrite {
                target: if o.kind == 1 {
                    OverwriteTarget::Member(o.id.clone())
                } else {
                    OverwriteTarget::Role(o.id.clone())
                },
                allow: o.allow,
                deny: o.deny,
            })
            .collect();
        let topic = api.topic.clone().filter(|t| !t.trim().is_empty());
        if let Some(ch) = export.channels.iter_mut().find(|c| c.id == api.id) {
            ch.position = Some(api.position);
            ch.overwrites = overwrites;
            if ch.kind != ChannelKind::Thread {
                ch.category = category;
            }
            if ch.topic.is_none() {
                ch.topic = topic;
            }
            continue;
        }
        export.channels.push(Channel {
            id: api.id.clone(),
            name: api.name.clone(),
            kind,
            topic,
            category,
            parent_id: None,
            position: Some(api.position),
            overwrites,
            private: false,
            archived: false,
            members: Vec::new(),
            messages: Vec::new(),
            file: None,
        });
    }

    export.roles = s
        .guild
        .roles
        .iter()
        .map(|r| Role {
            id: r.id.clone(),
            name: r.name.clone(),
            permissions: Some(r.permissions),
            position: r.position,
            managed: r.managed,
            everyone: r.id == s.guild.id,
        })
        .collect();

    for e in &s.guild.emojis {
        let (Some(id), Some(name)) = (e.id.as_ref(), e.name.as_ref()) else {
            continue;
        };
        let local = first_existing(&dir.join("emoji"), id, &["png", "gif", "webp"]).map(Media::Local);
        let entry = export.emoji.entry(id.clone()).or_insert_with(|| CustomEmoji {
            id: id.clone(),
            name: name.clone(),
            animated: e.animated,
            image: None,
            uses: 0,
            alias_of: None,
        });
        let image_present = matches!(&entry.image, Some(Media::Local(p)) if p.is_file());
        if !image_present && local.is_some() {
            entry.image = local;
        }
    }

    for m in &s.members {
        if m.user.id.is_empty() {
            continue;
        }
        let person = export.people.entry(m.user.id.clone()).or_insert_with(|| Person {
            id: m.user.id.clone(),
            username: m.user.username.clone(),
            nickname: None,
            is_bot: m.user.bot,
            avatar: None,
            role_ids: Vec::new(),
            listed_member: false,
            email: None,
            source_admin: false,
            deactivated: false,
        });
        person.listed_member = true;
        person.role_ids = m.roles.clone();
        if let Some(nick) = m.nick.clone().or_else(|| m.user.global_name.clone()).filter(|n| !n.is_empty()) {
            person.nickname = Some(nick);
        }
    }
}

fn first_existing(dir: &Path, stem: &str, exts: &[&str]) -> Option<PathBuf> {
    exts.iter().map(|ext| dir.join(format!("{stem}.{ext}"))).find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_bits_parse_as_string_or_number() {
        let r: ApiRole = serde_json::from_str(r#"{"id":"1","permissions":"40"}"#).unwrap();
        assert_eq!(r.permissions, 40);
        let r: ApiRole = serde_json::from_str(r#"{"id":"1","permissions":8}"#).unwrap();
        assert_eq!(r.permissions, 8);
        let r: ApiRole = serde_json::from_str(r#"{"id":"1","permissions":null}"#).unwrap();
        assert_eq!(r.permissions, 0);
    }
}
