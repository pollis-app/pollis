//! #2084: a profile change on one device reaches the user's other devices and every other
//! member, on messages already on screen, without a restart; and a device that missed the
//! nudge catches up the next time its database opens, never keeping the old name for good.

use crate::harness::{enroll_second_device, wipe, TestClient, TEST_PIN};
use serial_test::serial;
use serde_json::json;

/// The sender label every message from `sender_id` carries on `client`'s next page read.
async fn labels(client: &TestClient, channel_id: &str, sender_id: &str) -> Vec<Option<String>> {
    client
        .fetch_channel_messages(channel_id)
        .await
        .iter()
        .filter(|m| m["sender_id"] == sender_id)
        .map(|m| m["sender_username"].as_str().map(str::to_string))
        .collect()
}

async fn join(admin: &TestClient, member: &TestClient, group_id: &str, username: &str) {
    admin.invite(group_id, username).await;
    let invite = member.first_pending_invite().await.expect("pending invite");
    member.accept_invite(invite["id"].as_str().expect("invite id")).await;
    member.poll().await;
    admin.poll().await;
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_rename_reaches_own_devices_and_other_members_without_a_restart() {
    wipe().await;
    let mut ann = TestClient::new().await;
    let mut bo = TestClient::new().await;
    let mut cy = TestClient::new().await;
    let ann_p = ann.sign_up("ann@test.local").await;
    let bo_p = bo.sign_up("bo@test.local").await;
    let cy_p = cy.sign_up("cy@test.local").await;
    let ann2 = enroll_second_device(&ann, "ann@test.local").await;

    let group = ann.create_group("Team").await;
    join(&ann, &bo, &group, &bo_p.username).await;
    join(&ann, &cy, &group, &cy_p.username).await;
    ann2.poll().await;
    let channel = ann.general_channel_id(&group).await;
    ann.send_channel_message(&channel, "before the rename").await;

    // Every device has read Ann's old handle once, so each has it cached.
    let old = Some(ann_p.username.clone());
    for client in [&ann2, &bo, &cy] {
        assert_eq!(labels(client, &channel, &ann_p.id).await, vec![old.clone()]);
    }

    ann.invoke_json(
        "update_user_profile",
        json!({
            "userId": ann_p.id,
            "username": "ann-renamed",
            "preferredName": "Ann Renamed",
            "phone": null,
            "avatarUrl": null,
        }),
    )
    .await;
    let new = Some("ann-renamed".to_string());
    // The renaming device shows it at once.
    assert_eq!(labels(&ann, &channel, &ann_p.id).await, vec![new.clone()]);

    // What each client does on the DS's `profile_changed` nudge: refresh, then re-read.
    for client in [&ann2, &bo] {
        let fresh = client
            .invoke_json("get_user_profile", json!({ "userId": ann_p.id }))
            .await;
        assert_eq!(fresh["username"], "ann-renamed");
        assert_eq!(fresh["preferred_name"], "Ann Renamed");
        assert_eq!(
            labels(client, &channel, &ann_p.id).await,
            vec![new.clone()],
            "an old message must show the new name after the nudge, with no restart"
        );
    }

    // A new message after the rename carries it too, for a client that already refreshed.
    ann.send_channel_message(&channel, "after the rename").await;
    assert_eq!(labels(&bo, &channel, &ann_p.id).await, vec![new.clone(), new.clone()]);

    // Cy missed the nudge (app closed). Reopening the app re-checks every cached name.
    cy.invoke_json("lock", json!({})).await;
    cy.invoke_json("unlock", json!({ "userId": cy_p.id, "pin": TEST_PIN })).await;
    assert_eq!(
        labels(&cy, &channel, &ann_p.id).await,
        vec![new.clone(), new.clone()],
        "a missed nudge must not keep the old name across a restart"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn refreshing_ones_own_profile_from_another_device_updates_the_account_index() {
    wipe().await;
    let mut ann = TestClient::new().await;
    let ann_p = ann.sign_up("ann@test.local").await;
    let ann2 = enroll_second_device(&ann, "ann@test.local").await;

    ann.invoke_json(
        "update_user_profile",
        json!({ "userId": ann_p.id, "username": "ann-elsewhere", "preferredName": null, "phone": null, "avatarUrl": null }),
    )
    .await;
    ann2.invoke_json("get_user_profile", json!({ "userId": ann_p.id })).await;
    let index = pollis_lib::accounts::read_accounts_index().expect("accounts index");
    let entry = index
        .accounts
        .iter()
        .find(|a| a.user_id == ann_p.id)
        .expect("account entry");
    assert_eq!(entry.username, "ann-elsewhere");
}
