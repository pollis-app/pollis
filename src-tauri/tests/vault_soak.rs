//! Vault media soak (#1491), ad hoc only (`#[ignore]`; `scripts/vault-soak.sh`). See `.codesight/wiki/testing.md`.

#[path = "vault_soak/config.rs"]
mod config;
#[path = "vault_soak/corpus.rs"]
mod corpus;
#[path = "vault_soak/proofs.rs"]
mod proofs;
#[path = "vault_soak/report.rs"]
mod report;
#[path = "vault_soak/tap.rs"]
mod tap;
#[path = "vault_soak/world.rs"]
mod world;

use std::collections::HashMap;
use std::time::{Instant, SystemTime};

use serde_json::json;

use config::{human, SoakConfig, Target};
use corpus::{Kind, SoakFile};
use proofs::{attacker_keys, open_all, open_chunk0, sealed_len, sha256, BlobVerdict, Scanner};
use report::{FileRun, Proof};
use world::{operator_get, plain_get, Client, World};

struct Stored {
    idx: usize,
    entry_id: String,
    key: String,
    file_id: String,
    loopback_url: Option<String>,
}

fn kind_name(k: &Kind) -> String {
    match k {
        Kind::Random => "random".to_string(),
        Kind::Media => "media".to_string(),
        Kind::Duplicate(_) => "duplicate".to_string(),
    }
}

fn libsql_text(s: &str) -> libsql::Value {
    libsql::Value::Text(s.to_string())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "ad-hoc soak: run via scripts/vault-soak.sh"]
async fn vault_media_soak() {
    let cfg = SoakConfig::from_env();
    std::fs::create_dir_all(&cfg.workdir).expect("workdir");
    let started = SystemTime::now();
    let t_all = Instant::now();
    eprintln!("[vault-soak] target {:?}, workdir {}, seed {}", cfg.target, cfg.workdir.display(), cfg.seed);

    let t = Instant::now();
    let files = corpus::build(&cfg.workdir.join("corpus"), &cfg.sizes, cfg.media_dir.as_deref(), cfg.seed);
    eprintln!("[vault-soak] corpus: {} files, {} in {:.1}s", files.len(), human(files.iter().map(|f| f.size).sum()), t.elapsed().as_secs_f64());
    let mut base = Scanner::new();
    corpus::add_needles(&mut base, &files);

    let world = world::setup(&cfg, base.clone_unbuilt().build()).await;
    let suffix = format!("{:x}", rand::random::<u32>());
    let alice = Client::sign_up(&world, &format!("pollis-soak-{suffix}@example.com")).await;
    eprintln!("[vault-soak] signed up {}", alice.user_id);

    // Account-wide secrets that must never leave the device or reach its disk raw.
    let account = pollis_core::commands::vault::test_hooks::vault_file_secrets(&alice.state, &alice.user_id, &"00".repeat(32))
        .await
        .expect("vault secrets");
    let db_key = alice.db_key().await.expect("unlocked");
    let mut scanner = base.clone_unbuilt();
    scanner.add_secret(&account.account_seed, None, "account identity key seed");
    scanner.add_secret(&account.root_key, None, "Vault file root key");
    scanner.add_secret(&db_key, None, "local DB key");
    let set_scanner = |s: &Scanner| {
        let built = s.clone_unbuilt().build();
        world.ds_tap.set_scanner(built.clone());
        if let Some(t) = &world.s3_tap {
            t.set_scanner(built.clone());
        }
        built
    };
    let mut built = set_scanner(&scanner);

    let mut runs: Vec<FileRun> = files
        .iter()
        .map(|f| FileRun {
            label: f.label.clone(),
            kind: kind_name(&f.kind),
            content_type: f.content_type.clone(),
            size: f.size,
            ..Default::default()
        })
        .collect();
    let mut stored: Vec<Stored> = Vec::new();
    let mut objects: HashMap<usize, [u8; 32]> = HashMap::new();
    let cap = pollis_api::broker::R2_MEDIA_MAX_BYTES;
    let cache_cap = pollis_core::commands::r2::MEDIA_CACHE_MAX_FILE_BYTES;

    for (i, f) in files.iter().enumerate() {
        let run = &mut runs[i];
        eprintln!("[vault-soak] {} ({}) …", f.label, human(f.size));
        alice.pick_file(&f.path);
        let marks = (world.ds_tap.mark(), world.s3_tap.as_ref().map(|t| t.mark()));
        let t = Instant::now();
        let up: Result<serde_json::Value, String> = alice
            .invoke("upload_media", json!({ "path": f.path.to_string_lossy(), "filename": f.filename, "contentType": f.content_type }))
            .await;
        let upload_secs = t.elapsed().as_secs_f64();

        if f.size > cap {
            run.over_cap = true;
            let quiet = world.ds_tap.mark() == marks.0 && world.s3_tap.as_ref().map(|t| t.mark()) == marks.1;
            run.push(Proof::check(
                "upload refused over the cap",
                up.is_err() && quiet,
                format!(
                    "{} > R2_MEDIA_MAX_BYTES {} → {} in {:.3}s; requests during the call: {}",
                    human(f.size),
                    human(cap),
                    match &up { Err(e) => format!("refused ({e})"), Ok(_) => "ACCEPTED".to_string() },
                    upload_secs,
                    if quiet { "none (no byte left the device)".to_string() } else { "SOME".to_string() },
                ),
            ));
            continue;
        }
        let up = match up {
            Ok(v) => v,
            Err(e) => {
                run.push(Proof::check("upload", false, e));
                continue;
            }
        };
        let key = up["key"].as_str().unwrap().to_string();
        let file_id = up["content_hash"].as_str().unwrap().to_string();
        let sha = up["sha256"].as_str().unwrap().to_string();
        let kv = up["key_version"].as_i64().unwrap_or(-1);
        run.file_id = Some(file_id.clone());

        let content = json!({ "_att": [{
            "key": key, "url": up["url"], "name": f.filename, "ct": f.content_type,
            "size": f.size, "hash": file_id, "kv": kv, "sha": sha,
            "bh": up["blurhash"], "w": up["width"], "h": up["height"],
        }]})
        .to_string();
        let t2 = Instant::now();
        let entry: Result<serde_json::Value, String> =
            alice.invoke("send_vault_message", json!({ "userId": alice.user_id, "content": content })).await;
        run.upload_secs = Some(upload_secs + t2.elapsed().as_secs_f64());
        let entry_id = match entry {
            Ok(v) => v["id"].as_str().unwrap().to_string(),
            Err(e) => {
                run.push(Proof::check("upload", false, format!("send_vault_message: {e}")));
                continue;
            }
        };
        run.push(Proof::check(
            "upload",
            sha == f.sha_hex() && kv == 0 && key == format!("media/{file_id}.enc"),
            format!("object {key}, Vault key version {kv}, returned sha256 matches the source: {}", sha == f.sha_hex()),
        ));
        // #1382: the file id is random, not a content digest.
        run.push(Proof::check(
            "DS: file id is not derived from the content",
            file_id != f.sha_hex() && file_id != hex::encode(sha256(f.sha_hex().as_bytes())),
            format!("content_hash {file_id} ≠ sha256(plaintext)"),
        ));

        let sec = pollis_core::commands::vault::test_hooks::vault_file_secrets(&alice.state, &alice.user_id, &file_id)
            .await
            .expect("file secrets");
        scanner.add_secret(&sec.file_key, Some(i), &format!("{} file key", f.label));
        scanner.add_secret(&sec.file_nonce, Some(i), &format!("{} file nonce", f.label));
        built = set_scanner(&scanner);

        let (status, object) = operator_get(&world.http, &cfg.s3, &key).await;
        let verdict = BlobVerdict::of(&built, &object);
        let want_len = sealed_len(f.size);
        run.push(Proof::check(
            "at rest: object is ciphertext",
            status == 200 && verdict.clean() && object.len() as u64 == want_len && verdict.sha256 != f.sha256,
            format!(
                "HTTP {status}; {} (sealed length {} expected {}); sha256(object) ≠ sha256(plaintext): {}",
                verdict.summary(),
                object.len(),
                want_len,
                verdict.sha256 != f.sha256
            ),
        ));
        objects.insert(i, verdict.sha256);
        let model = proofs::derive_like_file_key(&sec.root_key, &hex::decode(&file_id).unwrap().try_into().unwrap());
        let opens = open_all(&object, &sec.file_key, &sec.file_nonce).map(|p| sha256(&p) == f.sha256).unwrap_or(false);
        let attackers: Vec<String> = attacker_keys(&hex::decode(&file_id).unwrap().try_into().unwrap(), &f.sha256)
            .into_iter()
            .map(|(what, k, n)| format!("{what}: {}", if open_chunk0(&object, &k, &n).is_ok() { "OPENED" } else { "refused" }))
            .collect();
        run.push(Proof::check(
            "at rest: opens with the account's key and nothing else",
            opens && model.0 == sec.file_key && attackers.iter().all(|a| a.ends_with("refused")),
            format!(
                "real key (HKDF of the Vault root, held only on enrolled devices) opens it and the plaintext sha256 matches: {opens}; {}",
                attackers.join(", ")
            ),
        ));
        if let Kind::Duplicate(orig) = f.kind {
            let other = stored.iter().find(|s| s.idx == orig);
            let same_id = other.map(|s| s.file_id == file_id).unwrap_or(false);
            let same_ct = objects.get(&orig) == objects.get(&i);
            run.push(Proof::check(
                "at rest: identical plaintext shares nothing",
                other.is_some() && !same_id && !same_ct,
                format!(
                    "duplicate of {}: file id differs: {}, ciphertext differs: {}",
                    files[orig].label, !same_id, !same_ct
                ),
            ));
        }

        if let Some(s3) = &world.s3_tap {
            s3.settle().await;
            let path = format!("/{}/{}", cfg.s3.bucket, key);
            let puts: Vec<_> = s3.exchanges().into_iter().filter(|e| e.method == "PUT" && e.path == path).collect();
            let ok = puts.len() == 1
                && puts[0].request.clean()
                && puts[0].request.sha256 == objects[&i]
                && puts[0].request.len as u64 == want_len;
            run.push(Proof::check(
                "transit: S3 PUT body is the ciphertext",
                ok,
                match puts.first() {
                    Some(p) => format!(
                        "{} PUT(s) to {path}; body {}; byte-identical to the stored object: {}",
                        puts.len(),
                        p.request.summary(),
                        p.request.sha256 == objects[&i]
                    ),
                    None => format!("no PUT to {path} seen by the tap"),
                },
            ));
        } else {
            run.push(Proof::skip(
                "transit: S3 PUT body",
                "dev target: R2 is reached directly over TLS, no tap on that hop (the DS hop is tapped; the stored object above is what was sent)",
            ));
        }

        match &world.db_main {
            Some(db) => {
                let obj = world::rows(db, "SELECT content_hash, r2_key FROM attachment_object WHERE content_hash = ?1", vec![libsql_text(&file_id)]).await;
                let refs = world::count(db, "SELECT 1 FROM vault_attachment_ref WHERE content_hash = ?1 AND vault_message_id = ?2", vec![libsql_text(&file_id), libsql_text(&entry_id)]).await;
                let vm = world::rows(db, "SELECT * FROM vault_message WHERE id = ?1", vec![libsql_text(&entry_id)]).await;
                let mut findings = Vec::new();
                for r in obj.iter().flatten().chain(vm.iter().flatten()) {
                    for c in r {
                        findings.extend(built.describe(&built.scan(c)));
                    }
                }
                let ok = matches!(&obj, Ok(r) if r.len() == 1 && r[0][1] == key.as_bytes())
                    && matches!(refs, Ok(1))
                    && matches!(&vm, Ok(r) if r.len() == 1)
                    && findings.is_empty();
                run.push(Proof::check(
                    "DS rows hold no key, plaintext or name",
                    ok,
                    format!(
                        "attachment_object rows {:?} (content_hash = the random file id, r2_key only), vault_attachment_ref {:?}, vault_message rows {:?}; needle hits: {}",
                        obj.as_ref().map(|r| r.len()),
                        refs,
                        vm.as_ref().map(|r| r.len()),
                        if findings.is_empty() { "none".to_string() } else { findings.join("; ") }
                    ),
                ));
            }
            None => run.push(Proof::skip("DS rows", "no DS database access on this target (set TURSO_URL/TURSO_TOKEN)")),
        }

        let scope = json!({ "kind": "vault" });
        let t = Instant::now();
        let url: Result<String, String> = alice
            .invoke("get_media_url", json!({ "r2Key": key, "contentHash": file_id, "contentType": f.content_type, "sha256": sha, "keyScope": scope }))
            .await;
        let mut loopback_url = None;
        match url {
            Ok(u) if !u.is_empty() => {
                let resp = world.http.get(&u).send().await.expect("loopback GET");
                let cache_control = resp.headers().get("cache-control").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                let body = resp.bytes().await.expect("loopback body");
                run.view_secs = Some(t.elapsed().as_secs_f64());
                run.push(Proof::check(
                    "transient: viewing serves the plaintext from memory, never storable",
                    sha256(&body) == f.sha256 && cache_control.contains("no-store"),
                    format!("loopback {} B, sha256 matches: {}; Cache-Control: {cache_control}", body.len(), sha256(&body) == f.sha256),
                ));
                loopback_url = Some(u);
                // Encrypted under this device's DB key.
                let cached = pollis_core::commands::r2::find_cached_file(&file_id);
                match cached {
                    Some((path, _)) => {
                        let bytes = std::fs::read(&path).expect("read cache file");
                        let v = BlobVerdict::of(&built, &bytes);
                        let opens = pollis_core::commands::r2::cache_decrypt(&bytes, &db_key, file_id.as_bytes())
                            .map(|p| sha256(&p) == f.sha256)
                            .unwrap_or(false);
                        let mut wrong = db_key.clone();
                        wrong[0] ^= 0xff;
                        let wrong_opens = pollis_core::commands::r2::cache_decrypt(&bytes, &wrong, file_id.as_bytes()).is_ok();
                        run.push(Proof::check(
                            "local: media cache entry is encrypted at rest",
                            v.clean() && bytes.len() as u64 == f.size + 28 && opens && !wrong_opens,
                            format!(
                                "{}: {}; opens with the device DB key: {opens}; with a wrong key: {}",
                                path.strip_prefix(&world.app_dir).unwrap_or(&path).display(),
                                v.summary(),
                                if wrong_opens { "OPENED" } else { "refused" }
                            ),
                        ));
                    }
                    None => run.push(Proof::check("local: media cache entry is encrypted at rest", false, "no cache entry after a view")),
                }
            }
            Ok(_) => {
                // Over the per-file cache cap: byte path, nothing cached.
                let bytes = pollis_core::commands::r2::download_media(
                    key.clone(),
                    file_id.clone(),
                    sha.clone(),
                    pollis_core::commands::r2::AttachmentKeyScope::Vault,
                    &alice.state,
                )
                .await;
                run.view_secs = Some(t.elapsed().as_secs_f64());
                let ok = bytes.as_ref().map(|b| sha256(b) == f.sha256).unwrap_or(false);
                run.push(Proof::check(
                    "transient: viewing serves the plaintext from memory, never storable",
                    ok,
                    format!("over the {} per-file cache cap: byte path, memory only; sha256 matches: {ok}", human(cache_cap)),
                ));
                let cached = pollis_core::commands::r2::find_cached_file(&file_id);
                run.push(Proof::check(
                    "local: media cache entry is encrypted at rest",
                    cached.is_none(),
                    "nothing written to the cache for a file over the per-file cap".to_string(),
                ));
            }
            Err(e) => run.push(Proof::check("transient: viewing", false, format!("get_media_url: {e}"))),
        }

        let target = cfg.workdir.join("saved").join(&f.filename);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        alice.pick_save_target(&target);
        let t = Instant::now();
        let saved: Result<String, String> = alice
            .invoke(
                "save_media_to_path",
                json!({ "r2Key": key, "contentHash": file_id, "sha256": sha, "keyScope": { "kind": "vault" }, "targetPath": target.to_string_lossy() }),
            )
            .await;
        run.save_secs = Some(t.elapsed().as_secs_f64());
        let back = std::fs::read(&target).map(|b| sha256(&b)).ok();
        run.push(Proof::check(
            "round trip: re-download is byte-identical",
            saved.is_ok() && back == Some(f.sha256),
            format!(
                "save_media_to_path → {}; sha256(saved) == sha256(original): {}",
                saved.as_deref().map(|m| format!("ok (provenance marker: {m})")).unwrap_or_else(|e| format!("error {e}")),
                back == Some(f.sha256)
            ),
        ));
        // A user-requested export, so the soak cleans it up.
        let _ = std::fs::remove_file(&target);

        if let Some(s3) = &world.s3_tap {
            s3.settle().await;
            let path = format!("/{}/{}", cfg.s3.bucket, key);
            let gets: Vec<_> = s3.exchanges().into_iter().filter(|e| e.method == "GET" && e.path == path).collect();
            let ok = !gets.is_empty() && gets.iter().all(|g| g.status == 200 && g.response.clean() && g.response.sha256 == objects[&i]);
            run.push(Proof::check(
                "transit: S3 GET bodies are the ciphertext",
                ok,
                format!(
                    "{} GET(s) of {path}; every response byte-identical to the stored ciphertext and needle-free: {ok}",
                    gets.len()
                ),
            ));
        }

        stored.push(Stored { idx: i, entry_id, key, file_id, loopback_url });
    }

    let mut global: Vec<Proof> = Vec::new();

    world.ds_tap.settle().await;
    if let Some(s3) = &world.s3_tap {
        s3.settle().await;
    }
    sweep_checkpoint(&cfg, &world, &alice, &built, &stored, &files, started, "after upload + view", &mut global).await;

    // Lock: the loopback URLs must stop resolving.
    alice.invoke::<()>("lock", json!({})).await.expect("lock");
    let mut dead = 0;
    let mut alive = Vec::new();
    for s in &stored {
        if let Some(u) = &s.loopback_url {
            let (st, body) = plain_get(&world.http, u).await;
            if st == 403 || st == 0 {
                dead += 1;
            } else {
                alive.push(format!("{} → HTTP {st} ({} B)", files[s.idx].label, body.len()));
            }
        }
    }
    global.push(Proof::check(
        "transient: locking ends every view",
        alive.is_empty(),
        format!("{dead} loopback URL(s) answer 403 once the app is locked{}", if alive.is_empty() { String::new() } else { format!("; still serving: {}", alive.join(", ")) }),
    ));
    let locked_cache = world::walk(&world.cache_root.join(&alice.user_id)).len();
    let unlocked: Result<serde_json::Value, String> =
        alice.invoke("unlock", json!({ "userId": alice.user_id, "pin": world::PIN })).await;
    assert!(unlocked.is_ok(), "unlock: {unlocked:?}");
    let unlocked_cache = world::walk(&world.cache_root.join(&alice.user_id)).len();
    global.push(Proof::check(
        "transient: the media cache does not outlive its session",
        unlocked_cache == 0,
        format!("{locked_cache} encrypted cache file(s) stay while locked (unreadable without the DB key); the next unlock empties the cache: {unlocked_cache} left"),
    ));

    for s in &stored {
        let f = &files[s.idx];
        let run = &mut runs[s.idx];
        let t = Instant::now();
        let del: Result<(), String> =
            alice.invoke("delete_vault_message", json!({ "id": s.entry_id, "userId": alice.user_id })).await;
        run.delete_secs = Some(t.elapsed().as_secs_f64());
        if let Err(e) = del {
            run.push(Proof::check("delete", false, e));
            continue;
        }
        let (st, _) = operator_get(&world.http, &cfg.s3, &s.key).await;
        run.push(Proof::check("delete: object is gone from storage", st == 404, format!("operator GET {} → HTTP {st}", s.key)));

        match &world.db_main {
            Some(db) => {
                let a = world::count(db, "SELECT 1 FROM attachment_object WHERE content_hash = ?1", vec![libsql_text(&s.file_id)]).await;
                let r = world::count(db, "SELECT 1 FROM vault_attachment_ref WHERE content_hash = ?1", vec![libsql_text(&s.file_id)]).await;
                let m = world::count(db, "SELECT 1 FROM vault_message WHERE id = ?1", vec![libsql_text(&s.entry_id)]).await;
                run.push(Proof::check(
                    "delete: DS rows are gone",
                    matches!((&a, &r, &m), (Ok(0), Ok(0), Ok(0))),
                    format!("attachment_object {a:?}, vault_attachment_ref {r:?}, vault_message {m:?}"),
                ));
            }
            None => run.push(Proof::skip("delete: DS rows", "no DS database access on this target")),
        }

        let cached = pollis_core::commands::r2::find_cached_file(&s.file_id);
        run.push(Proof::check(
            "delete: local cache entry is gone",
            cached.is_none(),
            match cached {
                Some((p, _)) => format!("still on disk: {}", p.display()),
                None => "no cache file for the file id".to_string(),
            },
        ));

        let again = pollis_core::commands::r2::download_media(
            s.key.clone(),
            s.file_id.clone(),
            f.sha_hex(),
            pollis_core::commands::r2::AttachmentKeyScope::Vault,
            &alice.state,
        )
        .await;
        run.push(Proof::check(
            "delete: the owner's own device can no longer fetch it",
            again.is_err(),
            match again {
                Err(e) => format!("download_media → {e}"),
                Ok(b) => format!("download_media returned {} B", b.len()),
            },
        ));
    }

    let listed: Vec<serde_json::Value> =
        alice.invoke("get_vault_messages", json!({ "userId": alice.user_id })).await.unwrap_or_default();
    for s in &stored {
        let still = listed.iter().any(|m| m["id"].as_str() == Some(s.entry_id.as_str()));
        runs[s.idx].push(Proof::check("delete: entry is gone after a re-sync", !still, format!("get_vault_messages lists it: {still}")));
    }

    // Viewed after the unlock (which empties the cache) and before the fresh device moves the cache user.
    let sentinel = cfg.workdir.join("sentinel.bin");
    {
        let mut b = vec![0u8; 1024 * 1024];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut b);
        std::fs::write(&sentinel, &b).expect("write sentinel");
    }
    alice.pick_file(&sentinel);
    let sent: serde_json::Value = alice
        .invoke("upload_media", json!({ "path": sentinel.to_string_lossy(), "filename": "sentinel.bin", "contentType": "application/octet-stream" }))
        .await
        .expect("sentinel upload");
    let sentinel_content = json!({ "_att": [{
        "key": sent["key"], "url": sent["url"], "name": "sentinel.bin", "ct": "application/octet-stream",
        "size": sent["size_bytes"], "hash": sent["content_hash"], "kv": sent["key_version"], "sha": sent["sha256"],
    }]})
    .to_string();
    let _: serde_json::Value = alice
        .invoke("send_vault_message", json!({ "userId": alice.user_id, "content": sentinel_content }))
        .await
        .expect("sentinel entry");
    let sentinel_url: String = alice
        .invoke(
            "get_media_url",
            json!({ "r2Key": sent["key"], "contentHash": sent["content_hash"], "contentType": "application/octet-stream", "sha256": sent["sha256"], "keyScope": { "kind": "vault" } }),
        )
        .await
        .expect("sentinel view");
    let _ = plain_get(&world.http, &sentinel_url).await;
    let _ = std::fs::remove_file(&sentinel);

    let bob = Client::sign_up(&world, &format!("pollis-soak-{suffix}-fresh@example.com")).await;
    for s in &stored {
        let url = pollis_core::commands::r2::test_presign_get(&bob.state, &s.key).await;
        let st = match &url {
            Ok(u) => plain_get(&world.http, u).await.0,
            Err(_) => 0,
        };
        runs[s.idx].push(Proof::check(
            "delete: a fresh session cannot fetch it",
            st == 404,
            match url {
                Ok(_) => format!("fresh device's presigned GET → HTTP {st}"),
                Err(e) => format!("presign failed: {e}"),
            },
        ));
    }

    let user_cache = world.cache_root.join(&alice.user_id);
    let before = world::walk(&user_cache).len();
    let end: Result<(), String> = match cfg.target {
        Target::Local => alice.invoke("logout", json!({ "deleteData": false })).await,
        Target::Dev => alice.invoke("delete_account", json!({ "userId": alice.user_id })).await,
    };
    let after = world::walk(&user_cache);
    let mut answering = Vec::new();
    for u in stored.iter().filter_map(|s| s.loopback_url.as_ref()).chain([&sentinel_url]) {
        let (st, _) = plain_get(&world.http, u).await;
        if st != 403 && st != 0 {
            answering.push(format!("{u_short} → HTTP {st}", u_short = u.rsplit('/').next().unwrap_or("")));
        }
    }
    global.push(Proof::check(
        "transient: ending the session leaves nothing behind",
        end.is_ok() && before > 0 && after.is_empty() && answering.is_empty(),
        format!(
            "{} → {:?}; the user's media cache held {before} file(s) (a stored, viewed sentinel) and holds {} after; loopback URLs still answering: {}",
            if cfg.target == Target::Local { "logout" } else { "delete_account" },
            end,
            after.len(),
            if answering.is_empty() { "none".to_string() } else { answering.join(", ") }
        ),
    ));
    let bob_end: Result<(), String> = match cfg.target {
        Target::Local => bob.invoke("logout", json!({ "deleteData": false })).await,
        Target::Dev => bob.invoke("delete_account", json!({ "userId": bob.user_id })).await,
    };
    if let Err(e) = bob_end {
        eprintln!("[vault-soak] fresh-session teardown: {e}");
    }

    world.ds_tap.settle().await;
    if let Some(s3) = &world.s3_tap {
        s3.settle().await;
    }
    let disk = disk_sweep(&world, &built, &[]);
    global.push(Proof::check(
        "local: no plaintext or key on disk after the session",
        disk.1.is_empty(),
        format!("{} file(s) under the app data dir scanned; findings: {}", disk.0, if disk.1.is_empty() { "none".to_string() } else { disk.1.join("; ") }),
    ));
    {
        let mut by: std::collections::BTreeMap<String, Vec<(u128, u16)>> = Default::default();
        for e in world.ds_tap.exchanges() {
            by.entry(e.path.clone()).or_default().push((e.millis, e.status));
        }
        let lines: Vec<String> = by
            .into_iter()
            .map(|(path, mut v)| {
                v.sort();
                let errors = v.iter().filter(|(_, st)| *st >= 400).count();
                format!(
                    "{path} ×{} p50 {} ms, max {} ms{}",
                    v.len(),
                    v[v.len() / 2].0,
                    v.last().unwrap().0,
                    if errors > 0 { format!(", {errors} × 4xx/5xx") } else { String::new() }
                )
            })
            .collect();
        global.push(Proof::skip("DS latency (informational)", lines.join("; ")));
    }
    let (n, findings) = world.ds_tap.rescan(&built);
    global.push(Proof::check(
        "transit: every DS request and response body",
        findings.is_empty(),
        format!(
            "{} DS exchanges ({n} bodies) rescanned with every needle, including the per-file keys learned after upload; findings: {}",
            world.ds_tap.exchanges().len(),
            if findings.is_empty() { "none".to_string() } else { findings.join("; ") }
        ),
    ));
    if let Some(s3) = &world.s3_tap {
        let ex = s3.exchanges();
        let bad: Vec<String> = ex
            .iter()
            .filter(|e| !e.request.hits.is_empty() || !e.response.hits.is_empty() || e.request.magic.is_some() || (e.status == 200 && e.response.magic.is_some()))
            .map(|e| format!("{} {} → {}: req {} / resp {}", e.method, e.path, e.status, e.request.summary(), e.response.summary()))
            .collect();
        let by_method = |m: &str| ex.iter().filter(|e| e.method == m).count();
        global.push(Proof::check(
            "transit: every object-storage body",
            bad.is_empty(),
            format!(
                "{} exchanges (PUT {}, GET {}, DELETE {}); bytes up {}, down {}; findings: {}",
                ex.len(),
                by_method("PUT"),
                by_method("GET"),
                by_method("DELETE"),
                human(ex.iter().map(|e| e.request.len as u64).sum()),
                human(ex.iter().map(|e| e.response.len as u64).sum()),
                if bad.is_empty() { "none".to_string() } else { bad.join("; ") }
            ),
        ));
    }
    if let Some(db) = &world.db_main {
        if cfg.target == Target::Local {
            for (name, d) in [("main", Some(db)), ("commit log", world.db_log.as_ref())] {
                if let Some(d) = d {
                    match world::scan_all_tables(d, &built).await {
                        Ok((t, r, c, f)) => global.push(Proof::check(
                            &format!("DS: full scan of the {name} database after deletion"),
                            f.is_empty(),
                            format!("{t} tables, {r} rows, {c} cells; findings: {}", if f.is_empty() { "none".to_string() } else { f.join("; ") }),
                        )),
                        Err(e) => global.push(Proof::check(&format!("DS: full scan of the {name} database"), false, e)),
                    }
                }
            }
        }
    }

    let header = vec![
        ("Target".to_string(), match cfg.target {
            Target::Local => format!("local — in-process DS + S3 at {}", cfg.s3.endpoint),
            Target::Dev => format!("dev — {} + dev R2", cfg.dev.as_ref().map(|d| d.ds_url.clone()).unwrap_or_default()),
        }),
        ("Seed".to_string(), cfg.seed.to_string()),
        ("Sizes".to_string(), cfg.sizes.iter().map(|s| human(*s)).collect::<Vec<_>>().join(", ")),
        ("Caps".to_string(), format!("upload R2_MEDIA_MAX_BYTES {}; per-file cache {}; sealed in {} AES-256-GCM chunks", human(cap), human(cache_cap), human(proofs::CHUNK as u64))),
        ("Build".to_string(), std::env::var("POLLIS_SOAK_BUILD").unwrap_or_else(|_| "unknown (not run via scripts/vault-soak.sh)".to_string())),
        ("Wall time".to_string(), format!("{:.1}s", t_all.elapsed().as_secs_f64())),
    ];
    let md = report::render(&header, &runs, &global);
    if let Some(dir) = cfg.report.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(&cfg.report, &md).expect("write report");
    println!("{md}");
    eprintln!("[vault-soak] report: {}", cfg.report.display());

    if std::env::var("POLLIS_SOAK_KEEP").ok().as_deref() != Some("1") {
        let _ = std::fs::remove_dir_all(cfg.workdir.join("corpus"));
    }

    let failed: usize = runs.iter().map(|r| r.failed()).sum::<usize>() + global.iter().filter(|p| p.pass == Some(false)).count();
    assert_eq!(failed, 0, "{failed} vault soak proof(s) failed — see {}", cfg.report.display());
}

/// Scan every file under the app's data dir for any needle. `(files scanned, findings)`.
fn disk_sweep(world: &World, scanner: &Scanner, extra: &[std::path::PathBuf]) -> (usize, Vec<String>) {
    let mut paths = world::walk(&world.app_dir);
    paths.extend(extra.iter().cloned());
    let mut findings = Vec::new();
    for p in &paths {
        let Ok(bytes) = std::fs::read(p) else { continue };
        let hits = scanner.scan(&bytes);
        if !hits.is_empty() {
            findings.push(format!("{}: {}", p.display(), scanner.describe(&hits).join("; ")));
        }
    }
    (paths.len(), findings)
}

/// Temp-dir files modified since the soak started, excluding its own workdir.
fn temp_sweep(cfg: &SoakConfig, scanner: &Scanner, since: SystemTime) -> (usize, u64, Vec<String>) {
    let tmp = std::env::temp_dir();
    let work = cfg.workdir.canonicalize().unwrap_or_else(|_| cfg.workdir.clone());
    let (mut n, mut bytes) = (0usize, 0u64);
    let mut findings = Vec::new();
    for p in world::walk(&tmp) {
        let canon = p.canonicalize().unwrap_or_else(|_| p.clone());
        if canon.starts_with(&work) {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&p) else { continue };
        if meta.modified().map(|m| m < since).unwrap_or(true) || meta.len() > 4 * 1024 * 1024 * 1024 {
            continue;
        }
        let Ok(data) = std::fs::read(&p) else { continue };
        n += 1;
        bytes += data.len() as u64;
        let hits: Vec<usize> = scanner.scan(&data).into_iter().filter(|&h| !scanner.needles()[h].secret).collect();
        if !hits.is_empty() {
            findings.push(format!("{}: {}", p.display(), scanner.describe(&hits).join("; ")));
        }
    }
    (n, bytes, findings)
}

#[allow(clippy::too_many_arguments)]
async fn sweep_checkpoint(
    cfg: &SoakConfig,
    world: &World,
    alice: &Client,
    scanner: &Scanner,
    stored: &[Stored],
    files: &[SoakFile],
    since: SystemTime,
    when: &str,
    global: &mut Vec<Proof>,
) {
    let (n, findings) = disk_sweep(world, scanner, &[]);
    let cached = world::walk(&world.cache_root).len();
    global.push(Proof::check(
        &format!("local: no plaintext or key anywhere on disk ({when})"),
        findings.is_empty(),
        format!("{n} file(s) under the app data dir scanned ({cached} media cache entries); findings: {}", if findings.is_empty() { "none".to_string() } else { findings.join("; ") }),
    ));
    let (tn, tb, tf) = temp_sweep(cfg, scanner, since);
    global.push(Proof::check(
        &format!("transient: no decrypted copy in the system temp dir ({when})"),
        tf.is_empty(),
        format!("{tn} file(s), {} written since the soak began, scanned; findings: {}", human(tb), if tf.is_empty() { "none".to_string() } else { tf.join("; ") }),
    ));

    // Positive control: the vault entries really are inside the DB.
    let db_path = world.app_dir.join(format!("pollis_{}.db", alice.user_id));
    let raw = std::fs::read(&db_path).unwrap_or_default();
    let wal = std::fs::metadata(world.app_dir.join(format!("pollis_{}.db-wal", alice.user_id))).map(|m| m.len()).unwrap_or(0);
    let header_plain = raw.starts_with(b"SQLite format 3\0");
    let key = alice.db_key().await.unwrap_or_default();
    let inside = {
        let conn = rusqlite::Connection::open(&db_path).expect("open sqlcipher db");
        conn.execute_batch(&format!("PRAGMA key = \"x'{}'\"", hex::encode(&key))).expect("key");
        let mut stmt = conn.prepare("SELECT content FROM vault_entry_cache").expect("prepare");
        let all: Vec<String> = stmt.query_map([], |r| r.get::<_, String>(0)).expect("query").flatten().collect();
        stored.iter().filter(|s| all.iter().any(|c| c.contains(&s.file_id) && c.contains(&files[s.idx].filename))).count()
    };
    let wrong_key_opens = {
        let conn = rusqlite::Connection::open(&db_path).expect("open sqlcipher db");
        let mut k = key.clone();
        if let Some(b) = k.first_mut() {
            *b ^= 0xff;
        }
        conn.execute_batch(&format!("PRAGMA key = \"x'{}'\"", hex::encode(&k))).expect("key");
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0)).is_ok()
    };
    global.push(Proof::check(
        &format!("local: the SQLCipher DB is encrypted at rest ({when})"),
        !header_plain && inside == stored.len() && !wrong_key_opens,
        format!(
            "{} ({} B + {wal} B WAL, both in the sweep above): plaintext SQLite header: {header_plain}; opened with the device key it holds {inside}/{} stored entries (file id + name); a wrong key opens it: {wrong_key_opens}",
            db_path.file_name().unwrap().to_string_lossy(),
            raw.len(),
            stored.len()
        ),
    ));
}
