//! Per-group aliases (#2003), end to end through the real command pipeline and
//! the in-process DS: fallback, the membership gate, apply-to-all, the lifecycle
//! across leave / reset / admin reset, and the alias a group's push names.

use crate::harness::{wipe, TestClient};
use serial_test::serial;

async fn aliases(client: &TestClient, group_id: &str) -> Vec<serde_json::Value> {
    client
        .invoke_json(
            "list_group_aliases",
            serde_json::json!({ "groupId": group_id, "requesterId": client.user_id() }),
        )
        .await
        .as_array()
        .expect("alias list")
        .clone()
}

async fn mine(client: &TestClient) -> Vec<serde_json::Value> {
    client
        .invoke_json("list_my_aliases", serde_json::json!({ "userId": client.user_id() }))
        .await
        .as_array()
        .expect("own alias list")
        .clone()
}

async fn set(client: &TestClient, group_id: Option<&str>, name: Option<&str>, avatar: Option<&str>) {
    client
        .invoke_json(
            "set_group_alias",
            serde_json::json!({
                "userId": client.user_id(),
                "groupId": group_id,
                "displayName": name,
                "avatarUrl": avatar,
            }),
        )
        .await;
}

fn name_of(list: &[serde_json::Value], user_id: &str) -> Option<String> {
    list.iter()
        .find(|a| a["user_id"] == user_id)
        .and_then(|a| a["display_name"].as_str().map(str::to_string))
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
async fn aliases_are_per_group_members_only_and_outlive_membership() {
    wipe().await;
    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;
    let _alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let _carol_p = carol.sign_up("carol@test.local").await;

    let work = alice.create_group("Work").await;
    let club = alice.create_group("Club").await;
    join(&alice, &bob, &work, &bob_p.username).await;
    join(&alice, &bob, &club, &bob_p.username).await;

    // No backfill: nothing to overlay, every surface shows the main profile.
    assert!(aliases(&alice, &work).await.is_empty());

    set(&bob, Some(&work), Some("Robert (Work)"), None).await;
    assert_eq!(name_of(&aliases(&alice, &work).await, &bob_p.id).as_deref(), Some("Robert (Work)"));
    assert!(aliases(&alice, &club).await.is_empty(), "an alias is per group");

    // A non-member reads nothing, and cannot write there.
    assert!(aliases(&carol, &work).await.is_empty());
    let refused = carol
        .invoke_try(
            "set_group_alias",
            serde_json::json!({ "userId": carol.user_id(), "groupId": work, "displayName": "Intruder" }),
        )
        .await;
    assert!(refused.is_err(), "a non-member may not set an alias in the group");

    // Apply to all: the name lands in every current group, overwriting the field only.
    set(&bob, None, Some("Bobby"), None).await;
    assert_eq!(name_of(&aliases(&alice, &work).await, &bob_p.id).as_deref(), Some("Bobby"));
    assert_eq!(name_of(&aliases(&alice, &club).await, &bob_p.id).as_deref(), Some("Bobby"));
    assert_eq!(mine(&bob).await.len(), 2);

    // Leaving keeps it: the remaining members still render it on his old messages.
    bob.leave_group(&club).await;
    alice.poll().await;
    assert_eq!(name_of(&aliases(&alice, &club).await, &bob_p.id).as_deref(), Some("Bobby"));
    assert!(aliases(&bob, &club).await.is_empty(), "the leaver no longer reads the group");
    assert_eq!(mine(&bob).await.len(), 2, "the owner still lists it");

    // The owner resets it after leaving.
    bob.invoke_json(
        "reset_group_alias",
        serde_json::json!({ "groupId": club, "userId": bob_p.id, "requesterId": bob_p.id }),
    )
    .await;
    assert!(aliases(&alice, &club).await.is_empty());

    // A plain member cannot reset someone else's; the admin can, and he may set it again.
    let alice_id = alice.user_id().to_string();
    set(&alice, Some(&work), Some("Al"), None).await;
    let refused = bob
        .invoke_try(
            "reset_group_alias",
            serde_json::json!({ "groupId": work, "userId": alice_id, "requesterId": bob_p.id }),
        )
        .await;
    assert!(refused.is_err(), "only an admin resets another member's alias");
    alice
        .invoke_json(
            "reset_group_alias",
            serde_json::json!({ "groupId": work, "userId": bob_p.id, "requesterId": alice_id }),
        )
        .await;
    assert_eq!(name_of(&aliases(&alice, &work).await, &bob_p.id), None);
    set(&bob, Some(&work), Some("Bob again"), None).await;
    assert_eq!(name_of(&aliases(&alice, &work).await, &bob_p.id).as_deref(), Some("Bob again"));

    // "Use my main profile": clearing the last field removes the alias.
    set(&bob, Some(&work), Some(""), None).await;
    assert_eq!(name_of(&aliases(&alice, &work).await, &bob_p.id), None);
    assert!(mine(&bob).await.is_empty());
}
