//! App approvals (#2005), end to end through the in-process DS: an admin
//! approves, narrows and updates an App from the bridge commands the apps use;
//! the runtime's check (`bots::apps::verify_installation_approval`, run before
//! every load) accepts exactly that and refuses every record it cannot verify
//! from its own state; members read the approved App and its marks.

use serde_json::json;
use serial_test::serial;

use pollis_api::bot_apps::BotAppRunState;
use pollis_app_manifest::AdminGrants;
use pollis_core::commands::bots::app_approval::{
    check_artifact, check_artifact_from, developer_key_public, sign_dev_build, AppApproval, AppApprovalRecord, AppApprovalRefusal, Artifact,
    DevApproval, VerifiedAppApproval, APP_APPROVAL_VERSION,
};
use pollis_core::commands::bots::apps::{self, AppRunReport, ArtifactFacts};
use pollis_core::commands::bots::FixedAudit;
use pollis_lib::commands::transparency::AuditStatus;

use crate::harness::{pair_new_bot, wipe, writable_remote, BotHostClient, TestClient};

const V1: &str = r##"{"name":"triage","version":"1.0.0","triggers":["messages"],
    "read":{"channels":["#general"],"history":30},"actions":["reply","react"],"egress":["api.linear.app"]}"##;
/// v1 minus react: narrows only.
const V2: &str = r##"{"name":"triage","version":"1.1.0","triggers":["messages"],
    "read":{"channels":["#general"],"history":30},"actions":["reply"],"egress":["api.linear.app"]}"##;
/// v2 plus a new egress host: a widening.
const V3: &str = r##"{"name":"triage","version":"2.0.0","triggers":["messages"],
    "read":{"channels":["#general"],"history":30},"actions":["reply"],"egress":["api.linear.app","hooks.example.com"]}"##;

fn component(tag: &str) -> Vec<u8> {
    format!("\0asm\x0d\0\x01\0 {tag}").into_bytes()
}

fn hex_hash(c: &[u8], m: &str) -> String {
    pollis_app_manifest::hex(&pollis_app_manifest::app_hash(c, m.as_bytes()).unwrap())
}

fn manifest_hash(m: &str) -> String {
    pollis_app_manifest::hex(&pollis_app_manifest::manifest_hash(m.as_bytes()).unwrap())
}

async fn device_of(client: &TestClient) -> String {
    client.state.device_id.lock().await.clone().expect("device id")
}

async fn join(admin: &TestClient, member: &TestClient, group_id: &str, username: &str) {
    admin.invite(group_id, username).await;
    let invite = member.first_pending_invite().await.expect("pending invite");
    member.accept_invite(invite["id"].as_str().expect("invite id")).await;
    member.poll().await;
    admin.poll().await;
}

/// A group with an admin (alice), a second admin (carol), a member (bob) and a bot on one host.
struct Fixture {
    alice: TestClient,
    bob: TestClient,
    carol: TestClient,
    group: String,
    host: BotHostClient,
}

async fn fixture() -> Fixture {
    wipe().await;
    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let carol_p = carol.sign_up("carol@test.local").await;
    let group = alice.create_group("Ops").await;
    join(&alice, &bob, &group, &bob_p.username).await;
    join(&alice, &carol, &group, &carol_p.username).await;
    alice.set_member_role(&group, &carol_p.id, "admin").await;
    let (host, _) = pair_new_bot(&alice, &group, "Triage").await;
    // The host sees everyone's current leaves.
    host.read_channel(&alice.general_channel_id(&group).await).await.unwrap();
    Fixture { alice, bob, carol, group, host }
}

impl Fixture {
    fn bot(&self) -> &str {
        self.host.bot_user_id()
    }

    /// The runtime tells the admins what artifact it has (as `pollis-bot` does
    /// after a refused check).
    async fn runtime_has(&self, component: &[u8], manifest: &str, developer_key: Option<String>) {
        let facts = ArtifactFacts {
            app_hash: Some(hex_hash(component, manifest)),
            manifest: Some(manifest.to_string()),
            source: Some("https://apps.example/triage.wasm".into()),
            developer_key,
        };
        let report = AppRunReport::refused(&AppApprovalRefusal::NotApproved, facts);
        apps::report_app(&self.host.state, self.host.session(), &report).await.expect("report");
    }

    /// The runtime's check before a load.
    async fn check(&self) -> Result<VerifiedAppApproval, AppApprovalRefusal> {
        self.check_with(AuditStatus::Unavailable).await
    }

    async fn check_with(&self, log_says: AuditStatus) -> Result<VerifiedAppApproval, AppApprovalRefusal> {
        apps::verify_installation_approval(&self.host.state, self.host.session(), &FixedAudit(log_says))
            .await
            .expect("verify")
    }

    async fn view(&self, admin: &TestClient) -> serde_json::Value {
        admin.invoke_json("get_bot_app", json!({ "groupId": self.group, "botUserId": self.bot() })).await
    }

    async fn approve(&self, admin: &TestClient, input: serde_json::Value) -> Result<serde_json::Value, String> {
        admin.invoke_try("approve_bot_app", json!({ "input": input })).await
    }

    /// The next seq the DS will take.
    async fn next_seq(&self) -> i64 {
        let conn = writable_remote().await.conn().await.expect("remote conn");
        let mut rows = conn
            .query(
                "SELECT COALESCE(MAX(seq), 0) + 1 FROM bot_app_approval WHERE installation_id = ?1",
                libsql::params![self.bot().to_string()],
            )
            .await
            .expect("seq");
        rows.next().await.unwrap().unwrap().get(0).unwrap()
    }

    fn production(&self, signer: &str, device: &str, seq: i64, app_hash: String, manifest: &str, grants: AdminGrants) -> AppApproval {
        AppApproval {
            app_hash,
            approver_device_id: device.into(),
            approver_user_id: signer.into(),
            auto_update: false,
            bot_user_id: self.bot().into(),
            grants,
            group_id: self.group.clone(),
            installation_id: self.bot().into(),
            issued_at: chrono::Utc::now().timestamp(),
            manifest_hash: manifest_hash(manifest),
            seq,
            source: None,
            v: APP_APPROVAL_VERSION,
        }
    }

    /// Server-side: store a record as the DS would whatever it was given.
    async fn plant(&self, seq: i64, kind: &str, encoded: &str, manifest: &str, approver: &str) {
        let conn = writable_remote().await.conn().await.expect("remote conn");
        conn.execute(
            "INSERT INTO bot_app_approval (installation_id, seq, kind, record, manifest, approver_user_id, approver_device_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'planted')",
            libsql::params![self.bot().to_string(), seq, kind.to_string(), encoded.to_string(), manifest.to_string(), approver.to_string()],
        )
        .await
        .expect("plant approval");
    }

    /// Sign `record` with `signer`'s device key and plant it at its seq.
    async fn plant_signed(&self, signer: &TestClient, record: &AppApprovalRecord, row_kind: &str, manifest: &str) {
        let encoded = apps::sign_app_approval(&signer.state, signer.user_id(), &device_of(signer).await, record)
            .await
            .expect("sign");
        self.plant(record.common().seq, row_kind, &encoded, manifest, signer.user_id()).await;
    }

    /// An honest production approval of `manifest` by `signer`, planted.
    async fn honest(&self, signer: &TestClient, manifest: &str) -> AppApprovalRecord {
        let seq = self.next_seq().await;
        let grants = full_grants(manifest);
        let record = AppApprovalRecord::Production(self.production(
            signer.user_id(),
            &device_of(signer).await,
            seq,
            hex_hash(&component("v1"), manifest),
            manifest,
            grants,
        ));
        self.plant_signed(signer, &record, "production", manifest).await;
        record
    }
}

/// Everything a manifest asks for, with no channel (no channel id is needed to verify).
fn full_grants(manifest: &str) -> AdminGrants {
    let m = pollis_app_manifest::Manifest::from_json(manifest.as_bytes()).unwrap();
    AdminGrants {
        channels: Vec::new(),
        history: m.read.history,
        events: m.read.events.clone(),
        actions: m.actions.clone(),
        egress: m.egress.clone(),
    }
}

fn refused(r: Result<VerifiedAppApproval, AppApprovalRefusal>) -> AppApprovalRefusal {
    match r {
        Err(refusal) => refusal,
        Ok(v) => panic!("expected a refusal, got approval {}", v.seq()),
    }
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn an_admin_approves_narrows_and_updates_an_app_and_the_runtime_runs_exactly_that() {
    let f = fixture().await;

    // Nothing approved: nothing runs; admins see the artifact the runtime has.
    assert_eq!(refused(f.check().await), AppApprovalRefusal::NotApproved);
    f.runtime_has(&component("v1"), V1, None).await;
    let view = f.view(&f.alice).await;
    assert!(view["approved"].is_null(), "{view}");
    let pending = &view["pending"];
    assert_eq!(pending["appHash"], hex_hash(&component("v1"), V1));
    assert_eq!(pending["changes"].as_array().unwrap().len(), 0, "a first approval has no diff");
    assert_eq!(view["report"]["state"], "awaiting_approval");
    let general = f.alice.general_channel_id(&f.group).await;
    assert_eq!(view["channels"][0]["id"], general, "the grant controls offer #general");

    // A member who is not an admin cannot approve.
    let input = json!({
        "groupId": f.group, "botUserId": f.bot(), "development": false,
        "appHash": pending["appHash"], "manifestHash": pending["manifestHash"],
        "grants": pending["suggestedGrants"], "autoUpdate": false,
    });
    assert!(f.approve(&f.bob, input.clone()).await.is_err());

    // Approve: the runtime accepts exactly that App with those grants.
    let view = f.approve(&f.alice, input).await.expect("approve");
    assert_eq!(view["approved"]["seq"], 1);
    let v = f.check().await.expect("approval verifies");
    let app = check_artifact(&v, &Artifact { component: &component("v1"), manifest_json: V1.as_bytes(), dev_signature: None })
        .expect("the approved artifact loads");
    assert_eq!(app.grants.channels.len(), 1);
    assert!(!app.grants.egress.is_empty());
    assert!(matches!(
        check_artifact(&v, &Artifact { component: &component("other"), manifest_json: V1.as_bytes(), dev_signature: None }),
        Err(AppApprovalRefusal::HashMismatch { .. })
    ));

    // Members read the approved App; it can reach api.linear.app.
    let listed = f.bob.invoke_json("list_group_apps", json!({ "groupId": f.group })).await;
    assert_eq!(listed[0]["botUserId"], f.bot());
    assert_eq!(listed[0]["development"], false);
    assert_eq!(listed[0]["cannotSendOutside"], false);

    // Narrow in place: drop the egress host and shorten history.
    let approved = &view["approved"];
    let mut grants = approved["grants"].clone();
    grants["egress"] = json!([]);
    grants["history"] = json!(7);
    let view = f
        .approve(
            &f.alice,
            json!({
                "groupId": f.group, "botUserId": f.bot(), "development": false,
                "appHash": approved["appHash"], "manifestHash": approved["manifestHash"],
                "grants": grants, "autoUpdate": false,
            }),
        )
        .await
        .expect("narrow");
    assert_eq!(view["approved"]["seq"], 2);
    assert_eq!(view["approved"]["cannotSendOutside"], true);
    let v = f.check().await.expect("narrowed approval verifies");
    assert_eq!(v.grants().history, 7);
    assert!(v.grants().egress.is_empty());
    let listed = f.bob.invoke_json("list_group_apps", json!({ "groupId": f.group })).await;
    assert_eq!(listed[0]["cannotSendOutside"], true, "members see \"Can't send data outside this group\"");

    // A wider grant than the manifest is refused before anything is signed.
    let mut wide = view["approved"]["grants"].clone();
    wide["egress"] = json!(["evil.example"]);
    let err = f
        .approve(
            &f.alice,
            json!({
                "groupId": f.group, "botUserId": f.bot(), "development": false,
                "appHash": view["approved"]["appHash"], "manifestHash": view["approved"]["manifestHash"],
                "grants": wide, "autoUpdate": false,
            }),
        )
        .await
        .unwrap_err();
    assert!(err.contains("wider"), "{err}");

    // An update with a widening: flagged, and the new host is not pre-accepted.
    f.runtime_has(&component("v3"), V3, None).await;
    let view = f.view(&f.alice).await;
    let pending = &view["pending"];
    assert_eq!(pending["widens"], true);
    let widenings: Vec<&serde_json::Value> =
        pending["changes"].as_array().unwrap().iter().filter(|c| c["widening"] == true).collect();
    assert!(widenings.iter().any(|c| c["field"] == "egress" && c["value"] == "hooks.example.com"), "{pending}");
    assert_eq!(pending["suggestedGrants"]["egress"], json!([]), "a widening is never pre-accepted");
    let view = f
        .approve(
            &f.alice,
            json!({
                "groupId": f.group, "botUserId": f.bot(), "development": false,
                "appHash": pending["appHash"], "manifestHash": pending["manifestHash"],
                "grants": pending["suggestedGrants"], "autoUpdate": false,
            }),
        )
        .await
        .expect("approve update");
    assert_eq!(view["approved"]["seq"], 3);
    let v = f.check().await.expect("update verifies");
    let app = check_artifact(&v, &Artifact { component: &component("v3"), manifest_json: V3.as_bytes(), dev_signature: None })
        .expect("the update loads");
    assert!(app.grants.egress.is_empty());
    // The old artifact no longer runs.
    assert!(matches!(
        check_artifact(&v, &Artifact { component: &component("v1"), manifest_json: V1.as_bytes(), dev_signature: None }),
        Err(AppApprovalRefusal::HashMismatch { .. })
    ));

    // Approving what the runtime no longer has is refused as changed.
    let err = f
        .approve(
            &f.alice,
            json!({
                "groupId": f.group, "botUserId": f.bot(), "development": false,
                "appHash": hex_hash(&component("gone"), V2), "manifestHash": manifest_hash(V2),
                "grants": full_grants(V2), "autoUpdate": false,
            }),
        )
        .await
        .unwrap_err();
    assert!(err.contains(apps::APP_APPROVAL_CHANGED), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn the_runtime_refuses_every_approval_it_cannot_verify() {
    let f = fixture().await;
    let alice_dev = device_of(&f.alice).await;

    // Signed by a member who is not an admin.
    f.honest(&f.bob, V1).await;
    assert!(matches!(refused(f.check().await), AppApprovalRefusal::NotAdmin(_)));

    // Alice's device named, but signed by another key (bob's).
    let seq = f.next_seq().await;
    let forged = AppApprovalRecord::Production(f.production(
        f.alice.user_id(),
        &alice_dev,
        seq,
        hex_hash(&component("v1"), V1),
        V1,
        full_grants(V1),
    ));
    let encoded = apps::sign_app_approval(&f.bob.state, f.bob.user_id(), &device_of(&f.bob).await, &forged).await.unwrap();
    f.plant(seq, "production", &encoded, V1, f.alice.user_id()).await;
    assert_eq!(refused(f.check().await), AppApprovalRefusal::BadSignature);

    // A device that is no leaf of the approver's.
    let seq = f.next_seq().await;
    let rec = AppApprovalRecord::Production(f.production(
        f.alice.user_id(),
        "01JFORGEDDEVICE00000000000",
        seq,
        hex_hash(&component("v1"), V1),
        V1,
        full_grants(V1),
    ));
    f.plant_signed(&f.alice, &rec, "production", V1).await;
    assert_eq!(refused(f.check().await), AppApprovalRefusal::ApproverNotInGroup);

    // The served manifest is not the one the record names.
    let seq = f.next_seq().await;
    let rec = AppApprovalRecord::Production(f.production(
        f.alice.user_id(),
        &alice_dev,
        seq,
        hex_hash(&component("v1"), V1),
        V1,
        full_grants(V1),
    ));
    f.plant_signed(&f.alice, &rec, "production", V3).await;
    assert_eq!(refused(f.check().await), AppApprovalRefusal::ManifestMismatch);

    // Grants wider than the manifest, even signed by an admin.
    let seq = f.next_seq().await;
    let mut grants = full_grants(V1);
    grants.egress.insert("evil.example".into());
    let rec =
        AppApprovalRecord::Production(f.production(f.alice.user_id(), &alice_dev, seq, hex_hash(&component("v1"), V1), V1, grants));
    f.plant_signed(&f.alice, &rec, "production", V1).await;
    assert!(matches!(refused(f.check().await), AppApprovalRefusal::WidenedGrants(_)));

    // A development approval served as a production one.
    let seq = f.next_seq().await;
    let dev = AppApprovalRecord::Development(DevApproval {
        approver_device_id: alice_dev.clone(),
        approver_user_id: f.alice.user_id().into(),
        bot_user_id: f.bot().into(),
        developer_key: developer_key_public(&[4u8; 32]),
        grants: full_grants(V1),
        group_id: f.group.clone(),
        installation_id: f.bot().into(),
        issued_at: chrono::Utc::now().timestamp(),
        manifest_hash: manifest_hash(V1),
        seq,
        v: APP_APPROVAL_VERSION,
    });
    f.plant_signed(&f.alice, &dev, "production", V1).await;
    assert_eq!(refused(f.check().await), AppApprovalRefusal::WrongKind);

    // A record for another bot.
    let seq = f.next_seq().await;
    let mut other = f.production(f.alice.user_id(), &alice_dev, seq, hex_hash(&component("v1"), V1), V1, full_grants(V1));
    other.installation_id = "01JOTHERBOT000000000000000".into();
    other.bot_user_id = other.installation_id.clone();
    f.plant_signed(&f.alice, &AppApprovalRecord::Production(other), "production", V1).await;
    assert_eq!(refused(f.check().await), AppApprovalRefusal::WrongInstallation);

    // The transparency log disagrees with the approver's key.
    f.honest(&f.alice, V1).await;
    assert_eq!(refused(f.check_with(AuditStatus::Alarm).await), AppApprovalRefusal::TransparencyAlarm);

    // Honest: accepted, and the host remembers the seq.
    let accepted = f.honest(&f.alice, V1).await;
    let v = f.check().await.expect("honest approval verifies");
    assert_eq!(v.seq(), accepted.common().seq);

    // A rolled-back seq: the DS withholds the newest record and serves the one before.
    let newest = f.honest(&f.alice, V1).await.common().seq;
    f.check().await.expect("newest verifies");
    let conn = writable_remote().await.conn().await.expect("remote conn");
    conn.execute("DROP TRIGGER IF EXISTS bot_app_approval_kept_while_the_bot_exists", ()).await.unwrap();
    conn.execute(
        "DELETE FROM bot_app_approval WHERE installation_id = ?1 AND seq = ?2",
        libsql::params![f.bot().to_string(), newest],
    )
    .await
    .unwrap();
    conn.execute(
        "CREATE TRIGGER bot_app_approval_kept_while_the_bot_exists BEFORE DELETE ON bot_app_approval FOR EACH ROW \
         WHEN EXISTS (SELECT 1 FROM users WHERE id = OLD.installation_id) \
         BEGIN SELECT RAISE(ABORT, 'bot app approvals go only with their bot'); END",
        (),
    )
    .await
    .expect("restore the schema");
    assert_eq!(
        refused(f.check().await),
        AppApprovalRefusal::RolledBack { accepted: newest, served: newest - 1 }
    );
    assert_eq!(apps::accepted_seq(&f.host.state, f.bot()).await.unwrap(), newest);

    // A demoted admin's approval, then a removed admin's.
    f.honest(&f.carol, V1).await;
    f.check().await.expect("carol, an admin, approves");
    f.honest(&f.carol, V1).await;
    f.alice.set_member_role(&f.group, f.carol.user_id(), "member").await;
    assert!(matches!(refused(f.check().await), AppApprovalRefusal::NotAdmin(_)));
    f.alice.set_member_role(&f.group, f.carol.user_id(), "admin").await;
    f.honest(&f.carol, V1).await;
    f.alice.remove_member(&f.group, f.carol.user_id()).await;
    f.host.read_channel(&f.alice.general_channel_id(&f.group).await).await.unwrap();
    assert!(matches!(
        refused(f.check().await),
        AppApprovalRefusal::ApproverNotInGroup | AppApprovalRefusal::NotAdmin(_)
    ));
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_development_installation_runs_its_developers_builds_and_members_see_the_mark() {
    let f = fixture().await;
    let seed = [6u8; 32];
    let key = developer_key_public(&seed);
    f.runtime_has(&component("dev-1"), V2, Some(key.clone())).await;
    let view = f.view(&f.alice).await;
    let pending = &view["pending"];
    assert_eq!(pending["developerKey"], key);
    let view = f
        .approve(
            &f.alice,
            json!({
                "groupId": f.group, "botUserId": f.bot(), "development": true,
                "developerKey": key, "manifestHash": pending["manifestHash"],
                "grants": pending["suggestedGrants"], "autoUpdate": false,
            }),
        )
        .await
        .expect("approve as development");
    assert_eq!(view["approved"]["development"], true);

    let v = f.check().await.expect("development approval verifies");
    assert!(matches!(v.record, AppApprovalRecord::Development(_)));
    // Any build the key signs runs, as long as it does not widen.
    for tag in ["dev-2", "dev-3"] {
        let c = component(tag);
        let sig = sign_dev_build(&seed, &pollis_app_manifest::app_hash(&c, V2.as_bytes()).unwrap());
        check_artifact(&v, &Artifact { component: &c, manifest_json: V2.as_bytes(), dev_signature: Some(&sig) })
            .expect("a signed build loads");
    }
    let c = component("dev-4");
    let sig = sign_dev_build(&seed, &pollis_app_manifest::app_hash(&c, V3.as_bytes()).unwrap());
    assert!(matches!(
        check_artifact(&v, &Artifact { component: &c, manifest_json: V3.as_bytes(), dev_signature: Some(&sig) }),
        Err(AppApprovalRefusal::BuildWidens(_))
    ));

    let listed = f.bob.invoke_json("list_group_apps", json!({ "groupId": f.group })).await;
    assert_eq!(listed[0]["development"], true, "every member sees the Development build mark");
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn an_installation_set_to_update_itself_takes_non_widening_updates_from_its_source_with_no_admin_online() {
    let f = fixture().await;
    let source = "https://apps.example/triage.wasm";
    f.runtime_has(&component("v1"), V1, None).await;
    let view = f.view(&f.alice).await;
    let pending = &view["pending"];
    f.approve(
        &f.alice,
        json!({
            "groupId": f.group, "botUserId": f.bot(), "development": false,
            "appHash": pending["appHash"], "manifestHash": pending["manifestHash"],
            "grants": pending["suggestedGrants"], "autoUpdate": true,
        }),
    )
    .await
    .expect("approve with auto-update");
    let approved_hash = hex_hash(&component("v1"), V1);
    let (f, approved_hash) = (&f, approved_hash.as_str());
    let run = |c: Vec<u8>, m: &'static str, from: &'static str| async move {
        let v = f.check().await.expect("approval verifies");
        let app = check_artifact_from(&v, &Artifact { component: &c, manifest_json: m.as_bytes(), dev_signature: None }, from)?;
        let hash = pollis_app_manifest::hex(&app.app_hash);
        apps::accept_app_hash(&f.host.state, f.bot(), v.seq(), approved_hash, &hash).await.expect("ledger")?;
        Ok::<_, AppApprovalRefusal>(app)
    };

    // No admin does anything from here on.
    run(component("v1"), V1, source).await.expect("the approved artifact runs");
    // A narrowing v2 from the pinned source: the runtime takes it itself.
    let app = run(component("v2"), V2, source).await.expect("a non-widening update runs");
    assert!(app.auto_updated);
    // The same v2 from anywhere else waits for an admin.
    assert!(matches!(run(component("v2"), V2, "/opt/elsewhere/triage.wasm").await, Err(AppApprovalRefusal::HashMismatch { .. })));
    // A widening v3 from the pinned source waits for an admin.
    assert!(matches!(run(component("v3"), V3, source).await, Err(AppApprovalRefusal::BuildWidens(_))));
    // Serving v1 again after v2 cannot undo the update.
    assert_eq!(run(component("v1"), V1, source).await.unwrap_err(), AppApprovalRefusal::ArtifactRolledBack);
    run(component("v2"), V2, source).await.expect("v2 still runs");

    // Admins see what runs from the runtime's report; nothing is waiting.
    let facts = ArtifactFacts {
        app_hash: Some(hex_hash(&component("v2"), V2)),
        manifest: Some(V2.to_string()),
        source: Some(source.into()),
        developer_key: None,
    };
    apps::report_app(&f.host.state, f.host.session(), &AppRunReport::running(facts, true)).await.expect("report");
    let view = f.view(&f.carol).await;
    assert!(view["pending"].is_null(), "{view}");
    assert_eq!(view["running"]["autoUpdated"], true);
    assert_eq!(view["running"]["version"], "1.1.0");
    assert_eq!(view["report"]["state"], serde_json::to_value(BotAppRunState::Running).unwrap());
}
