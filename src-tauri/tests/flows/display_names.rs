//! #2085: display names arrive with the data they belong to. The group roster, the DM list and
//! its requests, and a group's pending invites each name people by display name in the same
//! read, with no profile lookup behind them.

use crate::harness::{ds_request_mark, ds_requests_since, wipe, TestClient};
use serde_json::json;
use serial_test::serial;

async fn set_display_name(client: &TestClient, name: &str) {
    client
        .invoke_json(
            "update_user_profile",
            json!({ "userId": client.user_id(), "username": null, "preferredName": name, "phone": null, "avatarUrl": null }),
        )
        .await;
}

fn name_of(rows: &[serde_json::Value], id_key: &str, id: &str, field: &str) -> Option<String> {
    rows.iter()
        .find(|r| r[id_key] == id)
        .and_then(|r| r[field].as_str().map(str::to_string))
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn rosters_dm_lists_and_invites_carry_display_names_in_the_same_read() {
    wipe().await;
    let mut ann = TestClient::new().await;
    let mut bo = TestClient::new().await;
    let mut cy = TestClient::new().await;
    let ann_p = ann.sign_up("ann@test.local").await;
    let bo_p = bo.sign_up("bo@test.local").await;
    let cy_p = cy.sign_up("cy@test.local").await;
    set_display_name(&ann, "Ann Admin").await;
    set_display_name(&cy, "Cy Invitee").await;

    let group = ann.create_group("Team").await;
    ann.invite(&group, &bo_p.username).await;
    let invite = bo.first_pending_invite().await.expect("invite");
    bo.accept_invite(invite["id"].as_str().unwrap()).await;
    bo.poll().await;
    ann.invite(&group, &cy_p.username).await;

    // The roster: one read, the display name on the row (core's `display_name`).
    let bo_device = bo.state.device_id.lock().await.clone().expect("bo's device");
    let mark = ds_request_mark();
    let members = bo
        .invoke_json("get_group_members", json!({ "groupId": group, "requesterId": bo_p.id }))
        .await;
    let members = members.as_array().expect("members");
    assert_eq!(name_of(members, "user_id", &ann_p.id, "display_name").as_deref(), Some("Ann Admin"));
    assert_eq!(name_of(members, "user_id", &bo_p.id, "display_name"), None, "no display name set");
    let directory_users = <pollis_api::directory::DirectoryUsersBody as pollis_api::DsRequest>::PATH;
    assert!(
        !ds_requests_since(mark, &bo_device).iter().any(|p| p == directory_users),
        "the roster must not need a separate profile read"
    );

    // Pending invites name the invitee by display name too.
    let invites = ann.list_group_invites(&group).await;
    assert_eq!(
        name_of(&invites, "invitee_id", &cy_p.id, "invitee_preferred_name").as_deref(),
        Some("Cy Invitee")
    );

    // A DM request and the DM list carry the peer's display name.
    let dm = ann.create_dm(&[&cy_p.id]).await;
    let requests = cy.list_dm_requests().await;
    let request = requests.iter().find(|r| r["id"] == dm.as_str()).expect("request");
    let members = request["members"].as_array().expect("members");
    assert_eq!(name_of(members, "user_id", &ann_p.id, "preferred_name").as_deref(), Some("Ann Admin"));
    cy.accept_dm_request(&dm).await;
    let dms = ann.list_dms().await;
    let listed = dms.iter().find(|r| r["id"] == dm.as_str()).expect("dm");
    let members = listed["members"].as_array().expect("members");
    assert_eq!(name_of(members, "user_id", &cy_p.id, "preferred_name").as_deref(), Some("Cy Invitee"));
}
