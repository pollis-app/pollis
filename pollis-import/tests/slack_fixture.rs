//! Slack (#1179) over `tests/fixtures/northwind`: a workspace export in
//! Slack's own shapes with a workspace snapshot. See `gen_northwind.py` for
//! what it holds. The same export is also read as the ZIP an admin
//! downloads (flat, and wrapped in a folder), and without its snapshot.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use pollis_import::model::{Export, Media};
use pollis_import::plan::{
    plan, ChannelNote, EmojiNote, ExistingChannel, ImportOptions, ImportPlan, Note, SkipReason,
};
use pollis_import::{read_export, report, SourceKind};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/northwind")
}

fn northwind() -> Export {
    read_export(SourceKind::Slack, &fixture()).expect("read the fixture")
}

fn names(p: &ImportPlan) -> Vec<&str> {
    p.channels.iter().map(|c| c.name.as_str()).collect()
}

/// A scratch dir unique to the test.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pollis-import-test-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Zip the fixture as Slack does (`prefix` empty) or wrapped in a folder.
fn zip_fixture(out: &Path, prefix: &str, skip: &[&str], extra: &[(&str, &[u8])]) {
    let mut zip = zip::ZipWriter::new(File::create(out).unwrap());
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let root = fixture();
    let mut files = Vec::new();
    collect(&root, &root, &mut files);
    files.sort();
    for rel in files {
        if skip.iter().any(|s| rel == *s || rel.starts_with(&format!("{s}/"))) {
            continue;
        }
        zip.start_file(format!("{prefix}{rel}"), opts).unwrap();
        zip.write_all(&fs::read(root.join(&rel)).unwrap()).unwrap();
    }
    for (name, bytes) in extra {
        zip.start_file(*name, opts).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<String>) {
    for e in fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(root, &p, out);
        } else {
            out.push(p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"));
        }
    }
}

#[test]
fn a_workspace_becomes_one_group_with_its_channels_in_slack_order() {
    let export = northwind();
    assert!(export.has_snapshot);
    assert!(export.warnings.is_empty(), "{:?}", export.warnings);
    let p = plan(&export, &ImportOptions::default());
    assert_eq!(p.source, SourceKind::Slack);
    assert_eq!(p.group.name, "Northwind Studio");
    assert!(matches!(&p.group.icon, Some(Media::Local(path)) if path.is_file()));
    // The default channel first, then Slack's alphabetical sidebar; no categories.
    assert_eq!(names(&p), ["general", "design", "eng-backend", "leadership", "random", "watercooler-music"]);
    assert!(p.categories.is_empty());
    assert!(p.channels.iter().all(|c| c.category.is_none() && c.kind.as_str() == "text"));
}

#[test]
fn purpose_and_topic_become_the_description() {
    let p = plan(&northwind(), &ImportOptions::default());
    let desc = |n: &str| p.channels.iter().find(|c| c.name == n).unwrap().description.clone();
    // Different: purpose, then the topic.
    assert_eq!(desc("general").as_deref(), Some("Company-wide announcements and work-based matters\nOffsite is 12 March"));
    // The same text in both: once.
    assert_eq!(desc("random").as_deref(), Some("Non-work banter and water cooler conversation"));
    // Neither: none.
    assert_eq!(desc("eng-backend"), None);
}

#[test]
fn a_private_channel_is_flagged_and_archived_ones_are_skipped_by_default() {
    let p = plan(&northwind(), &ImportOptions::default());
    let private: Vec<_> = p.channels.iter().filter(|c| c.private).map(|c| c.name.as_str()).collect();
    assert_eq!(private, ["leadership"]);
    assert!(p.notes.contains(&Note::PrivateChannels { count: 1 }));

    let archived: Vec<_> = p
        .skipped_channels
        .iter()
        .filter(|s| matches!(s.reason, SkipReason::Archived))
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(archived, ["hiring-2024", "launch-2023"]);
    assert_eq!(p.totals.archived_skipped, 2);
    assert!(p.notes.contains(&Note::ArchivedSkipped { count: 2 }));
    // DMs and group DMs are listed as not imported, never as channels.
    let direct = p.skipped_channels.iter().filter(|s| matches!(s.reason, SkipReason::Direct)).count();
    assert_eq!(direct, 2);
}

#[test]
fn archived_channels_come_when_asked_and_keep_their_flags() {
    let options = ImportOptions { include_archived: true, ..ImportOptions::default() };
    let p = plan(&northwind(), &options);
    assert_eq!(
        names(&p),
        ["general", "design", "eng-backend", "hiring-2024", "launch-2023", "leadership", "random", "watercooler-music"]
    );
    let hiring = p.channels.iter().find(|c| c.name == "hiring-2024").unwrap();
    assert!(hiring.private && hiring.archived);
    assert!(hiring.notes.iter().any(|n| matches!(n, ChannelNote::Archived)));
    assert_eq!(p.totals.archived_skipped, 0);
    assert!(!p.notes.iter().any(|n| matches!(n, Note::ArchivedSkipped { .. })));
}

#[test]
fn people_are_planned_with_emails_admins_and_rosters_never_imported() {
    let p = plan(&northwind(), &ImportOptions::default());
    let person = |u: &str| p.people.iter().find(|x| x.username == u);
    // A deactivated account is never invited, even though it wrote.
    assert!(person("fin").is_none());
    // Owner and admin map to admin; a guest to member.
    assert!(person("ana").unwrap().admin);
    assert!(person("ben").unwrap().admin);
    assert!(!person("eva").unwrap().admin);
    // Emails where Slack shares them, none where it does not.
    assert_eq!(person("cho").unwrap().email.as_deref(), Some("slack-import-cho@pollis.test"));
    assert_eq!(person("dev").unwrap().email, None);
    // Bots, apps and Slackbot are listed apart and never get an email invite.
    for bot in ["github", "polly", "slackbot"] {
        let b = person(bot).unwrap();
        assert!(b.is_bot && b.email.is_none(), "{bot}");
    }
    // The roster: channels they belong to (membership), in plan order.
    assert_eq!(person("cho").unwrap().channels, ["general", "design", "eng-backend", "random"]);
    assert_eq!(person("jo").unwrap().channels, ["general", "design", "random", "watercooler-music"]);
    // Private-channel membership shows on the roster too.
    assert!(person("ben").unwrap().channels.contains(&"leadership".to_string()));

    assert_eq!(p.totals.people, 6);
    assert_eq!(p.totals.people_with_email, 5);
    assert!(p.notes.contains(&Note::EmailInvites { with_email: 5, without_email: 1 }));
    assert!(!p.notes.contains(&Note::NoEmails));
    assert!(p.permissions_known);
    assert!(p.roles.is_empty());
}

#[test]
fn emoji_follow_the_shared_collision_policy_and_aliases_resolve() {
    let p = plan(&northwind(), &ImportOptions::default());
    let e = |n: &str| p.emoji.iter().find(|x| x.source_name == n).unwrap();
    // Used more (inline twice) keeps the plain name; the other is suffixed and reported.
    assert_eq!(e("party-parrot").shortcode.as_deref(), Some("party_parrot"));
    assert!(e("party-parrot").animated);
    assert_eq!(e("party_parrot").shortcode.as_deref(), Some("party_parrot_2"));
    assert!(matches!(&e("party_parrot").note, Some(EmojiNote::Renamed { collided_with }) if collided_with == "party_parrot"));
    // `-` and `+` become `_`.
    assert_eq!(e("nw-logo").shortcode.as_deref(), Some("nw_logo"));
    // Aliases: their own shortcode, the target's picture.
    assert_eq!(e("ship-it").shortcode.as_deref(), Some("ship_it"));
    assert_eq!(e("ship-it").alias_of.as_deref(), Some("shipit"));
    assert_eq!(e("ship-it").image, e("shipit").image);
    assert_eq!(e("partyparrot").shortcode.as_deref(), Some("partyparrot"));
    assert!(e("partyparrot").animated);
    // An alias of a standard emoji has nothing to point at.
    assert!(matches!(e("thumbsup_all").note, Some(EmojiNote::AliasTargetMissing)));
    assert_eq!(e("thumbsup_all").shortcode, None);
    // One character: no legal shortcode. A URL only: no picture to upload.
    assert!(matches!(e("x").note, Some(EmojiNote::NoValidShortcode)));
    assert!(matches!(e("missing-pic").note, Some(EmojiNote::ImageRemote)));
    assert_eq!(p.totals.to_upload_emoji, 6);
    for code in p.emoji.iter().filter_map(|x| x.shortcode.as_deref()) {
        assert!(pollis_import::shortcode::shortcode_is_valid(code), "{code}");
    }
}

#[test]
fn user_groups_are_reported_never_imported() {
    let p = plan(&northwind(), &ImportOptions::default());
    // The disabled group is left out; a deactivated member too.
    assert_eq!(p.user_groups.len(), 1);
    let g = &p.user_groups[0];
    assert_eq!((g.handle.as_str(), g.name.as_str()), ("design-team", "Design team"));
    assert_eq!(g.members, ["cho", "Jo Müller"]);
    assert!(p.notes.contains(&Note::UserGroupsNotImported { count: 1 }));
}

#[test]
fn history_is_measured_and_left_behind() {
    let p = plan(&northwind(), &ImportOptions::default());
    let general = p.channels.iter().find(|c| c.name == "general").unwrap();
    // Three things people wrote (one with a file) and a reply; the join and
    // topic notices are not messages.
    assert_eq!(general.history.messages, 4);
    assert_eq!(general.history.system_messages, 2);
    assert_eq!(general.history.replies, 1);
    assert_eq!(general.history.pinned, 1);
    assert_eq!(general.history.media.files, 1);
    assert!(p.notes.contains(&Note::HistoryNotImported { messages: 10 }));
}

#[test]
fn a_rerun_against_the_result_creates_nothing() {
    let first = plan(&northwind(), &ImportOptions::default());
    let options = ImportOptions {
        existing_channels: first
            .channels
            .iter()
            .map(|c| ExistingChannel { category: None, name: c.name.to_uppercase() })
            .collect(),
        existing_shortcodes: first.emoji.iter().filter_map(|e| e.shortcode.clone()).collect(),
        imported_emoji: first.emoji.iter().filter_map(|e| Some((e.source_id.clone(), e.shortcode.clone()?))).collect(),
        ..ImportOptions::default()
    };
    let again = plan(&northwind(), &options);
    assert_eq!(again.totals.to_create_channels, 0);
    assert_eq!(again.totals.to_upload_emoji, 0);
    let a: Vec<_> = first.emoji.iter().map(|e| e.shortcode.clone()).collect();
    let b: Vec<_> = again.emoji.iter().map(|e| e.shortcode.clone()).collect();
    assert_eq!(a, b, "a re-run keeps every name");
}

#[test]
fn the_downloaded_zip_reads_like_the_folder() {
    let folder = plan(&northwind(), &ImportOptions::default());
    let dir = scratch("zip");
    for (file, prefix) in [("flat.zip", ""), ("wrapped.zip", "Northwind Studio Slack export/")] {
        let zip = dir.join(file);
        // An entry that tries to climb out is ignored, never written.
        zip_fixture(&zip, prefix, &[], &[("../../escape.json", b"[]")]);
        let export = read_export(SourceKind::Slack, &zip).expect(file);
        assert!(export.warnings.is_empty(), "{file}: {:?}", export.warnings);
        let p = plan(&export, &ImportOptions::default());
        assert_eq!(names(&p), names(&folder), "{file}");
        assert_eq!(p.totals.to_upload_emoji, folder.totals.to_upload_emoji, "{file}");
        assert_eq!(p.totals.messages, folder.totals.messages, "{file}");
        // Images inside the ZIP are copied out where the upload can read them.
        for e in p.emoji.iter().filter(|e| e.shortcode.is_some()) {
            assert!(matches!(&e.image, Some(Media::Local(path)) if path.is_file()), "{}", e.source_name);
        }
        assert!(matches!(&p.group.icon, Some(Media::Local(path)) if path.is_file()));
    }
    assert!(!dir.parent().unwrap().join("escape.json").exists());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn without_a_snapshot_the_workspace_is_named_from_the_download() {
    let dir = scratch("nosnap");
    let zip = dir.join("Northwind Studio Slack export Jan 1 2021 - Nov 18 2026.zip");
    zip_fixture(&zip, "", &["slack-workspace.json", "emoji", "icon.png"], &[]);
    let export = read_export(SourceKind::Slack, &zip).unwrap();
    assert!(!export.has_snapshot);
    let p = plan(&export, &ImportOptions::default());
    assert_eq!(p.group.name, "Northwind Studio");
    assert!(p.emoji.is_empty() && p.user_groups.is_empty() && p.group.icon.is_none());
    assert!(p.notes.contains(&Note::NoWorkspaceSnapshot));
    assert_eq!(names(&p).len(), 6);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn something_else_is_not_a_slack_export() {
    let dir = scratch("empty");
    assert!(read_export(SourceKind::Slack, &dir).is_err());
    let not_zip = dir.join("notes.zip");
    fs::write(&not_zip, b"not a zip").unwrap();
    assert!(read_export(SourceKind::Slack, &not_zip).is_err());
    // A Discord folder is not a Slack export either.
    let den = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dragons-den");
    assert!(read_export(SourceKind::Slack, &den).is_err());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn the_invite_sheet_carries_emails_and_rosters() {
    let sheet = report::invite_sheet(&plan(&northwind(), &ImportOptions::default()));
    let lines: Vec<_> = sheet.lines().collect();
    assert_eq!(lines[0], "username,display_name,email,pollis_role,slack_roles,channels,messages");
    // Six people, no bots, no deactivated account.
    assert_eq!(lines.len(), 7);
    assert!(lines.iter().any(|l| l.starts_with("ana,Ana,slack-import-ana@pollis.test,admin,")));
    assert!(lines.iter().any(|l| l.starts_with("dev,Dev Raman,,member,")));
    assert!(!sheet.contains("fin") && !sheet.contains("github"));
}

#[test]
fn the_plan_is_deterministic_and_every_name_is_valid() {
    let a = serde_json::to_string(&plan(&northwind(), &ImportOptions::default())).unwrap();
    let b = serde_json::to_string(&plan(&northwind(), &ImportOptions::default())).unwrap();
    assert_eq!(a, b);
    let p = plan(&northwind(), &ImportOptions { include_archived: true, ..ImportOptions::default() });
    assert!(pollis_api::profile::is_valid_display_name(&p.group.name));
    for c in &p.channels {
        assert!(pollis_api::profile::is_valid_display_name(&c.name), "{}", c.name);
    }
}
