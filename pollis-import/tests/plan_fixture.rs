//! Two fixtures.
//!
//! `tests/fixtures/dnd` is DiscordChatExporter files alone, exercising every
//! lossy path of a channel export: categories, threads (anchored and
//! orphaned), system notices, emoji that collide only by case, a
//! one-character emoji, media present, missing or only a dead CDN link, a bot
//! author, a DM export and a file that is not an export at all.
//!
//! `tests/fixtures/dragons-den` is the same server with a guild snapshot
//! (`discord-guild.json`) beside it: positions, empty categories, voice and
//! stage channels, a forum, roles with permissions, per-channel overwrites
//! (one private channel), members who never wrote, and emoji images.

use std::path::Path;

use pollis_import::discord::read_export_dir;
use pollis_import::model::Export;
use pollis_import::plan::{
    plan, ChannelNote, EmojiNote, ExistingChannel, ImportOptions, Note, Permission, RoleTarget,
    SkipReason, TargetKind,
};
use pollis_import::report;
use pollis_import::shortcode::shortcode_is_valid;

fn read(name: &str) -> Export {
    read_export_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)).unwrap()
}

fn dnd() -> Export {
    read("dnd")
}

fn den() -> Export {
    read("dragons-den")
}

fn layout(p: &pollis_import::plan::ImportPlan) -> Vec<String> {
    p.channels
        .iter()
        .map(|c| {
            let kind = if c.kind == TargetKind::Voice { " (voice)" } else { "" };
            match &c.category {
                Some(k) => format!("{k} / {}{kind}", c.name),
                None => format!("{}{kind}", c.name),
            }
        })
        .collect()
}

#[test]
fn the_server_is_chosen_by_its_channels_not_by_file_order() {
    // "Direct Messages - ..." sorts first; it must not become the group.
    let export = dnd();
    assert_eq!(export.community.name, "Dragon's Den");
    assert!(!export.has_snapshot);
    assert!(export.warnings.iter().any(|w| w.contains("direct-message export")));
    assert!(export.warnings.iter().any(|w| w.contains("broken.json")));
}

#[test]
fn channel_files_alone_map_their_categories_in_creation_order() {
    let p = plan(&dnd(), &ImportOptions::default());
    assert_eq!(
        layout(&p),
        vec!["tavern (voice)", "Campaign / session-notes", "Text Channels / general"],
        "uncategorised first; categories and channels by creation without positions"
    );
    let names: Vec<_> = p.categories.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["Campaign", "Text Channels"]);
    assert_eq!(p.channels[1].description.as_deref(), Some("Recaps after every session"));
    // An empty topic is not a description.
    assert_eq!(p.channels[2].description, None);
    assert!(p.notes.contains(&Note::NoSnapshot));
    assert!(!p.permissions_known);
}

#[test]
fn threads_anchor_to_their_starter_or_are_reported() {
    let p = plan(&dnd(), &ImportOptions::default());
    let notes = p.channels.iter().find(|c| c.name == "session-notes").unwrap();
    assert_eq!(notes.threads.len(), 1);
    assert_eq!(notes.threads[0].name, "Loot split");
    assert!(notes.threads[0].root_message_id.is_some());
    assert!(p
        .skipped_channels
        .iter()
        .any(|s| s.name == "Orphan" && matches!(s.reason, SkipReason::OrphanThread)));
}

#[test]
fn history_is_measured_but_not_imported() {
    let p = plan(&dnd(), &ImportOptions::default());
    let h = &p.channels.iter().find(|c| c.name == "session-notes").unwrap().history;
    assert_eq!(h.messages, 3);
    assert_eq!(h.system_messages, 1);
    assert_eq!(h.replies, 1);
    assert_eq!(h.pinned, 1);
    assert_eq!(h.edited, 1);
    assert_eq!(p.totals.messages, 8);
    assert!(p.notes.contains(&Note::HistoryNotImported { messages: 8 }));
    let m = &h.media;
    assert_eq!((m.files, m.local_present, m.local_missing.len(), m.remote_only), (3, 1, 1, 1));
}

#[test]
fn emoji_collisions_and_impossible_names_are_explicit() {
    let p = plan(&dnd(), &ImportOptions::default());
    let by_name = |n: &str| p.emoji.iter().find(|e| e.source_name == n).unwrap();
    // The more used one keeps the plain name.
    assert_eq!(by_name("Nat20").shortcode.as_deref(), Some("nat20"));
    assert_eq!(by_name("nat20").shortcode.as_deref(), Some("nat20_2"));
    assert!(matches!(by_name("nat20").note, Some(EmojiNote::Renamed { .. })));
    assert_eq!(by_name("x").shortcode, None);
    assert!(matches!(by_name("x").note, Some(EmojiNote::NoValidShortcode)));
    assert!(matches!(by_name("BeholderEye").note, Some(EmojiNote::ImageRemote)));
    for e in &p.emoji {
        if let Some(s) = &e.shortcode {
            assert!(shortcode_is_valid(s));
        }
    }
}

#[test]
fn an_existing_shortcode_in_the_group_forces_a_rename() {
    let options = ImportOptions { existing_shortcodes: vec!["nat20".into()], ..ImportOptions::default() };
    let p = plan(&dnd(), &options);
    let nat20 = p.emoji.iter().find(|e| e.source_name == "Nat20").unwrap();
    assert_eq!(nat20.shortcode.as_deref(), Some("nat20_2"));
    assert!(!nat20.exists);
}

#[test]
fn people_are_counted_only_in_imported_channels_and_bots_are_flagged() {
    let p = plan(&dnd(), &ImportOptions::default());
    let rob = p.people.iter().find(|x| x.username == "rob").unwrap();
    // His message in the skipped orphan thread does not count.
    assert_eq!(rob.messages, 2);
    assert!(!rob.admin, "no permissions without a snapshot");
    let dan = p.people.iter().find(|x| x.username == "dan").unwrap();
    assert_eq!(dan.display_name, "DM Dan");
    assert!(p.people.iter().find(|x| x.username == "Avrae").unwrap().is_bot);
    assert_eq!(p.totals.people, 3, "bots are not people to invite");
}

#[test]
fn the_snapshot_supplies_order_categories_and_unexported_channels() {
    let export = den();
    assert!(export.has_snapshot);
    assert!(export.warnings.is_empty(), "{:?}", export.warnings);
    let p = plan(&export, &ImportOptions::default());
    assert_eq!(
        layout(&p),
        vec![
            "rules",
            "Text Channels / general",
            "Text Channels / looking-for-group",
            // Text-like channels before voice, as Discord draws a category.
            "Campaign / session-notes",
            "Campaign / lore",
            "Campaign / maps",
            "Campaign / gm-only",
            "Campaign / the-table (voice)",
            "Voice Channels / tavern (voice)",
            "Voice Channels / storytime (voice)",
            "Off-topic / memes",
            "Off-topic / 3-fan-art",
            // Same name as Text Channels' general: allowed in another category.
            "Off-topic / general",
        ]
    );
    let cats: Vec<_> = p.categories.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(cats, vec!["Text Channels", "Campaign", "Voice Channels", "Off-topic", "Archive"]);
    let by = |n: &str| p.channels.iter().find(|c| c.source_name == n).unwrap();
    assert!(matches!(by("rules").notes[0], ChannelNote::Announcement));
    assert!(matches!(by("lore").notes[0], ChannelNote::Forum));
    assert!(matches!(by("storytime").notes[0], ChannelNote::Stage));
    assert!(matches!(&by("<3-fan-art").notes[0], ChannelNote::Renamed { from } if from == "<3-fan-art"));
    assert_eq!(by("rules").description.as_deref(), Some("Read before your first session"));
    assert!(!p.notes.contains(&Note::NoSnapshot));
    assert!(p.group.icon.is_some());
}

#[test]
fn overwrites_are_reported_and_a_private_channel_is_called_out() {
    let p = plan(&den(), &ImportOptions::default());
    let gm = p.channels.iter().find(|c| c.name == "gm-only").unwrap();
    assert!(gm.private);
    assert_eq!(gm.overwrites.len(), 2);
    assert_eq!(gm.overwrites[0].target, "@everyone");
    assert_eq!(gm.overwrites[0].deny, vec![Permission::View]);
    assert_eq!(gm.overwrites[1].target, "Dungeon Master");
    assert_eq!(gm.overwrites[1].allow, vec![Permission::View, Permission::SendMessages]);
    let memes = p.channels.iter().find(|c| c.name == "memes").unwrap();
    assert!(memes.overwrites[0].member);
    assert_eq!(memes.overwrites[0].target, "rob");
    assert!(!p.channels.iter().find(|c| c.name == "maps").unwrap().private, "a deny that is not view is not private");
    assert_eq!(p.totals.overwrites, 4);
    assert!(p.notes.contains(&Note::PrivateChannels { count: 1 }));
}

#[test]
fn roles_collapse_to_admin_and_member_and_every_role_is_reported() {
    let p = plan(&den(), &ImportOptions::default());
    assert!(p.permissions_known);
    let role = |n: &str| p.roles.iter().find(|r| r.name == n).unwrap();
    assert_eq!(role("Dungeon Master").maps_to, RoleTarget::Admin, "ADMINISTRATOR");
    assert_eq!(role("Moderator").maps_to, RoleTarget::Admin, "MANAGE_GUILD");
    assert_eq!(role("Players").maps_to, RoleTarget::Discarded);
    assert_eq!(role("Players").holders, 3);
    assert_eq!(role("Avrae").maps_to, RoleTarget::Bot);
    assert_eq!(role("@everyone").maps_to, RoleTarget::Member);
    assert!(p.notes.contains(&Note::RolesCollapsed { discarded: 2 }));

    let person = |n: &str| p.people.iter().find(|x| x.username == n).unwrap();
    assert!(person("dan").admin, "the owner");
    assert!(person("rob").admin, "a Moderator");
    assert!(!person("ashley").admin);
    // A member who never wrote is still on the invite plan.
    assert_eq!(person("kim").messages, 0);
    assert_eq!(person("kim").display_name, "Quiet Kim");
    assert_eq!(person("ashley").channels, vec!["Campaign / session-notes", "Voice Channels / tavern"]);
}

#[test]
fn snapshot_emoji_take_their_images_from_the_emoji_folder() {
    let p = plan(&den(), &ImportOptions::default());
    let by_name = |n: &str| p.emoji.iter().find(|e| e.source_name == n).unwrap();
    assert_eq!(by_name("Mimic").shortcode.as_deref(), Some("mimic"), "unused, but in the server's list");
    assert_eq!(by_name("crit-fail").shortcode.as_deref(), Some("crit_fail"));
    assert_eq!(by_name("nat20").shortcode.as_deref(), Some("nat20_2"));
    assert!(matches!(by_name("BeholderEye").note, Some(EmojiNote::ImageMissing)));
    assert_eq!(p.totals.to_upload_emoji, 4);
}

#[test]
fn a_rerun_against_the_result_creates_nothing() {
    let first = plan(&den(), &ImportOptions::default());
    let options = ImportOptions {
        existing_categories: first.categories.iter().map(|c| c.name.to_uppercase()).collect(),
        existing_channels: first
            .channels
            .iter()
            .map(|c| ExistingChannel { category: c.category.clone(), name: c.name.clone() })
            .collect(),
        existing_shortcodes: first.emoji.iter().filter_map(|e| e.shortcode.clone()).collect(),
        imported_emoji: first
            .emoji
            .iter()
            .filter_map(|e| Some((e.source_id.clone(), e.shortcode.clone()?)))
            .collect(),
        include_archived: false,
    };
    let again = plan(&den(), &options);
    assert_eq!(again.totals.to_create_categories, 0);
    assert_eq!(again.totals.to_create_channels, 0);
    assert_eq!(again.totals.to_upload_emoji, 0);
    // Kept names, not "nat20_3".
    let a: Vec<_> = first.emoji.iter().map(|e| e.shortcode.clone()).collect();
    let b: Vec<_> = again.emoji.iter().map(|e| e.shortcode.clone()).collect();
    assert_eq!(a, b);
}

#[test]
fn a_channel_matches_only_within_its_category() {
    let options = ImportOptions {
        existing_channels: vec![ExistingChannel { category: Some("text channels".into()), name: "GENERAL".into() }],
        ..ImportOptions::default()
    };
    let p = plan(&den(), &options);
    let generals: Vec<_> = p.channels.iter().filter(|c| c.name == "general").map(|c| c.exists).collect();
    assert_eq!(generals, vec![true, false]);
}

#[test]
fn the_invite_sheet_lists_people_not_bots() {
    let sheet = report::invite_sheet(&plan(&den(), &ImportOptions::default()));
    let lines: Vec<_> = sheet.lines().collect();
    assert_eq!(lines[0], "username,display_name,email,pollis_role,discord_roles,channels,messages");
    assert_eq!(lines.len(), 5);
    assert!(lines.iter().any(|l| l.starts_with("dan,DM Dan,,admin,")));
    assert!(!sheet.contains("Avrae"));
}

#[test]
fn the_plan_is_deterministic() {
    let a = serde_json::to_string(&plan(&den(), &ImportOptions::default())).unwrap();
    let b = serde_json::to_string(&plan(&den(), &ImportOptions::default())).unwrap();
    assert_eq!(a, b);
}

#[test]
fn every_planned_name_is_valid_for_pollis() {
    let p = plan(&den(), &ImportOptions::default());
    assert!(pollis_api::profile::is_valid_display_name(&p.group.name));
    for k in &p.categories {
        assert!(pollis_api::profile::is_valid_display_name(&k.name), "{}", k.name);
    }
    for c in &p.channels {
        assert!(pollis_api::profile::is_valid_display_name(&c.name), "{}", c.name);
    }
}
