//! LOCAL DIAG ONLY (never committed). A dev-DS client WITH LiveKit realtime that mimics the desktop
//! renderer's `new_message` handling (useLiveKitRealtime.ts + notify.ts + messageWake.ts), driven by
//! command files `$SIM_CMD_DIR/<n>`.
//!
//!   desk_sim <email>      new account -> first device; existing -> enrolls (prints SIM_CODE)
//! Commands: approve <code> | dm <username> | accept-dm | connect | ingest | unread | sync
//!           | read <conv> | read-all | rr <conv> (read receipt for newest msgs) | send <conv> <text>
//!           | store | wait <s> | quit
//! A `new_message` event runs the desktop's handler: ingest, read_last_messages, shouldAlertForWake,
//! and keeps a JS-store-like `unread` map (incrementUnread) beside Rust's get_unread_counts.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use pollis_core::commands::{auth, device_enrollment, dm, groups, messages, mls, pin};
use pollis_core::config::Config;
use pollis_core::realtime::RealtimeEvent;
use pollis_core::state::AppState;

static NOWAKE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

struct Sink(tokio::sync::mpsc::UnboundedSender<RealtimeEvent>);
impl pollis_core::sink::EventSink<RealtimeEvent> for Sink {
    fn send(&self, e: RealtimeEvent) -> std::result::Result<(), String> {
        self.0.send(e).map_err(|e| e.to_string())
    }
}

fn ts() -> String {
    chrono::Utc::now().format("%H:%M:%S%.3f").to_string()
}

async fn convs(state: &Arc<AppState>, me: &str) -> Vec<(String, bool)> {
    let _ = mls::poll_mls_welcomes(state, me.to_string()).await;
    let mut out = Vec::new();
    if let Ok(d) = dm::list_dm_channels(me.to_string(), state).await {
        for c in d {
            out.push((c.id, true));
        }
    }
    if let Ok(g) = groups::list_user_groups_with_channels(me.to_string(), state).await {
        for g in g {
            for c in g.channels {
                out.push((c.id, false));
            }
        }
    }
    out
}

async fn ingest(state: &Arc<AppState>, me: &str, id: &str, is_dm: bool) {
    let r = if is_dm {
        messages::get_dm_messages(me.to_string(), id.to_string(), Some(50), None, state).await.map(|p| p.messages.len())
    } else {
        messages::get_channel_messages(me.to_string(), id.to_string(), Some(50), None, state).await.map(|p| p.messages.len())
    };
    match r {
        Ok(n) => println!("{} INGEST {id} rows={n}", ts()),
        Err(e) => println!("{} INGEST {id} error={e}", ts()),
    }
}

async fn rust_unread(state: &Arc<AppState>, me: &str) -> String {
    match messages::get_unread_counts(me.to_string(), state).await {
        Ok(v) => v.iter().map(|u| format!("{}={}", u.conversation_id, u.count)).collect::<Vec<_>>().join(" "),
        Err(e) => format!("error={e}"),
    }
}

async fn on_new_message(
    state: &Arc<AppState>,
    me: &str,
    store: &Arc<Mutex<HashMap<String, i64>>>,
    channel_id: Option<String>,
    conversation_id: Option<String>,
) {
    let Some(target) = channel_id.clone().or(conversation_id.clone()) else { return };
    // #1628 handler: the ingest reports the news; alerts = reported rows not own (nothing selected).
    let fresh = if channel_id.is_some() {
        messages::ingest_channel_envelopes(me.to_string(), target.clone(), state).await
    } else {
        messages::ingest_dm_envelopes(me.to_string(), target.clone(), state).await
    };
    let (ids, alerts): (Vec<String>, usize) = match &fresh {
        Ok(v) => (v.iter().map(|m| m.id.clone()).collect(), v.iter().filter(|m| m.sender_id != me).count()),
        Err(e) => {
            println!("{} INGEST {target} error={e}", ts());
            (Vec::new(), 0)
        }
    };
    // Unread is hydrated from Rust after every ingest; never incremented.
    hydrate(state, me, store).await;
    let ru = rust_unread(state, me).await;
    let js = format!("{:?}", store.lock().unwrap());
    println!(
        "{} WAKE conv={target} news={ids:?} WOULD_ALERT={} js_store={js} rust_unread=[{ru}]",
        ts(),
        alerts > 0,
    );
}

async fn hydrate(state: &Arc<AppState>, me: &str, store: &Arc<Mutex<HashMap<String, i64>>>) {
    if let Ok(v) = messages::get_unread_counts(me.to_string(), state).await {
        *store.lock().unwrap() = v.into_iter().map(|u| (u.conversation_id, u.count)).collect();
    }
}

async fn on_nudge(state: &Arc<AppState>, me: &str, store: &Arc<Mutex<HashMap<String, i64>>>) {
    let moved = messages::refresh_read_cursors(me.to_string(), state).await;
    hydrate(state, me, store).await;
    let ru = rust_unread(state, me).await;
    let js = format!("{:?}", store.lock().unwrap());
    println!(
        "{} NUDGE refresh moved={:?} js_store={js} rust_unread=[{ru}]",
        ts(),
        moved.map_err(|e| e.to_string()),
    );
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let email = std::env::args().nth(1).context("usage: desk_sim <email>")?;
    let dir: PathBuf = std::env::var("SIM_CMD_DIR").context("SIM_CMD_DIR")?.into();
    let config = Config::from_env().context("POLLIS_DELIVERY_URL")?;
    let ds = config.pollis_delivery_url.clone().unwrap_or_default();
    if !ds.contains("api-dev") {
        bail!("dev DS only");
    }
    let state = Arc::new(AppState::new(config).await.context("AppState")?);
    let resumed = if std::env::var("SIM_RESUME").is_ok() {
        auth::get_session(&state).await?
    } else {
        None
    };
    let me = if let Some(p) = resumed {
        pin::unlock(&state, p.id.clone(), "0000".into()).await?;
        println!("{} SIM_RESUMED (relaunch)", ts());
        p
    } else {
        auth::request_otp(&state, email.clone()).await?;
        auth::verify_otp(&state, email.clone(), "000000".into()).await?
    };
    if std::env::var("SIM_RESUME").is_ok() {
        // nothing: already unlocked
    } else if me.enrollment_required {
        let h = device_enrollment::start_device_enrollment(&state, me.id.clone()).await?;
        println!("SIM_CODE={}", h.verification_code);
        let st = device_enrollment::await_enrollment_approval(&state, h.request_id.clone()).await?;
        println!("SIM_ENROLL={st:?}");
        pin::set_pin(&state, None, "0000".into()).await?;
        device_enrollment::finalize_device_enrollment(&state, me.id.clone()).await?;
        auth::initialize_identity(&state, me.id.clone()).await?;
    } else {
        pin::set_pin(&state, None, "0000".into()).await?;
        auth::initialize_identity(&state, me.id.clone()).await?;
    }
    println!("SIM_READY user={} handle={}", me.id, me.username);
    let me_id = me.id.clone();

    let store: Arc<Mutex<HashMap<String, i64>>> = Arc::new(Mutex::new(HashMap::new()));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RealtimeEvent>();
    pollis_core::commands::livekit::subscribe_realtime(Arc::new(Sink(tx)), &state).await?;
    {
        let state = Arc::clone(&state);
        let store = Arc::clone(&store);
        let me = me_id.clone();
        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                let json = serde_json::to_string(&ev).unwrap_or_default();
                println!("{} EVENT {json}", ts());
                if NOWAKE.load(std::sync::atomic::Ordering::SeqCst) {
                    continue;
                }
                match ev {
                    RealtimeEvent::NewMessage { channel_id, conversation_id } => {
                        on_new_message(&state, &me, &store, channel_id, conversation_id).await;
                    }
                    RealtimeEvent::ReadCursorsChanged => on_nudge(&state, &me, &store).await,
                    RealtimeEvent::RealtimeReconnected { .. } => on_nudge(&state, &me, &store).await,
                    _ => {}
                }
            }
        });
    }

    let mut n = 1u32;
    loop {
        let path = dir.join(n.to_string());
        let started = Instant::now();
        while !path.exists() {
            if started.elapsed() > Duration::from_secs(4 * 3600) {
                bail!("idle");
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        let line = std::fs::read_to_string(&path)?.trim().to_string();
        let (cmd, rest) = line.split_once(' ').unwrap_or((line.as_str(), ""));
        println!("{} CMD {n}: {line}", ts());
        let r: Result<()> = async {
            match cmd {
                "quit" => std::process::exit(0),
                "approve" => {
                    let reqs = device_enrollment::list_pending_enrollment_requests(&state, me_id.clone()).await?;
                    let r = reqs.first().context("no pending request")?;
                    device_enrollment::approve_device_enrollment(&state, r.request_id.clone(), rest.to_string()).await?;
                    println!("APPROVED");
                }
                "dm" => {
                    let other = pollis_core::commands::user::search_user_by_username(rest.to_string(), &state)
                        .await?
                        .context("no such user")?;
                    let ch = dm::create_dm_channel(me_id.clone(), vec![me_id.clone(), other.id.clone()], &state).await?;
                    println!("DM_ID={}", ch.id);
                }
                "accept-dm" => {
                    let started = Instant::now();
                    loop {
                        if let Some(r) = dm::list_dm_requests(me_id.clone(), &state).await?.into_iter().next() {
                            dm::accept_dm_request(r.id.clone(), me_id.clone(), &state).await?;
                            println!("ACCEPTED {}", r.id);
                            break;
                        }
                        if started.elapsed() > Duration::from_secs(300) {
                            bail!("no DM request");
                        }
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                }
                "connect" => {
                    let mut ids: Vec<String> = Vec::new();
                    if let Ok(g) = groups::list_user_groups_with_channels(me_id.clone(), &state).await {
                        ids.extend(g.into_iter().map(|g| g.id));
                    }
                    if let Ok(d) = dm::list_dm_channels(me_id.clone(), &state).await {
                        ids.extend(d.into_iter().map(|c| c.id));
                    }
                    ids.push(format!("inbox-{}", me_id));
                    println!("CONNECT {} rooms", ids.len());
                    pollis_core::commands::livekit::connect_rooms(ids, me_id.clone(), String::new(), &state).await?;
                    println!("CONNECTED");
                }
                "launch" => {
                    // AppShell mount on desktop: hydrate, sync, catch_up_all_mls_groups, connect_rooms — concurrently.
                    let v = messages::get_unread_counts(me_id.clone(), &state).await?;
                    *store.lock().unwrap() = v.into_iter().map(|u| (u.conversation_id, u.count)).collect();
                    println!("{} LAUNCH hydrate rust=[{}]", ts(), rust_unread(&state, &me_id).await);
                    let (s1, s2, s3) = (Arc::clone(&state), Arc::clone(&state), Arc::clone(&state));
                    let (u1, u2, u3) = (me_id.clone(), me_id.clone(), me_id.clone());
                    let a = tokio::spawn(async move {
                        let r = messages::sync_read_cursors(u1, &s1).await;
                        println!("{} LAUNCH sync {:?}", ts(), r.map_err(|e| e.to_string()));
                    });
                    let b = tokio::spawn(async move {
                        let r = mls::catch_up_all_mls_groups(&s2, &u2).await;
                        println!("{} LAUNCH catch_up {:?}", ts(), r.map_err(|e| e.to_string()));
                    });
                    let c = tokio::spawn(async move {
                        let mut ids: Vec<String> = Vec::new();
                        if let Ok(g) = groups::list_user_groups_with_channels(u3.clone(), &s3).await {
                            ids.extend(g.into_iter().map(|g| g.id));
                        }
                        if let Ok(d) = dm::list_dm_channels(u3.clone(), &s3).await {
                            ids.extend(d.into_iter().map(|c| c.id));
                        }
                        ids.push(format!("inbox-{}", u3));
                        let r = pollis_core::commands::livekit::connect_rooms(ids, u3, String::new(), &s3).await;
                        println!("{} LAUNCH connect {:?}", ts(), r.map_err(|e| e.to_string()));
                    });
                    let _ = tokio::join!(a, b, c);
                    println!("{} LAUNCH done rust=[{}]", ts(), rust_unread(&state, &me_id).await);
                }
                "ingest" => {
                    for (id, d) in convs(&state, &me_id).await {
                        ingest(&state, &me_id, &id, d).await;
                    }
                    println!("{} UNREAD rust=[{}]", ts(), rust_unread(&state, &me_id).await);
                }
                "unread" => {
                    let ru = rust_unread(&state, &me_id).await;
                    let js = format!("{:?}", store.lock().unwrap());
                    println!("{} UNREAD rust=[{ru}] js_store={js}", ts());
                }
                "store" => {
                    // useHydrateUnread: whole-set replace from Rust.
                    let v = messages::get_unread_counts(me_id.clone(), &state).await?;
                    let js = {
                        let mut s = store.lock().unwrap();
                        *s = v.into_iter().map(|u| (u.conversation_id, u.count)).collect();
                        format!("{:?}", s)
                    };
                    println!("{} HYDRATE js_store={js}", ts());
                }
                "sync" => {
                    match messages::sync_read_cursors(me_id.clone(), &state).await {
                        Ok(m) => println!("{} SYNC moved={m}", ts()),
                        Err(e) => println!("{} SYNC error={e}", ts()),
                    }
                    println!("{} UNREAD rust=[{}]", ts(), rust_unread(&state, &me_id).await);
                }
                "read" => {
                    let moved = messages::mark_conversation_read(rest.to_string(), me_id.clone(), &state).await?;
                    println!("{} READ {rest} moved={moved}", ts());
                    tokio::time::sleep(Duration::from_secs(8)).await;
                }
                "read-all" => {
                    for (id, d) in convs(&state, &me_id).await {
                        ingest(&state, &me_id, &id, d).await;
                        let moved = messages::mark_conversation_read(id.clone(), me_id.clone(), &state).await?;
                        println!("{} READ {id} moved={moved}", ts());
                    }
                    tokio::time::sleep(Duration::from_secs(8)).await;
                }
                "rr" => {
                    let page = messages::get_dm_messages(me_id.clone(), rest.to_string(), Some(50), None, &state).await?;
                    let ids: Vec<String> = page.messages.iter().filter(|m| m.sender_id != me_id).map(|m| m.id.clone()).collect();
                    messages::mark_messages_read(rest.to_string(), me_id.clone(), ids.clone(), &state).await?;
                    println!("{} READ_RECEIPT {rest} n={}", ts(), ids.len());
                }
                "send" => {
                    let (conv, text) = rest.split_once(' ').context("send <conv> <text>")?;
                    let m = messages::send_message(conv.to_string(), me_id.clone(), text.to_string(), None, None, Some(me.username.clone()), &state).await?;
                    println!("{} SENT {}", ts(), m.id);
                }
                "nowake" => {
                    NOWAKE.store(rest != "off", std::sync::atomic::Ordering::SeqCst);
                    println!("NOWAKE {}", rest != "off");
                }
                "dual" => {
                    // Two overlapping ingests of one DM, as a wake-driven ingest racing a send's catch-up.
                    let (a, b) = tokio::join!(
                        messages::get_dm_messages(me_id.clone(), rest.to_string(), Some(50), None, &state),
                        messages::get_dm_messages(me_id.clone(), rest.to_string(), Some(50), None, &state)
                    );
                    println!("{} DUAL a={} b={}", ts(), a.is_ok(), b.is_ok());
                }
                "devices" => {
                    for d in auth::list_user_devices(&state, me_id.clone()).await? {
                        println!("DEVICE {d}");
                    }
                }
                "revoke" => {
                    auth::revoke_device(&state, me_id.clone(), rest.to_string()).await?;
                    println!("REVOKED {rest}");
                }
                "edit" => {
                    // edit <conv> <text>: edits this user's newest own message there
                    let (conv, text) = rest.split_once(' ').context("edit <conv> <text>")?;
                    let page = messages::get_dm_messages(me_id.clone(), conv.to_string(), Some(50), None, &state).await?;
                    let mine = page.messages.iter().find(|m| m.sender_id == me_id).context("no own message")?;
                    pollis_core::commands::messages::edit_message(conv.to_string(), mine.id.clone(), me_id.clone(), text.to_string(), &state).await?;
                    println!("{} EDITED {}", ts(), mine.id);
                }
                "react" => {
                    let page = messages::get_dm_messages(me_id.clone(), rest.to_string(), Some(50), None, &state).await?;
                    let m = page.messages.first().context("no message")?;
                    pollis_core::commands::messages::add_reaction(m.id.clone(), me_id.clone(), "👍".into(), &state).await?;
                    println!("{} REACTED {}", ts(), m.id);
                }
                "rows" => {
                    // rows <conv>: dump what this device holds (ids, sender, content length, edited/undecryptable)
                    let page = messages::get_dm_messages(me_id.clone(), rest.to_string(), Some(100), None, &state).await?;
                    for m in page.messages.iter().rev() {
                        println!(
                            "ROW {} from_me={} sent_at={} text={:?} edited={}",
                            m.id,
                            m.sender_id == me_id,
                            m.sent_at,
                            m.content.clone().unwrap_or_default(),
                            m.edited_at.is_some()
                        );
                    }
                }
                "wait" => tokio::time::sleep(Duration::from_secs(rest.parse().unwrap_or(5))).await,
                other => bail!("unknown command {other}"),
            }
            Ok(())
        }
        .await;
        if let Err(e) = r {
            println!("{} CMD_ERROR {n}: {e}", ts());
        }
        println!("DONE {n}");
        n += 1;
    }
}
