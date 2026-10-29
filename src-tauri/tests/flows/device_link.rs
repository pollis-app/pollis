//! QR device link, end to end through the real dispatch path (#1207):
//! an enrolled device shows a QR, a fresh client claims it (no email OTP),
//! enrolls, and the enrolled device approves on the link tag instead of a
//! typed code. Plus the guardrails: the PIN gate, single use, and a forged tag
//! that must never be approvable. `docs/qr-device-link-design.md`.

use crate::harness::{wipe, TestClient, TEST_PIN};
use pollis_core::commands::auth::UserProfile;
use serde_json::json;
use serial_test::serial;

/// Claim a QR payload on `client`, decoding the profile.
async fn claim(client: &TestClient, payload: &str, name: &str) -> Result<UserProfile, String> {
    client
        .invoke_try("claim_device_link", json!({ "payload": payload, "deviceName": name }))
        .await
        .map(|v| serde_json::from_value(v).expect("UserProfile"))
}

/// The primary: signed up, PIN set, with a group to prove the linked device
/// really joins the account.
async fn primary_with_group(email: &str) -> (TestClient, String) {
    let mut primary = TestClient::new().await;
    primary.sign_up(email).await;
    let group: serde_json::Value = primary
        .invoke_json(
            "create_group",
            json!({ "name": "Linked Group", "description": null, "ownerId": primary.user_id() }),
        )
        .await;
    (primary, group["id"].as_str().expect("group id").to_string())
}

async fn create_link(primary: &TestClient) -> (String, String) {
    let handle = primary
        .invoke_json("create_device_link", json!({ "userId": primary.user_id(), "pin": TEST_PIN }))
        .await;
    (
        handle["link_id"].as_str().expect("link_id").to_string(),
        handle["qr_payload"].as_str().expect("qr_payload").to_string(),
    )
}

async fn link_state(primary: &TestClient, link_id: &str) -> serde_json::Value {
    primary
        .invoke_json("poll_device_link", json!({ "userId": primary.user_id(), "linkId": link_id }))
        .await
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_qr_link_enrolls_a_phone_without_the_email_otp() {
    wipe().await;
    let (primary, group_id) = primary_with_group("link-owner@test.local").await;
    let (link_id, payload) = create_link(&primary).await;
    assert_eq!(link_state(&primary, &link_id).await["state"], "open");

    // The phone scans: signed in by the link alone.
    let mut phone = TestClient::new().await;
    let profile = claim(&phone, &payload, "Test Phone").await.unwrap_or_else(|e| panic!("claim_device_link: {e}"));
    assert_eq!(profile.id, primary.user_id(), "the link signs the phone into the creator's account");
    assert!(profile.enrollment_required, "a linked phone still has to be handed the account key");
    phone.profile = Some(profile.clone());

    let claimed = link_state(&primary, &link_id).await;
    assert_eq!(claimed["state"], "claimed");
    assert_eq!(claimed["device_name"], "Test Phone");

    // The phone files its (tagged) enrollment request…
    let handle = phone.invoke_json("start_device_enrollment", json!({ "userId": profile.id })).await;
    let request_id = handle["request_id"].as_str().expect("request_id").to_string();

    // …and the creator sees it as approvable — the tag verified — without
    // anyone typing a code.
    let ready = link_state(&primary, &link_id).await;
    assert_eq!(ready["state"], "ready_to_approve", "the tag must verify: {ready}");

    // The typed-code approval never sees it: the phone shows no code, so the
    // sign-in-time fallback that raises desktop's "a new device wants to
    // enroll" card (and mobile's) must not list a link-bound request. The live
    // nudge is skipped on its `link_id` instead (core `dispatch_data`).
    let pending = primary
        .invoke_json("list_pending_enrollment_requests", json!({ "userId": primary.user_id() }))
        .await;
    assert!(
        !pending.as_array().unwrap().iter().any(|r| r["request_id"] == request_id.as_str()),
        "a QR-link request must not reach the typed-code approval: {pending}"
    );
    primary
        .invoke_json("approve_device_link", json!({ "userId": primary.user_id(), "linkId": link_id }))
        .await;

    let mut status = String::new();
    for _ in 0..20 {
        let resp = phone.invoke_json("poll_enrollment_status", json!({ "requestId": request_id })).await;
        status = resp["status"].as_str().unwrap_or_default().to_string();
        if status == "approved" {
            break;
        }
    }
    assert_eq!(status, "approved");

    // The order the apps use: PIN (opens the local DB), then finalize.
    phone
        .invoke_try("set_pin", json!({ "newPin": TEST_PIN, "oldPin": null }))
        .await
        .unwrap_or_else(|e| panic!("set_pin: {e}"));
    phone
        .invoke_try("finalize_device_enrollment", json!({ "userId": profile.id }))
        .await
        .unwrap_or_else(|e| panic!("finalize_device_enrollment: {e}"));

    let devices = phone.invoke_json("list_user_devices", json!({ "userId": profile.id })).await;
    assert_eq!(devices.as_array().map(|a| a.len()), Some(2), "two devices after linking: {devices}");
    let groups = phone.invoke_json("list_user_groups", json!({ "userId": profile.id })).await;
    assert!(
        groups.as_array().unwrap().iter().any(|g| g["id"] == group_id.as_str()),
        "the linked phone sees the account's group: {groups}"
    );

    // Audit: the enrollment is recorded as a QR link.
    let events = primary
        .invoke_json("list_security_events", json!({ "userId": primary.user_id(), "limit": 10 }))
        .await;
    assert!(
        events["events"].as_array().unwrap().iter().any(|e| e["kind"] == "device_enrolled"
            && e["metadata"].as_str().is_some_and(|m| m.contains("via=qr_link"))),
        "a device_enrolled event with via=qr_link: {events}"
    );

    // #1548: the same command pages and filters through the real dispatch path.
    // A `kinds` filter keeps only that kind, and a one-row page of a log with
    // more than one row hands back a cursor that resumes strictly after it.
    let filtered = primary
        .invoke_json(
            "list_security_events",
            json!({ "userId": primary.user_id(), "limit": 50, "kinds": ["device_enrolled"] }),
        )
        .await;
    let filtered_rows = filtered["events"].as_array().unwrap();
    assert!(!filtered_rows.is_empty(), "{filtered}");
    assert!(
        filtered_rows.iter().all(|e| e["kind"] == "device_enrolled"),
        "the kind filter leaked another kind: {filtered}"
    );
    let all = events["events"].as_array().unwrap();
    if all.len() > 1 {
        let first = primary
            .invoke_json("list_security_events", json!({ "userId": primary.user_id(), "limit": 1 }))
            .await;
        let cursor = first["next_cursor"].clone();
        assert!(cursor.is_object(), "a one-row page of a longer log has a cursor: {first}");
        let second = primary
            .invoke_json(
                "list_security_events",
                json!({ "userId": primary.user_id(), "limit": 1, "before": cursor }),
            )
            .await;
        assert_eq!(first["events"][0]["id"], all[0]["id"], "{first}");
        assert_eq!(second["events"][0]["id"], all[1]["id"], "{second}");
    }
    drop(phone);
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_link_cannot_be_created_without_the_pin() {
    wipe().await;
    let (primary, _) = primary_with_group("link-pin@test.local").await;
    let wrong = if TEST_PIN == "1111" { "2222" } else { "1111" };
    let res = primary
        .invoke_try("create_device_link", json!({ "userId": primary.user_id(), "pin": wrong }))
        .await;
    assert!(res.is_err(), "a wrong PIN must not mint a link: {res:?}");
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_qr_can_only_be_claimed_once() {
    wipe().await;
    let (primary, _) = primary_with_group("link-once@test.local").await;
    let (_, payload) = create_link(&primary).await;

    let first = TestClient::new().await;
    claim(&first, &payload, "First").await.unwrap_or_else(|e| panic!("first claim: {e}"));

    let second = TestClient::new().await;
    let again = claim(&second, &payload, "Second").await;
    assert!(again.is_err(), "a QR is single use: {:?}", again.map(|p| p.id));
}

/// A request whose tag was not keyed by the QR's secret — what a server that
/// substituted the phone's ephemeral key would have to present — is reported
/// as tampered and refused, never wrapped to.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_forged_link_tag_is_never_approvable() {
    wipe().await;
    let (primary, _) = primary_with_group("link-forged@test.local").await;
    let (link_id, payload) = create_link(&primary).await;

    let mut phone = TestClient::new().await;
    let profile = claim(&phone, &payload, "Forger").await.unwrap_or_else(|e| panic!("claim: {e}"));
    phone.profile = Some(profile.clone());

    // Key the phone's tag with a MAC key that is NOT derived from the QR.
    {
        let mut pending = phone.state.device_link_pending.lock().await;
        let link = pending.as_mut().expect("claim leaves pending link state");
        link.kind = pollis_core::commands::device_link::PendingLinkKind::Claimed {
            mac_key: zeroize::Zeroizing::new([0xAB; 32]),
        };
    }
    phone.invoke_json("start_device_enrollment", json!({ "userId": profile.id })).await;

    let st = link_state(&primary, &link_id).await;
    assert_eq!(st["state"], "tampered", "a tag the QR's secret did not key must not verify: {st}");
    let approve = primary
        .invoke_try("approve_device_link", json!({ "userId": primary.user_id(), "linkId": link_id }))
        .await;
    assert!(approve.is_err(), "approval must refuse a tampered request: {approve:?}");
}

// ── v2: the NEW device shows the QR, a signed-in device scans it (#1361) ─────

/// The new device offers a code; returns `(link_id, qr_payload)`.
async fn offer(new_device: &TestClient) -> (String, String) {
    let handle = new_device
        .invoke_json("create_device_offer", json!({ "deviceName": "Test Desktop" }))
        .await;
    assert!(
        handle["qr_payload"].as_str().unwrap().starts_with("pollis-link:v2:"),
        "a v2 code: {handle}"
    );
    (
        handle["link_id"].as_str().expect("link_id").to_string(),
        handle["qr_payload"].as_str().expect("qr_payload").to_string(),
    )
}

async fn accept(scanner: &TestClient, payload: &str, pin: &str) -> Result<serde_json::Value, String> {
    scanner
        .invoke_try("accept_device_offer", json!({ "userId": scanner.user_id(), "pin": pin, "payload": payload }))
        .await
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_scanned_code_signs_the_showing_device_in_without_the_email_otp() {
    wipe().await;
    let (phone, group_id) = primary_with_group("scan-owner@test.local").await;
    let mut desktop = TestClient::new().await;
    let (link_id, payload) = offer(&desktop).await;

    // The phone scans it and confirms with its PIN.
    let accepted = accept(&phone, &payload, TEST_PIN).await.unwrap_or_else(|e| panic!("accept: {e}"));
    assert_eq!(accepted["device_name"], "Test Desktop");

    // The desktop collects its session: signed in by the scan alone.
    let wait = desktop.invoke_json("await_device_offer", json!({ "linkId": link_id })).await;
    assert_eq!(wait["state"], "signed_in", "{wait}");
    let profile: UserProfile = serde_json::from_value(wait["profile"].clone()).expect("UserProfile");
    assert_eq!(profile.id, phone.user_id(), "the scan signs the desktop into the scanner's account");
    assert!(profile.enrollment_required);
    desktop.profile = Some(profile.clone());

    let handle = desktop.invoke_json("start_device_enrollment", json!({ "userId": profile.id })).await;
    let request_id = handle["request_id"].as_str().expect("request_id").to_string();

    // The phone wraps to the key it read off the QR — no Approve, no code.
    let done = phone
        .invoke_json("finish_device_offer", json!({ "userId": phone.user_id(), "linkId": link_id }))
        .await;
    assert_eq!(done["state"], "linked", "{done}");

    let status = desktop.invoke_json("poll_enrollment_status", json!({ "requestId": request_id })).await;
    assert_eq!(status["status"], "approved");
    desktop
        .invoke_try("set_pin", json!({ "newPin": TEST_PIN, "oldPin": null }))
        .await
        .unwrap_or_else(|e| panic!("set_pin: {e}"));
    desktop
        .invoke_try("finalize_device_enrollment", json!({ "userId": profile.id }))
        .await
        .unwrap_or_else(|e| panic!("finalize_device_enrollment: {e}"));
    let groups = desktop.invoke_json("list_user_groups", json!({ "userId": profile.id })).await;
    assert!(
        groups.as_array().unwrap().iter().any(|g| g["id"] == group_id.as_str()),
        "the desktop sees the account's group: {groups}"
    );

    let events = phone
        .invoke_json("list_security_events", json!({ "userId": phone.user_id(), "limit": 10 }))
        .await;
    assert!(
        events["events"].as_array().unwrap().iter().any(|e| e["kind"] == "device_enrolled"
            && e["metadata"].as_str().is_some_and(|m| m.contains("via=qr_scan"))),
        "a device_enrolled event with via=qr_scan: {events}"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_code_cannot_be_accepted_without_the_pin_or_twice() {
    wipe().await;
    let (phone, _) = primary_with_group("scan-pin@test.local").await;
    let desktop = TestClient::new().await;
    let (_, payload) = offer(&desktop).await;

    let wrong = if TEST_PIN == "1111" { "2222" } else { "1111" };
    assert!(accept(&phone, &payload, wrong).await.is_err(), "a wrong PIN must not accept");

    accept(&phone, &payload, TEST_PIN).await.unwrap_or_else(|e| panic!("accept: {e}"));
    // Bound once: another account cannot take it over, nor can this one again.
    let (other, _) = primary_with_group("scan-other@test.local").await;
    assert!(accept(&other, &payload, TEST_PIN).await.is_err(), "an accepted code is single use");
    assert!(accept(&phone, &payload, TEST_PIN).await.is_err(), "an accepted code is single use");
}

/// The key the account identity is wrapped to must be the one the QR showed.
/// A request carrying any other key — what a server substituting it would have
/// to present — is refused and nothing is wrapped.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_request_whose_key_is_not_the_scanned_one_is_never_wrapped_to() {
    wipe().await;
    let (phone, _) = primary_with_group("scan-swap@test.local").await;
    let mut desktop = TestClient::new().await;
    let (link_id, payload) = offer(&desktop).await;
    accept(&phone, &payload, TEST_PIN).await.unwrap_or_else(|e| panic!("accept: {e}"));
    let wait = desktop.invoke_json("await_device_offer", json!({ "linkId": link_id })).await;
    let profile: UserProfile = serde_json::from_value(wait["profile"].clone()).expect("UserProfile");
    desktop.profile = Some(profile.clone());

    // Enroll with a key that is NOT the one the QR carried.
    {
        let mut pending = desktop.state.device_link_pending.lock().await;
        let link = pending.as_mut().expect("collecting the offer leaves pending link state");
        link.kind = pollis_core::commands::device_link::PendingLinkKind::Offered {
            ephemeral_secret: zeroize::Zeroizing::new([0x5A; 32]),
        };
    }
    let handle = desktop.invoke_json("start_device_enrollment", json!({ "userId": profile.id })).await;
    let request_id = handle["request_id"].as_str().expect("request_id").to_string();

    let done = phone
        .invoke_json("finish_device_offer", json!({ "userId": phone.user_id(), "linkId": link_id }))
        .await;
    assert_eq!(done["state"], "tampered", "a key the QR did not show must not be wrapped to: {done}");
    let status = desktop.invoke_json("poll_enrollment_status", json!({ "requestId": request_id })).await;
    assert_eq!(status["status"], "pending", "nothing was approved: {status}");
}

/// Each side refuses the other direction's code, saying where it belongs.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn the_wrong_device_for_a_code_says_so() {
    wipe().await;
    let (phone, _) = primary_with_group("scan-wrong@test.local").await;
    let (_, v1) = create_link(&phone).await;
    let other_phone = TestClient::new().await;
    let desktop = TestClient::new().await;
    let (_, v2) = offer(&desktop).await;
    let e = accept(&phone, &v1, TEST_PIN).await.unwrap_err();
    assert!(e.contains("device you're signing in"), "{e}");
    let e = claim(&other_phone, &v2, "Phone").await.unwrap_err();
    assert!(e.contains("signed-in phone"), "{e}");
}
