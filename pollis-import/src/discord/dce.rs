//! DiscordChatExporter JSON (`--format Json`): one file per channel or thread.
//!
//! The wire structs are deliberately lenient (every field defaulted, unknown
//! fields ignored) because DCE's output has grown fields across versions and a
//! years-old export must still parse. Anything structurally wrong in one file
//! becomes a warning, never a failed import of the others.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::model::{
    Attachment, Category, Channel, ChannelKind, Community, CustomEmoji, Export, Media, Message,
    MessageKind, Person, Reaction, Source,
};
use crate::snowflake;

#[derive(Debug, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct DceFile {
    guild: DceGuild,
    channel: DceChannel,
    messages: Vec<DceMessage>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct DceGuild {
    id: String,
    name: String,
    icon_url: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct DceChannel {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    category_id: Option<String>,
    category: Option<String>,
    name: String,
    topic: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct DceMessage {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    timestamp: String,
    timestamp_edited: Option<String>,
    is_pinned: bool,
    content: String,
    author: DceUser,
    attachments: Vec<DceAttachment>,
    embeds: Vec<serde_json::Value>,
    stickers: Vec<DceSticker>,
    reactions: Vec<DceReaction>,
    mentions: Vec<DceUser>,
    reference: Option<DceReference>,
    inline_emojis: Vec<DceEmoji>,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(default, rename_all = "camelCase")]
struct DceUser {
    id: String,
    name: String,
    nickname: Option<String>,
    is_bot: bool,
    avatar_url: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct DceAttachment {
    id: String,
    url: String,
    file_name: String,
    file_size_bytes: u64,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct DceSticker {
    name: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct DceReaction {
    emoji: DceEmoji,
    count: u64,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(default, rename_all = "camelCase")]
struct DceEmoji {
    /// Empty (or absent) for a Unicode emoji.
    id: Option<String>,
    name: String,
    is_animated: bool,
    image_url: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct DceReference {
    message_id: Option<String>,
}

/// Read every `*.json` export in `dir` (not recursive) into one [`Export`].
///
/// A directory holds one server. Which one is decided by the files themselves:
/// the guild that most server-channel files belong to. DM exports (DCE files
/// them under a pseudo-guild) never decide it. Files from any other guild, and
/// files that are not DCE exports, are skipped with a warning.
pub fn read_export_dir(dir: &Path) -> std::io::Result<Export> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("json")))
        .collect();
    paths.sort();

    let mut builder = Builder::default();
    let mut parsed: Vec<(PathBuf, DceFile)> = Vec::new();
    for path in paths {
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                builder.warn(format!("{}: could not read ({e})", path.display()));
                continue;
            }
        };
        match serde_json::from_slice::<DceFile>(&bytes) {
            Ok(f) if f.guild.id.is_empty() || f.channel.id.is_empty() => {
                builder.warn(format!("{}: missing guild or channel id, skipped", path.display()));
            }
            Ok(f) => parsed.push((path, f)),
            Err(e) => builder.warn(format!(
                "{}: not a DiscordChatExporter JSON export ({e})",
                path.display()
            )),
        }
    }

    let mut votes: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, f) in &parsed {
        if channel_kind(&f.channel.kind) != ChannelKind::Direct {
            *votes.entry(f.guild.id.as_str()).or_default() += 1;
        }
    }
    // Most files wins; ties go to the smaller id so the choice is stable.
    let chosen = votes
        .iter()
        .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
        .map(|(id, _)| id.to_string());
    if votes.len() > 1 {
        builder.warn(format!(
            "the folder holds channels from {} servers; importing the one with the most channels",
            votes.len()
        ));
    }

    for (path, file) in parsed {
        builder.add_file(&path, file, chosen.as_deref());
    }
    Ok(builder.finish())
}

#[derive(Default)]
struct Builder {
    community: Option<Community>,
    channels: Vec<Channel>,
    people: BTreeMap<String, Person>,
    emoji: BTreeMap<String, CustomEmoji>,
    warnings: Vec<String>,
}

impl Builder {
    fn warn(&mut self, warning: String) {
        self.warnings.push(warning);
    }

    fn add_file(&mut self, path: &Path, file: DceFile, chosen_guild: Option<&str>) {
        if chosen_guild != Some(file.guild.id.as_str()) {
            let what = if channel_kind(&file.channel.kind) == ChannelKind::Direct {
                "a direct-message export; only server channels are imported".to_string()
            } else {
                format!("from another server (\"{}\")", file.guild.name)
            };
            self.warn(format!("{}: {what}, skipped", path.display()));
            return;
        }
        let base = path.parent().unwrap_or(Path::new("."));

        if self.community.is_none() {
            self.community = Some(Community {
                id: file.guild.id.clone(),
                name: file.guild.name.clone(),
                icon: file.guild.icon_url.as_deref().and_then(|u| media(base, u)),
            });
        }

        if self.channels.iter().any(|c| c.id == file.channel.id) {
            self.warn(format!(
                "{}: channel \"{}\" was already read from another file; skipped",
                path.display(),
                file.channel.name
            ));
            return;
        }

        let kind = channel_kind(&file.channel.kind);
        // DCE puts a thread's parent channel where a channel's category goes.
        let (category, parent_id) = if kind == ChannelKind::Thread {
            (None, file.channel.category_id.clone())
        } else {
            let category = match (&file.channel.category_id, &file.channel.category) {
                (Some(id), Some(name)) if !id.is_empty() => Some(Category {
                    id: id.clone(),
                    name: name.clone(),
                }),
                _ => None,
            };
            (category, None)
        };

        let mut messages = Vec::with_capacity(file.messages.len());
        for m in file.messages {
            if let Some(msg) = self.message(base, path, m) {
                messages.push(msg);
            }
        }
        // Exports are oldest first already; sort anyway so a hand-merged file
        // cannot reorder history.
        messages.sort_by(|a, b| a.sent_at_ms.cmp(&b.sent_at_ms).then_with(|| a.id.cmp(&b.id)));

        self.channels.push(Channel {
            id: file.channel.id,
            name: file.channel.name,
            kind,
            topic: file.channel.topic.filter(|t| !t.trim().is_empty()),
            category,
            parent_id,
            messages,
            file: path.to_path_buf(),
        });
    }

    fn person(&mut self, base: &Path, user: &DceUser) {
        if user.id.is_empty() {
            return;
        }
        let entry = self.people.entry(user.id.clone()).or_insert_with(|| Person {
            id: user.id.clone(),
            username: user.name.clone(),
            nickname: None,
            is_bot: user.is_bot,
            avatar: None,
        });
        // Later messages win: the newest nickname and avatar are the ones the
        // group knows them by now.
        if let Some(nick) = user.nickname.as_ref().filter(|n| !n.is_empty()) {
            entry.nickname = Some(nick.clone());
        }
        if let Some(url) = &user.avatar_url {
            entry.avatar = media(base, url);
        }
    }

    fn custom_emoji(&mut self, base: &Path, emoji: &DceEmoji, uses: u64) -> Option<String> {
        let id = emoji.id.as_ref().filter(|id| !id.is_empty())?;
        let entry = self.emoji.entry(id.clone()).or_insert_with(|| CustomEmoji {
            id: id.clone(),
            name: emoji.name.clone(),
            animated: emoji.is_animated,
            image: emoji.image_url.as_deref().and_then(|u| media(base, u)),
            uses: 0,
        });
        entry.uses += uses;
        Some(id.clone())
    }

    fn message(&mut self, base: &Path, path: &Path, m: DceMessage) -> Option<Message> {
        let Some(sent_at_ms) = snowflake::unix_ms(&m.id) else {
            self.warn(format!("{}: message id \"{}\" is not a snowflake; skipped", path.display(), m.id));
            return None;
        };
        self.person(base, &m.author);
        for user in &m.mentions {
            self.person(base, user);
        }
        for emoji in &m.inline_emojis {
            self.custom_emoji(base, emoji, 1);
        }

        let mut reactions = Vec::with_capacity(m.reactions.len());
        for r in &m.reactions {
            let emoji_id = self.custom_emoji(base, &r.emoji, r.count);
            reactions.push(Reaction {
                emoji_id,
                emoji: r.emoji.name.clone(),
                count: r.count,
            });
        }

        let attachments = m
            .attachments
            .iter()
            .filter_map(|a| {
                media(base, &a.url).map(|media| Attachment {
                    id: a.id.clone(),
                    file_name: a.file_name.clone(),
                    size_bytes: a.file_size_bytes,
                    media,
                })
            })
            .collect();

        Some(Message {
            id: m.id,
            sent_at_ms,
            timestamp: m.timestamp,
            edited_at: m.timestamp_edited,
            kind: message_kind(&m.kind),
            author_id: m.author.id,
            content: m.content,
            reply_to: m.reference.and_then(|r| r.message_id).filter(|id| !id.is_empty()),
            pinned: m.is_pinned,
            attachments,
            reactions,
            mention_ids: m.mentions.into_iter().map(|u| u.id).collect(),
            embed_count: m.embeds.len(),
            stickers: m.stickers.into_iter().map(|s| s.name).collect(),
        })
    }

    fn finish(mut self) -> Export {
        let community = self.community.take().unwrap_or_else(|| {
            self.warnings.push("no DiscordChatExporter JSON files found".to_string());
            Community {
                id: String::new(),
                name: String::new(),
                icon: None,
            }
        });
        Export {
            source: Source::DiscordChatExporter,
            community,
            channels: self.channels,
            people: self.people,
            emoji: self.emoji,
            warnings: self.warnings,
        }
    }
}

/// A DCE media reference: an `http(s)` URL, or (with `--media`) a path relative
/// to the export file.
fn media(base: &Path, url: &str) -> Option<Media> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        return Some(Media::Remote(url.to_string()));
    }
    // DCE percent-encodes spaces and a few other characters in local paths.
    Some(Media::Local(base.join(percent_decode(url))))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn channel_kind(kind: &str) -> ChannelKind {
    match kind {
        "GuildTextChat" => ChannelKind::Text,
        "GuildVoiceChat" => ChannelKind::Voice,
        "GuildNews" | "GuildAnnouncement" => ChannelKind::Announcement,
        "GuildForum" => ChannelKind::Forum,
        "GuildStageVoice" => ChannelKind::Stage,
        "GuildNewsThread" | "GuildPublicThread" | "GuildPrivateThread" => ChannelKind::Thread,
        "DirectTextChat" | "DirectGroupTextChat" => ChannelKind::Direct,
        other => ChannelKind::Other(other.to_string()),
    }
}

fn message_kind(kind: &str) -> MessageKind {
    match kind {
        "" | "Default" | "Reply" => MessageKind::Normal,
        other => MessageKind::System(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_percent_escapes_in_local_paths() {
        assert_eq!(percent_decode("a%20b%2Fc.png"), "a b/c.png");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }

    #[test]
    fn classifies_media() {
        let base = Path::new("/export");
        assert_eq!(
            media(base, "https://cdn.discordapp.com/x.png"),
            Some(Media::Remote("https://cdn.discordapp.com/x.png".into()))
        );
        assert_eq!(
            media(base, "general_files/map%201.png"),
            Some(Media::Local(PathBuf::from("/export/general_files/map 1.png")))
        );
        assert_eq!(media(base, "  "), None);
    }
}
