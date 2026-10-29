//! The soak's two worlds, its client, and operator's-eye views of storage and the DS DB.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pollis_delivery::db::Db;
use pollis_lib::config::Config;
use pollis_lib::keystore::{InMemoryKeystore, Keystore};
use pollis_lib::state::AppState;
use pollis_lib::test_harness::{bootstrap_log_schema, bootstrap_schema, build_client_app, drop_log_tables_from_main, invoke};
use serde_json::json;
use tauri::test::MockRuntime;
use tauri::{App, Manager, WebviewWindow};

use crate::config::{SoakConfig, Target, S3};
use crate::proofs::Scanner;
use crate::tap::Tap;

pub const LOCAL_OTP: &str = "000000";
pub const PIN: &str = "0000";

pub struct World {
    pub config: Config,
    pub otp: String,
    pub ds_tap: Arc<Tap>,
    /// `None` on the dev target: R2 is reached directly.
    pub s3_tap: Option<Arc<Tap>>,
    /// The DS's own database handles (local), or read-only Turso (dev).
    pub db_main: Option<Arc<Db>>,
    pub db_log: Option<Arc<Db>>,
    pub app_dir: PathBuf,
    pub cache_root: PathBuf,
    pub http: soak_reqwest::Client,
}

pub async fn setup(cfg: &SoakConfig, scanner: Arc<Scanner>) -> World {
    let app_dir = cfg.workdir.join("app");
    let cache_root = app_dir.join("media-cache");
    std::fs::create_dir_all(&cache_root).expect("create app dir");
    pollis_core::db::local::set_data_dir(&app_dir);
    pollis_core::commands::r2::set_media_cache_dir(cache_root.clone());

    let http = soak_reqwest::Client::builder()
        .redirect(soak_reqwest::redirect::Policy::none())
        .build()
        .expect("http client");
    let mut config = Config::for_test().expect("Config::for_test");

    match cfg.target {
        Target::Local => {
            let ds_dir = cfg.workdir.join("ds");
            std::fs::create_dir_all(&ds_dir).expect("ds dir");
            let main = Arc::new(Db::connect_local(ds_dir.join("main.db").to_str().unwrap()).await.expect("main db"));
            let log = Arc::new(Db::connect_local(ds_dir.join("log.db").to_str().unwrap()).await.expect("log db"));
            bootstrap_schema(&main.conn().await.expect("conn")).await.expect("schema");
            bootstrap_log_schema(&log.conn().await.expect("conn")).await.expect("log schema");
            drop_log_tables_from_main(&main.conn().await.expect("conn")).await.expect("drop log tables");

            // The tap keeps the Host header the presigned URL was signed for.
            let s3_tap = Tap::new("s3", &cfg.s3.endpoint, true, false, scanner.clone());
            let s3_url = s3_tap.serve().await;

            let broker = pollis_delivery::broker::BrokerConfig {
                r2_endpoint: Some(s3_url.clone()),
                r2_region: cfg.s3.region.clone(),
                r2_bucket: Some(cfg.s3.bucket.clone()),
                r2_access_key_id: Some(cfg.s3.access_key.clone()),
                r2_secret_access_key: Some(cfg.s3.secret_key.clone()),
                ..Default::default()
            };
            let state = pollis_delivery::AppState::new_with_log_db(main.clone(), log.clone(), true)
                .with_otp_config(pollis_delivery::otp::OtpConfig {
                    resend_api_key: None,
                    dev_otp: Some(LOCAL_OTP.to_string()),
                    resend_throttle_secs: 0,
                    max_sends_per_window: u32::MAX,
                    ..pollis_delivery::otp::OtpConfig::default()
                })
                .with_ratelimit_config(pollis_delivery::ratelimit::RateLimitConfig {
                    request_otp_max: 1_000_000,
                    request_otp_window_secs: 1,
                    verify_otp_max: 1_000_000,
                    verify_otp_window_secs: 1,
                    write_max: 1_000_000,
                    write_window_secs: 1,
                    invite_redeem_max: 1_000_000,
                    invite_redeem_window_secs: 1,
                    read_max: 1_000_000,
                    read_window_secs: 1,
                    probe_max: 1_000_000,
                    probe_window_secs: 1,
                    get_max: 1_000_000,
                    get_window_secs: 1,
                })
                .with_broker_config(broker);
            let router = pollis_delivery::build_router_with_state(state);
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.expect("ds bind");
            let ds_addr = listener.local_addr().expect("ds addr");
            tokio::spawn(async move {
                let _ = axum::serve(listener, router).await;
            });

            let ds_tap = Tap::new("ds", &format!("http://{ds_addr}"), false, true, scanner);
            let ds_url = ds_tap.serve().await;

            config.pollis_delivery_url = Some(ds_url);
            config.r2_endpoint = s3_url.clone();
            config.r2_public_url = s3_url;
            World {
                config,
                otp: LOCAL_OTP.to_string(),
                ds_tap,
                s3_tap: Some(s3_tap),
                db_main: Some(main),
                db_log: Some(log),
                app_dir,
                cache_root,
                http,
            }
        }
        Target::Dev => {
            let dev = cfg.dev.as_ref().expect("dev config");
            let ds_tap = Tap::new("ds", &dev.ds_url, false, true, scanner);
            let ds_url = ds_tap.serve().await;
            config.pollis_delivery_url = Some(ds_url);
            config.dev_access_key = dev.access_key.clone();
            config.r2_endpoint = cfg.s3.endpoint.clone();
            config.r2_public_url = cfg.s3.public_url.clone();
            let db_main = match &dev.turso {
                Some((url, token)) => match Db::connect_remote(url, token).await {
                    Ok(db) => Some(Arc::new(db)),
                    Err(e) => {
                        eprintln!("[vault-soak] Turso unavailable, DS-row proofs will be skipped: {e}");
                        None
                    }
                },
                None => None,
            };
            World {
                config,
                otp: dev.otp.clone(),
                ds_tap,
                s3_tap: None,
                db_main,
                db_log: None,
                app_dir,
                cache_root,
                http,
            }
        }
    }
}

pub struct Client {
    pub _app: App<MockRuntime>,
    pub webview: WebviewWindow<MockRuntime>,
    pub state: Arc<AppState>,
    pub user_id: String,
    pub username: String,
}

impl Client {
    /// A fresh device after first launch and sign-up, with its media server running.
    pub async fn sign_up(world: &World, email: &str) -> Self {
        let keystore: Arc<dyn Keystore> = Arc::new(InMemoryKeystore::new());
        let state = Arc::new(AppState::new_with_parts(world.config.clone(), keystore));
        let (app, webview) = build_client_app(state.clone()).expect("build client app");
        let port = pollis_core::media_server::spawn(state.clone()).await.expect("media server");
        *state.media_server_port.lock().await = Some(port);

        invoke::<()>(&webview, "request_otp", json!({ "email": email })).await.expect("request_otp");
        let profile: serde_json::Value =
            invoke(&webview, "verify_otp", json!({ "email": email, "code": world.otp })).await.expect("verify_otp");
        let user_id = profile["id"].as_str().expect("user id").to_string();
        let username = profile["username"].as_str().unwrap_or_default().to_string();
        let me = Self { _app: app, webview, state, user_id, username };
        me.activate();
        me.invoke::<()>("set_pin", json!({ "newPin": PIN, "oldPin": null })).await.expect("set_pin");
        me.invoke::<serde_json::Value>("initialize_identity", json!({ "userId": me.user_id }))
            .await
            .expect("initialize_identity");
        me
    }

    /// The signed DS client uses `accounts.json`'s active user; point it at this one.
    pub fn activate(&self) {
        let _ = pollis_lib::accounts::upsert_account(&self.user_id, &self.username, None);
    }

    pub async fn invoke<T: serde::de::DeserializeOwned + Send + 'static>(&self, cmd: &str, args: serde_json::Value) -> Result<T, String> {
        self.activate();
        invoke(&self.webview, cmd, args).await
    }

    pub fn pick_file(&self, path: &Path) {
        self._app.state::<pollis_lib::pathscope::PathScope>().remember_file(path);
    }

    pub fn pick_save_target(&self, path: &Path) {
        self._app.state::<pollis_lib::pathscope::PathScope>().remember_save_target(path);
    }

    pub async fn db_key(&self) -> Option<Vec<u8>> {
        self.state.unlock.lock().await.as_ref().map(|u| u.db_key.to_vec())
    }
}

/// GET an object with the bucket's own credentials, bypassing the tap.
pub async fn operator_get(http: &soak_reqwest::Client, s3: &S3, key: &str) -> (u16, Vec<u8>) {
    let url = pollis_delivery::broker::presign_r2_url(
        &s3.endpoint,
        &s3.bucket,
        &s3.region,
        &s3.access_key,
        &s3.secret_key,
        "GET",
        key,
        300,
        &chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string(),
    );
    match http.get(&url).send().await {
        Ok(r) => {
            let status = r.status().as_u16();
            (status, r.bytes().await.map(|b| b.to_vec()).unwrap_or_default())
        }
        Err(e) => panic!("operator GET {key}: {e}"),
    }
}

pub async fn plain_get(http: &soak_reqwest::Client, url: &str) -> (u16, Vec<u8>) {
    match http.get(url).send().await {
        Ok(r) => {
            let status = r.status().as_u16();
            (status, r.bytes().await.map(|b| b.to_vec()).unwrap_or_default())
        }
        Err(_) => (0, Vec::new()),
    }
}

pub type Row = Vec<Vec<u8>>;

fn cell(v: libsql::Value) -> Vec<u8> {
    match v {
        libsql::Value::Null => Vec::new(),
        libsql::Value::Integer(i) => i.to_string().into_bytes(),
        libsql::Value::Real(r) => r.to_string().into_bytes(),
        libsql::Value::Text(t) => t.into_bytes(),
        libsql::Value::Blob(b) => b,
    }
}

pub async fn rows(db: &Db, sql: &str, params: Vec<libsql::Value>) -> Result<Vec<Row>, String> {
    let conn = db.conn().await.map_err(|e| e.to_string())?;
    let mut rs = conn.query(sql, params).await.map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    while let Some(row) = rs.next().await.map_err(|e| e.to_string())? {
        let n = row.column_count();
        let mut r = Vec::with_capacity(n as usize);
        for i in 0..n {
            r.push(cell(row.get_value(i).map_err(|e| e.to_string())?));
        }
        out.push(r);
    }
    Ok(out)
}

pub async fn count(db: &Db, sql: &str, params: Vec<libsql::Value>) -> Result<usize, String> {
    Ok(rows(db, sql, params).await?.len())
}

/// Every cell of every table in `db`: (table, rows scanned, cells, findings).
pub async fn scan_all_tables(db: &Db, scanner: &Scanner) -> Result<(usize, usize, usize, Vec<String>), String> {
    let tables = rows(db, "SELECT name FROM sqlite_master WHERE type = 'table'", vec![]).await?;
    let (mut nt, mut nr, mut nc) = (0, 0, 0);
    let mut findings = Vec::new();
    for t in tables {
        let name = String::from_utf8_lossy(&t[0]).to_string();
        nt += 1;
        let all = rows(db, &format!("SELECT * FROM \"{}\"", name.replace('"', "\"\"")), vec![]).await?;
        for r in all {
            nr += 1;
            for c in r {
                nc += 1;
                let hits = scanner.scan(&c);
                if !hits.is_empty() {
                    findings.push(format!("table {name}: {}", scanner.describe(&hits).join("; ")));
                }
            }
        }
    }
    Ok((nt, nr, nc, findings))
}

pub fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(p),
                Ok(t) if t.is_file() => out.push(p),
                _ => {}
            }
        }
    }
    out
}
