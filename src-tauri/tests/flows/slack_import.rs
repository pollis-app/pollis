//! Slack structure import (#1179) through the real group commands and the
//! in-process DS: the fixture workspace lands as one group with its channels
//! in Slack's order and their descriptions, a second run creates nothing,
//! archived channels come only when asked (and slot into place), and the
//! email invites reach exactly the planned emails that have Pollis accounts,
//! once.
//!
//! The harness DS has no object storage, so emoji and the icon (R2 uploads)
//! fail here; their planning is `pollis-import/tests/slack_fixture.rs`, and
//! the uploads are verified against the dev DS (`pollis-tui/examples/import_peer.rs`).

use std::path::PathBuf;

use crate::harness::{wipe, TestClient};
use pollis_core::commands::import::{
    import_apply, import_preview, EmailInviteOutcome, ImportApplyOptions, ImportResult, SourceKind, TARGET_NEW,
};
use serial_test::serial;

fn fixture() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../pollis-import/tests/fixtures/northwind")
        .to_string_lossy()
        .into_owned()
}

/// The group's channels, top to bottom.
async fn layout(client: &TestClient, group_id: &str) -> Vec<String> {
    client.activate();
    let groups = pollis_core::commands::groups::list_user_groups_with_channels(client.user_id().to_string(), &client.state)
        .await
        .expect("groups");
    let g = groups.into_iter().find(|g| g.id == group_id).expect("the imported group");
    assert!(g.categories.is_empty(), "Slack has no categories");
    g.channels.iter().map(|c| c.name.clone()).collect()
}

fn outcome(r: &ImportResult, username: &str) -> Option<EmailInviteOutcome> {
    r.email_invites.iter().find(|i| i.username == username).map(|i| i.outcome)
}

async fn apply(
    client: &TestClient,
    target: Option<String>,
    include_archived: bool,
    invite_by_email: bool,
) -> ImportResult {
    client.activate();
    import_apply(
        SourceKind::Slack,
        fixture(),
        client.user_id().to_string(),
        target,
        Some(ImportApplyOptions { include_archived, invite_by_email }),
        &client.state,
    )
    .await
    .expect("apply")
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_slack_workspace_lands_in_order_and_invites_only_existing_accounts_once() {
    wipe().await;
    // The admin running the import is Ana, the workspace's owner.
    let mut admin = TestClient::new().await;
    admin.sign_up("slack-import-ana@pollis.test").await;
    // Cho already has a Pollis account; nobody else in the export does.
    let mut cho = TestClient::new().await;
    cho.sign_up("slack-import-cho@pollis.test").await;
    let uid = admin.user_id().to_string();
    admin.activate();

    // Dry run: nothing written, archived channels left out.
    let preview = import_preview(SourceKind::Slack, fixture(), uid.clone(), None, None, &admin.state)
        .await
        .expect("preview");
    assert!(preview.target.is_none());
    assert!(!preview.include_archived);
    assert_eq!(preview.plan.group.name, "Northwind Studio");
    assert_eq!(preview.plan.totals.to_create_channels, 6);
    assert!(preview.invite_sheet.contains("slack-import-cho@pollis.test"));
    assert!(admin.invoke_json("list_user_groups", serde_json::json!({ "userId": uid })).await.as_array().unwrap().is_empty());

    let first = apply(&admin, Some(TARGET_NEW.into()), false, true).await;
    assert!(first.created_group);
    assert_eq!(first.group_name, "Northwind Studio");
    assert_eq!((first.categories_created, first.channels_created), (0, 6));
    assert!(first.invite_link.is_some());
    assert!(first.failures.iter().all(|f| f.kind == "emoji" || f.kind == "icon"), "{:?}", first.failures);
    assert_eq!(
        layout(&admin, &first.group_id).await,
        ["general", "design", "eng-backend", "leadership", "random", "watercooler-music"]
    );
    // Purpose and topic arrive as the description.
    let groups = pollis_core::commands::groups::list_user_groups_with_channels(uid.clone(), &admin.state).await.unwrap();
    let general = groups[0].channels.iter().find(|c| c.name == "general").unwrap();
    assert_eq!(
        general.description.as_deref(),
        Some("Company-wide announcements and work-based matters\nOffsite is 12 March")
    );

    // Five planned emails: Cho has an account and is invited; Ana is the
    // admin; the rest have none and join by link. Bots and the deactivated
    // account are never tried; Dev has no email.
    assert_eq!(first.email_invites.len(), 5);
    assert_eq!(outcome(&first, "cho"), Some(EmailInviteOutcome::Invited));
    assert_eq!(outcome(&first, "ana"), Some(EmailInviteOutcome::You));
    for u in ["ben", "eva", "jo"] {
        assert_eq!(outcome(&first, u), Some(EmailInviteOutcome::NoAccount), "{u}");
    }
    assert_eq!(outcome(&first, "fin"), None);
    assert_eq!(outcome(&first, "dev"), None);
    let invite = cho.first_pending_invite().await.expect("Cho has a pending invite");
    assert_eq!(invite["group_name"].as_str(), Some("Northwind Studio"));

    // The re-run finds the group by name, creates nothing and invites nobody twice.
    let again = import_preview(SourceKind::Slack, fixture(), uid.clone(), None, None, &admin.state)
        .await
        .expect("preview again");
    assert_eq!(again.target.as_ref().map(|t| t.group_id.as_str()), Some(first.group_id.as_str()));
    assert_eq!(again.plan.totals.to_create_channels, 0);
    let second = apply(&admin, Some(first.group_id.clone()), false, true).await;
    assert_eq!((second.channels_created, second.channels_existing), (0, 6));
    assert!(second.invite_link.is_none() && second.invite_link_existing);
    assert_eq!(outcome(&second, "cho"), Some(EmailInviteOutcome::AlreadyInvited));

    // Once Cho is in, a re-run says so.
    cho.accept_invite(invite["id"].as_str().unwrap()).await;
    cho.poll().await;
    let third = apply(&admin, Some(first.group_id.clone()), false, true).await;
    assert_eq!(outcome(&third, "cho"), Some(EmailInviteOutcome::AlreadyMember));

    // Archived channels only when asked; they slot into Slack's order.
    let with_archived = import_preview(SourceKind::Slack, fixture(), uid.clone(), Some(first.group_id.clone()), Some(true), &admin.state)
        .await
        .expect("preview with archived");
    assert!(with_archived.include_archived);
    assert_eq!(with_archived.plan.totals.to_create_channels, 2);
    let fourth = apply(&admin, Some(first.group_id.clone()), true, false).await;
    assert_eq!(fourth.channels_created, 2);
    assert!(fourth.email_invites.is_empty(), "no invites unless asked");
    assert_eq!(
        layout(&admin, &first.group_id).await,
        ["general", "design", "eng-backend", "hiring-2024", "launch-2023", "leadership", "random", "watercooler-music"]
    );
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn the_shim_reads_only_a_picked_export() {
    wipe().await;
    let mut admin = TestClient::new().await;
    admin.sign_up("slack-import-owner@pollis.test").await;
    let err = admin
        .invoke_try(
            "import_preview",
            serde_json::json!({ "source": "slack", "path": fixture(), "userId": admin.user_id(), "target": null }),
        )
        .await
        .expect_err("an unpicked folder");
    assert!(err.contains("not chosen in a file dialog"), "{err}");
    // Not a Slack export: a clear error, nothing written.
    admin.activate();
    let discord = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../pollis-import/tests/fixtures/dragons-den");
    let err = import_preview(
        SourceKind::Slack,
        discord.to_string_lossy().into_owned(),
        admin.user_id().to_string(),
        None,
        None,
        &admin.state,
    )
    .await
    .expect_err("a Discord folder is not a Slack export");
    assert!(err.to_string().contains("not a Slack workspace export"), "{err}");
}
