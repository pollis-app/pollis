//! Discord structure import (#1180) through the real group commands and the
//! in-process DS: the fixture guild lands as categories and channels in
//! Discord's order, a second run creates nothing, a run after a partial one
//! fills the gap in place, and the path-taking shims refuse an unpicked folder.
//!
//! The harness DS has no object storage, so emoji and the icon (both R2
//! uploads) are reported as failures here; the planning side of emoji is
//! `pollis-import/tests/plan_fixture.rs`, and the uploads are verified against
//! the dev DS (PR for #1180).

use std::path::PathBuf;

use crate::harness::{wipe, TestClient};
use pollis_core::commands::import::{discord_import_apply, discord_import_preview, TARGET_NEW};
use serial_test::serial;

fn fixture() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../pollis-import/tests/fixtures/dragons-den")
        .to_string_lossy()
        .into_owned()
}

/// The group's sidebar as `Category / channel` lines, top to bottom.
async fn layout(client: &TestClient, group_id: &str) -> Vec<String> {
    client.activate();
    let groups = pollis_core::commands::groups::list_user_groups_with_channels(client.user_id().to_string(), &client.state)
        .await
        .expect("groups");
    let g = groups.into_iter().find(|g| g.id == group_id).expect("the imported group");
    let names: std::collections::BTreeMap<_, _> = g.categories.iter().map(|k| (k.id.clone(), k.name.clone())).collect();
    let mut out: Vec<String> = g
        .channels
        .iter()
        .map(|c| {
            let voice = if c.channel_type == "voice" { " (voice)" } else { "" };
            match c.category_id.as_ref().and_then(|id| names.get(id)) {
                Some(k) => format!("{k} / {}{voice}", c.name),
                None => format!("{}{voice}", c.name),
            }
        })
        .collect();
    out.extend(g.categories.iter().map(|k| format!("[{}]", k.name)));
    out
}

const EXPECTED: [&str; 18] = [
    "rules",
    "Text Channels / general",
    "Text Channels / looking-for-group",
    "Campaign / session-notes",
    "Campaign / lore",
    "Campaign / maps",
    "Campaign / gm-only",
    "Campaign / the-table (voice)",
    "Voice Channels / tavern (voice)",
    "Voice Channels / storytime (voice)",
    "Off-topic / memes",
    "Off-topic / 3-fan-art",
    "Off-topic / general",
    "[Text Channels]",
    "[Campaign]",
    "[Voice Channels]",
    "[Off-topic]",
    "[Archive]",
];

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_discord_server_lands_in_order_and_a_rerun_creates_nothing() {
    wipe().await;
    let mut admin = TestClient::new().await;
    admin.sign_up("import-admin@pollis.test").await;
    let uid = admin.user_id().to_string();
    admin.activate();

    // Dry run: nothing written.
    let preview = discord_import_preview(fixture(), uid.clone(), None, &admin.state).await.expect("preview");
    assert!(preview.target.is_none(), "no group of that name yet");
    assert_eq!(preview.plan.totals.to_create_channels, 13);
    assert!(admin.invoke_json("list_user_groups", serde_json::json!({ "userId": uid })).await.as_array().unwrap().is_empty());

    let first = discord_import_apply(fixture(), uid.clone(), Some(TARGET_NEW.into()), &admin.state).await.expect("apply");
    assert!(first.created_group);
    assert_eq!(first.group_name, "Dragon's Den");
    assert_eq!((first.categories_created, first.channels_created, first.channels_moved), (5, 13, 0));
    assert!(first.invite_link.is_some(), "an invite link to share");
    // Only the R2 uploads fail in the harness.
    assert!(
        first.failures.iter().all(|f| f.kind == "emoji" || f.kind == "icon"),
        "{:?}",
        first.failures
    );
    assert_eq!(layout(&admin, &first.group_id).await, EXPECTED);

    // The re-run finds the group by name and creates nothing.
    let again = discord_import_preview(fixture(), uid.clone(), None, &admin.state).await.expect("preview again");
    assert_eq!(again.target.as_ref().map(|t| t.group_id.as_str()), Some(first.group_id.as_str()));
    assert_eq!(again.plan.totals.to_create_channels, 0);
    assert_eq!(again.plan.totals.to_create_categories, 0);
    let second = discord_import_apply(fixture(), uid.clone(), Some(first.group_id.clone()), &admin.state)
        .await
        .expect("apply again");
    assert_eq!((second.categories_created, second.channels_created), (0, 0));
    assert_eq!((second.categories_existing, second.channels_existing), (5, 13));
    assert!(second.invite_link.is_none() && second.invite_link_existing, "no second link");
    assert_eq!(layout(&admin, &first.group_id).await, EXPECTED);

    // After a partial run (a channel missing from the middle of a block),
    // the re-run puts it back where it belongs.
    let groups = pollis_core::commands::groups::list_user_groups_with_channels(uid.clone(), &admin.state).await.unwrap();
    let lore = groups[0].channels.iter().find(|c| c.name == "lore").expect("lore").id.clone();
    pollis_core::commands::groups::delete_channel(lore, uid.clone(), &admin.state).await.expect("delete lore");
    let third = discord_import_apply(fixture(), uid.clone(), Some(first.group_id.clone()), &admin.state)
        .await
        .expect("apply after partial");
    assert_eq!(third.channels_created, 1);
    assert!(third.channels_moved >= 1);
    assert_eq!(layout(&admin, &first.group_id).await, EXPECTED);
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn only_an_admin_imports_into_a_group_and_only_from_a_picked_folder() {
    wipe().await;
    let mut admin = TestClient::new().await;
    admin.sign_up("import-owner@pollis.test").await;
    let mut member = TestClient::new().await;
    let member_profile = member.sign_up("import-member@pollis.test").await;
    let group_id = admin.create_group("Dragon's Den").await;
    admin.invite(&group_id, &member_profile.username).await;
    let invite = member.first_pending_invite().await.expect("pending invite");
    member.accept_invite(invite["id"].as_str().expect("invite id")).await;
    member.poll().await;

    member.activate();
    let preview = discord_import_preview(fixture(), member.user_id().to_string(), None, &member.state)
        .await
        .expect("a member may read the plan");
    assert!(preview.candidates.is_empty(), "not an admin of the matching group");
    let refused = discord_import_apply(fixture(), member.user_id().to_string(), Some(group_id.clone()), &member.state).await;
    assert!(refused.is_err(), "a member cannot import into the group");

    // The shim reads only a folder chosen in this device's picker.
    let err = admin
        .invoke_try(
            "discord_import_preview",
            serde_json::json!({ "dir": fixture(), "userId": admin.user_id(), "target": null }),
        )
        .await
        .expect_err("an unpicked folder");
    assert!(err.contains("not chosen in a file dialog"), "{err}");
}
