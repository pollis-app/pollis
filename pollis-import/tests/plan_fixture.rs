//! The D&D-shaped fixture in `tests/fixtures/dnd` exercises every lossy path
//! the planner has to report: categories, threads (anchored and orphaned),
//! replies, pins, edits, system notices, emoji that collide only by case, a
//! one-character emoji, media that is present, missing, or only a dead CDN
//! link, a bot author, a DM export and a file that is not an export at all.

use std::path::Path;

use pollis_import::discord::read_export_dir;
use pollis_import::plan::{plan, CategoryStrategy, ImportOptions, TargetKind};
use pollis_import::shortcode::shortcode_is_valid;

fn fixture() -> pollis_import::model::Export {
    read_export_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dnd")).unwrap()
}

#[test]
fn the_server_is_chosen_by_its_channels_not_by_file_order() {
    // "Direct Messages - ..." sorts first; it must not become the group.
    let export = fixture();
    assert_eq!(export.community.name, "Dragon's Den");
    assert!(export.warnings.iter().any(|w| w.contains("direct-message export")));
    assert!(export.warnings.iter().any(|w| w.contains("broken.json")));
}

#[test]
fn structure_maps_with_categories_as_prefixes() {
    let p = plan(&fixture(), &ImportOptions::default());
    let names: Vec<_> = p.channels.iter().map(|c| (c.name.as_str(), c.kind)).collect();
    assert_eq!(
        names,
        vec![
            ("Campaign / session-notes", TargetKind::Text),
            // Discord's default "Text Channels" category adds nothing.
            ("general", TargetKind::Text),
            ("tavern", TargetKind::Voice),
        ]
    );
    let notes = &p.channels[0];
    assert_eq!(notes.description.as_deref(), Some("Recaps after every session"));
    // An empty topic is not a description.
    assert_eq!(p.channels[1].description, None);
}

#[test]
fn dropping_categories_keeps_bare_names() {
    let options = ImportOptions { categories: CategoryStrategy::Drop, ..ImportOptions::default() };
    let p = plan(&fixture(), &options);
    assert_eq!(p.channels[0].name, "session-notes");
}

#[test]
fn threads_anchor_to_their_starter_or_are_reported() {
    let p = plan(&fixture(), &ImportOptions::default());
    let threads = &p.channels[0].threads;
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].name, "Loot split");
    assert!(threads[0].root_message_id.is_some(), "the starter message is in the parent export");
    assert!(p.skipped_channels.iter().any(|s| s.name == "Orphan"));
}

#[test]
fn history_is_measured_and_system_notices_are_not_messages() {
    let p = plan(&fixture(), &ImportOptions::default());
    let h = &p.channels[0].history;
    assert_eq!(h.messages, 3);
    assert_eq!(h.system_messages, 1);
    assert_eq!(h.replies, 1);
    assert_eq!(h.pinned, 1);
    assert_eq!(h.edited, 1);
    assert_eq!(p.totals.messages, 8);
}

#[test]
fn media_is_classified_present_missing_or_remote() {
    let p = plan(&fixture(), &ImportOptions::default());
    let m = &p.channels[0].history.media;
    assert_eq!(m.files, 3);
    assert_eq!(m.local_present, 1, "the percent-encoded \"map one.png\" resolves");
    assert_eq!(m.local_missing.len(), 1);
    assert_eq!(m.remote_only, 1);
    assert_eq!(p.totals.media_bytes, 2048 + 4096 + 8192);
}

#[test]
fn emoji_collisions_and_impossible_names_are_explicit() {
    let p = plan(&fixture(), &ImportOptions::default());
    let by_name = |n: &str| p.emoji.iter().find(|e| e.source_name == n).unwrap();
    // The more used one keeps the plain name.
    assert_eq!(by_name("Nat20").shortcode.as_deref(), Some("nat20"));
    assert_eq!(by_name("nat20").shortcode.as_deref(), Some("nat20_2"));
    assert!(by_name("nat20").note.is_some());
    assert_eq!(by_name("x").shortcode, None);
    assert_eq!(by_name("BeholderEye").shortcode, None, "no image to upload");
    for e in &p.emoji {
        if let Some(s) = &e.shortcode {
            assert!(shortcode_is_valid(s));
        }
    }
}

#[test]
fn an_existing_shortcode_in_the_group_forces_a_rename() {
    let options = ImportOptions { existing_shortcodes: vec!["nat20".into()], ..ImportOptions::default() };
    let p = plan(&fixture(), &options);
    let nat20 = p.emoji.iter().find(|e| e.source_name == "Nat20").unwrap();
    assert_eq!(nat20.shortcode.as_deref(), Some("nat20_2"));
}

#[test]
fn a_rerun_marks_existing_channels() {
    let options = ImportOptions { existing_channels: vec!["GENERAL".into()], ..ImportOptions::default() };
    let p = plan(&fixture(), &options);
    assert!(p.channels.iter().find(|c| c.name == "general").unwrap().exists);
    assert!(!p.channels.iter().find(|c| c.name == "tavern").unwrap().exists);
}

#[test]
fn people_are_counted_only_in_imported_channels_and_bots_are_flagged() {
    let p = plan(&fixture(), &ImportOptions::default());
    let rob = p.people.iter().find(|x| x.username == "rob").unwrap();
    // His message in the skipped orphan thread does not count.
    assert_eq!(rob.messages, 2);
    let dan = p.people.iter().find(|x| x.username == "dan").unwrap();
    assert_eq!(dan.display_name, "DM Dan");
    assert!(p.people.iter().find(|x| x.username == "Avrae").unwrap().is_bot);
    assert_eq!(p.totals.people, 3, "bots are not people to invite");
}

#[test]
fn the_plan_is_deterministic() {
    let a = serde_json::to_string(&plan(&fixture(), &ImportOptions::default())).unwrap();
    let b = serde_json::to_string(&plan(&fixture(), &ImportOptions::default())).unwrap();
    assert_eq!(a, b);
}
