//! A preferences save merges into the stored blob (#1650): no writer can drop a key it
//! does not know, and only an explicit `null` deletes one.

use libsql::Connection;
use pollis_api::profile::SavePreferencesBody;
use pollis_delivery::profile::apply_save_preferences;
use pollis_delivery::writes::WriteOutcome;
use serde_json::{json, Value};

async fn conn() -> Connection {
    let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
    let c = db.connect().unwrap();
    // Production parity: Turso runs with foreign keys off.
    c.execute_batch("PRAGMA foreign_keys=OFF;").await.unwrap();
    pollis_schema::apply::single_db(&c).await.unwrap();
    c
}

async fn save(c: &Connection, user: &str, prefs: Value) -> WriteOutcome {
    let body = SavePreferencesBody {
        user_id: user.into(),
        preferences: prefs.to_string(),
    };
    apply_save_preferences(c, Some(user), &body).await.unwrap()
}

async fn stored(c: &Connection, user: &str) -> Value {
    let mut r = c
        .query(
            "SELECT preferences FROM user_preferences WHERE user_id = ?1",
            libsql::params![user.to_string()],
        )
        .await
        .unwrap();
    let raw: String = r.next().await.unwrap().unwrap().get(0).unwrap();
    serde_json::from_str(&raw).unwrap()
}

/// The #1650 bug: a desktop save names only desktop's keys, and must not reset the phone's.
#[tokio::test]
async fn a_desktop_save_keeps_the_mobile_only_keys() {
    let c = conn().await;
    let mobile = json!({
        "accent_hex": "#5ab0fa",
        "mobile_behavior": { "haptics": false },
        "send_read_receipts": true,
    });
    assert!(matches!(save(&c, "alice", mobile).await, WriteOutcome::Ok));

    let desktop = json!({ "accent_color": "#fabf5a", "send_read_receipts": false });
    assert!(matches!(save(&c, "alice", desktop).await, WriteOutcome::Ok));

    assert_eq!(
        stored(&c, "alice").await,
        json!({
            "accent_hex": "#5ab0fa",
            "mobile_behavior": { "haptics": false },
            "accent_color": "#fabf5a",
            "send_read_receipts": false,
        })
    );
}

/// A named key is replaced whole: a nested entry the save leaves out is gone, not merged.
#[tokio::test]
async fn a_named_object_is_replaced_not_deep_merged() {
    let c = conn().await;
    save(&c, "alice", json!({ "user_volumes": { "bob": 0.5, "carol": 1.5 } })).await;
    save(&c, "alice", json!({ "user_volumes": { "bob": 0.7 } })).await;
    assert_eq!(stored(&c, "alice").await, json!({ "user_volumes": { "bob": 0.7 } }));
}

/// Deleting a key on purpose still works, by sending it as `null`; an omitted key is kept.
#[tokio::test]
async fn null_deletes_a_key_and_omission_keeps_it() {
    let c = conn().await;
    save(&c, "alice", json!({ "font_size": "16", "user_volumes": { "bob": 0.5 }, "skin": "refined" })).await;
    save(&c, "alice", json!({ "font_size": null, "user_volumes": null })).await;
    assert_eq!(stored(&c, "alice").await, json!({ "skin": "refined" }));

    // A first save never stores a `null`.
    save(&c, "bob", json!({ "skin": "terminal", "font_size": null })).await;
    assert_eq!(stored(&c, "bob").await, json!({ "skin": "terminal" }));
}

/// The stored blob is always a JSON object.
#[tokio::test]
async fn a_save_that_is_not_a_json_object_is_refused() {
    let c = conn().await;
    save(&c, "alice", json!({ "skin": "refined" })).await;
    for bad in ["[]", "\"x\"", "42", "null", "{not json"] {
        let body = SavePreferencesBody {
            user_id: "alice".into(),
            preferences: bad.into(),
        };
        let o = apply_save_preferences(&c, Some("alice"), &body).await.unwrap();
        assert!(matches!(o, WriteOutcome::Invalid(_)), "{bad} must be refused");
    }
    assert_eq!(stored(&c, "alice").await, json!({ "skin": "refined" }));
}

/// A malformed legacy row does not wedge every later save: it is treated as empty.
#[tokio::test]
async fn a_malformed_stored_blob_is_replaced_by_the_next_save() {
    let c = conn().await;
    c.execute(
        "INSERT INTO user_preferences (user_id, preferences) VALUES ('alice', 'garbage')",
        (),
    )
    .await
    .unwrap();
    assert!(matches!(save(&c, "alice", json!({ "skin": "refined" })).await, WriteOutcome::Ok));
    assert_eq!(stored(&c, "alice").await, json!({ "skin": "refined" }));
}

/// Only your own blob: a save claiming another user is refused and changes nothing.
#[tokio::test]
async fn a_save_for_another_user_is_forbidden() {
    let c = conn().await;
    save(&c, "alice", json!({ "skin": "refined" })).await;
    let body = SavePreferencesBody {
        user_id: "alice".into(),
        preferences: json!({ "skin": "terminal" }).to_string(),
    };
    let o = apply_save_preferences(&c, Some("mallory"), &body).await.unwrap();
    assert!(matches!(o, WriteOutcome::Forbidden));
    assert_eq!(stored(&c, "alice").await, json!({ "skin": "refined" }));
}
