//! No client IP may be readable in a stored device name or security-log note.
//!
//! Shipped desktop builds named a device `"{hostname} ({os})"`, and a hostname
//! can embed the machine's IP (`ip-10-0-0-12`). Two halves are pinned here:
//! migration 000034 scrubs rows written before the fix, and the DS write paths
//! redact any IP-shaped token a still-shipped client sends — redact, not refuse,
//! because those clients' writes must keep succeeding.

use pollis_api::account::SecurityEventBody;
use pollis_delivery::account::apply_record_security_event;
use pollis_delivery::writes::WriteOutcome;

mod common;

const SCRUB: &str = include_str!("../../pollis-schema/migrations/000034_scrub_ip_shaped_device_names.sql");

/// A DB at the schema just BEFORE the scrub, so it can be seeded with the rows
/// old clients wrote and the migration run against them.
async fn pre_scrub_db() -> common::TempDb {
    let db = common::TempDb::open("db.db").await;
    let conn = db.conn().await.unwrap();
    conn.execute_batch(pollis_schema::BASELINE_SQL).await.expect("baseline");
    for (version, _, sql) in pollis_schema::POST_BASELINE_MIGRATIONS {
        if *version >= 34 {
            break;
        }
        conn.execute_batch(sql).await.expect("migration");
    }
    conn.execute_batch("INSERT INTO users (id, email, username) VALUES ('u1','u1@x','u1');")
        .await
        .expect("seed user");
    db
}

#[tokio::test]
async fn the_scrub_replaces_ip_shaped_names_and_leaves_ordinary_ones() {
    let db = pre_scrub_db().await;
    let conn = db.conn().await.unwrap();
    let names = [
        ("d1", "ip-10-0-0-12 (linux)", "Linux desktop"),
        ("d2", "c-73-162-1-2.hsd1.ca.comcast.net (macos)", "macOS desktop"),
        ("d3", "203.0.113.7 (windows)", "Windows desktop"),
        ("d4", "fe80::1 (linux)", "Linux desktop"),
        ("d5", "192_168_1_5", "device"),
        ("d6", "dans-macbook-pro (macos)", "dans-macbook-pro (macos)"),
        ("d7", "Pollis on Windows", "Pollis on Windows"),
        ("d8", "macOS desktop", "macOS desktop"),
    ];
    for (id, name, _) in names {
        conn.execute(
            "INSERT INTO user_device (device_id, user_id, device_name) VALUES (?1, 'u1', ?2)",
            libsql::params![id, name],
        )
        .await
        .unwrap();
    }
    let events = [
        ("e1", "name=ip-10-0-0-5 (linux)", "name=Linux desktop"),
        ("e2", "name=dans-macbook-pro (macos)", "name=dans-macbook-pro (macos)"),
        ("e3", "via=qr,approver=01HQ7Z", "via=qr,approver=01HQ7Z"),
        // DS-authored JSON with colons is not client device text: untouched.
        ("e4", "{\"from\":\"a@x\",\"to\":\"b@x\"}", "{\"from\":\"a@x\",\"to\":\"b@x\"}"),
    ];
    for (id, meta, _) in events {
        conn.execute(
            "INSERT INTO security_event (id, user_id, kind, metadata) VALUES (?1, 'u1', 'k', ?2)",
            libsql::params![id, meta],
        )
        .await
        .unwrap();
    }

    conn.execute_batch(SCRUB).await.expect("scrub");
    // Idempotent: a second run changes nothing.
    conn.execute_batch(SCRUB).await.expect("scrub again");

    for (id, before, want) in names {
        let mut rows = conn
            .query("SELECT device_name FROM user_device WHERE device_id = ?1", libsql::params![id])
            .await
            .unwrap();
        let got: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(got, want, "device name {before:?}");
    }
    for (id, before, want) in events {
        let mut rows = conn
            .query("SELECT metadata FROM security_event WHERE id = ?1", libsql::params![id])
            .await
            .unwrap();
        let got: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(got, want, "metadata {before:?}");
    }
}

#[tokio::test]
async fn a_security_event_note_is_redacted_on_write_not_refused() {
    let db = common::TempDb::open("db.db").await;
    let conn = db.conn().await.unwrap();
    pollis_schema::apply::single_db(&conn).await.expect("schema");
    conn.execute_batch("INSERT INTO users (id, email, username) VALUES ('u1','u1@x','u1');")
        .await
        .unwrap();
    let body = SecurityEventBody {
        kind: "device_revoked".to_string(),
        device_id: Some("d1".to_string()),
        metadata: Some("name=ip-10-0-0-5 (linux)".to_string()),
        user_id: Some("u1".to_string()),
    };
    let outcome = apply_record_security_event(&conn, Some("u1"), &body).await.unwrap();
    assert!(matches!(outcome, WriteOutcome::Ok), "an old client's write must still succeed");
    let mut rows = conn.query("SELECT metadata FROM security_event", ()).await.unwrap();
    let got: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(got, "name=[redacted] (linux)");
}
