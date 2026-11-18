//! The subcommands, over a [`BotHost`]. Every failure names one fix.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};

use crate::config::{RuntimeConfig, WebhookConfig};
use crate::host::{BotHost, PairOutcome, PairTarget, PairingProgress, Wakes};
use crate::installations::{check_capacity, InstallationRecord, Registry};
use crate::job::Installation;
use crate::log;
use crate::ratelimit::TokenBucket;
use crate::runtime::Runtime;
use crate::secrets;
use crate::status::{StatusBoard, StatusFile};
use crate::webhook::{router, HookEntry, JobSubmitter, WebhookState};

/// `pair` and `add-host`: file the request, show the code, wait, save the installation.
pub async fn pair(
    host: &dyn BotHost,
    data_dir: &Path,
    config: &RuntimeConfig,
    target: PairTarget,
    name: Option<String>,
) -> Result<InstallationRecord> {
    let registry = Registry::new(data_dir);
    let existing = registry.list()?;
    if let PairTarget::ExistingBot { bot_user_id } = &target {
        if existing.iter().any(|r| &r.bot_user_id == bot_user_id) {
            bail!("this host already runs bot {bot_user_id}. Fix: run `pollis-bot add-host` on a different machine");
        }
    }
    check_capacity(existing.len() + 1, config.max_installations)?;

    log::info("pairing", "filing a pairing request");
    let progress = |p: PairingProgress| match p {
        PairingProgress::WaitingForApproval(ticket) => {
            let expires = chrono::DateTime::from_timestamp(ticket.expires_at, 0)
                .map(|t| t.format("%H:%M UTC").to_string())
                .unwrap_or_default();
            let place = match &target {
                PairTarget::NewBot => "open the group, then Bots → Add a bot",
                PairTarget::ExistingBot { .. } => "open the group, then Bots → the bot → Add a host",
            };
            println!();
            println!("    Pairing code:  {}", ticket.code);
            println!();
            println!("A group admin types this code in Pollis: {place}. It expires at {expires}.");
            log::info("pairing", "waiting for approval");
        }
        PairingProgress::JoiningGroup => log::info("pairing", "approved; joining the bot's group"),
    };
    match host.pair(data_dir, target.clone(), name, &progress).await? {
        PairOutcome::Expired => Err(anyhow!(
            "the code expired before an admin typed it. Fix: run `pollis-bot {}` again and type the new code before it expires",
            match target {
                PairTarget::NewBot => "pair",
                PairTarget::ExistingBot { .. } => "add-host",
            }
        )),
        PairOutcome::Paired(record) => {
            registry.save(&record)?;
            log::info(
                &log::scope(&record.installation_id),
                format!("paired: bot {} in group {}. Start it with `pollis-bot run`", record.bot_user_id, record.group_id),
            );
            Ok(record)
        }
    }
}

/// The effective webhook config for this run: the flag wins over the file.
pub fn effective_webhook(config: &RuntimeConfig, flag: Option<Option<SocketAddr>>) -> Option<WebhookConfig> {
    match flag {
        None => config.webhook.clone(),
        Some(addr) => {
            let mut w = config.webhook.clone().unwrap_or_default();
            if let Some(addr) = addr {
                w.bind = addr;
            }
            Some(w)
        }
    }
}

/// `run`: check every installation, start the webhook if configured and the
/// wake sources, catch up, then wait for events until stopped. With `once`:
/// catch up and handle everything pending, then return (a host that is woken,
/// does its work and goes back to sleep).
pub async fn run(
    host: &dyn BotHost,
    data_dir: &Path,
    config: &RuntimeConfig,
    webhook: Option<WebhookConfig>,
    once: bool,
) -> Result<()> {
    let registry = Registry::new(data_dir);
    let records = registry.list()?;
    if records.is_empty() {
        bail!("no installations on this host. Fix: run `pollis-bot pair` and have a group admin type the code");
    }
    check_capacity(records.len(), config.max_installations)?;

    let status = Arc::new(StatusBoard::new(Some(data_dir), webhook.as_ref().map(|w| w.bind.to_string())));
    let mut installations = Vec::new();
    let mut slots = HashMap::new();
    for record in &records {
        let scope = log::scope(&record.installation_id);
        log::info(&scope, format!("checking (bot {} in group {})", record.bot_user_id, record.group_id));
        let dir = registry.dir_for(&record.installation_id)?;
        let opened = host.open(record, &dir).await.with_context(|| {
            format!(
                "opening installation {id}. Fix: if this host was revoked or the bot deleted, run `pollis-bot forget --installation {id}` (then `pair` or `add-host` again if the bot should stay)",
                id = record.installation_id
            )
        })?;
        slots.insert(record.installation_id.clone(), opened.secrets);
        let handler = record
            .handler
            .build()
            .with_context(|| format!("loading the handler of installation {}", record.installation_id))?;
        installations.push(Arc::new(Installation::new(opened.core, handler)));
    }
    let runtime = Runtime::new(installations, config.max_installations, status)?;

    if once {
        log::info("runtime", "one pass: catching up and handling what is pending");
        let ok = runtime.run_once().await;
        log::info("runtime", "one pass done");
        if !ok {
            bail!("a job failed (see above). Fix: the failure line above names what to do; the next run retries");
        }
        return Ok(());
    }

    // The wake source only says when to run a job; it never runs one.
    for record in &records {
        let dir = registry.dir_for(&record.installation_id)?;
        let wakes = Wakes {
            wake: {
                let (rt, id) = (Arc::downgrade(&runtime), record.installation_id.clone());
                Arc::new(move || {
                    if let Some(rt) = rt.upgrade() {
                        rt.wake(&id);
                    }
                })
            },
            approval: {
                let (rt, id) = (Arc::downgrade(&runtime), record.installation_id.clone());
                Arc::new(move || {
                    if let Some(rt) = rt.upgrade() {
                        rt.approvals(&id);
                    }
                })
            },
        };
        if let Err(e) = host.watch(record, &dir, wakes).await {
            log::failure(
                &log::scope(&record.installation_id),
                format!("starting realtime wake-ups: {e:#}"),
                "check this host can reach the Delivery Service; until then it catches up on webhook calls and at the next start",
            );
        }
    }

    if let Some(w) = &webhook {
        let mut hooks = HashMap::new();
        for (id, slot) in &slots {
            let (secret, created) = secrets::ensure(slot.as_ref(), id).await?;
            if created {
                println!(
                    "Webhook secret for installation {id} (shown once; give it to your alert sender):\n    {}",
                    String::from_utf8_lossy(&secret)
                );
            }
            let mut entry = HookEntry::new(secret, TokenBucket::new(w.per_minute, w.burst, Instant::now()));
            // An App's webhook gets the request as sent, not the alerts JSON.
            if records
                .iter()
                .any(|r| &r.installation_id == id && matches!(r.handler, crate::handler::HandlerKind::App { .. }))
            {
                entry = entry.raw();
            }
            hooks.insert(id.clone(), entry);
        }
        let submitter: Arc<dyn JobSubmitter> = Arc::new(runtime.clone());
        let app = router(Arc::new(WebhookState::new(hooks, submitter)));
        let listener = tokio::net::TcpListener::bind(w.bind).await.with_context(|| {
            format!("binding the webhook listener to {}. Fix: choose a free address in config.json `webhook.bind` or `run --webhook ADDR`", w.bind)
        })?;
        if !w.bind.ip().is_loopback() {
            log::info("webhook", format!("listening on {}, reachable beyond this machine: callers still need the secret", w.bind));
        } else {
            log::info("webhook", format!("listening on {}", w.bind));
        }
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                log::failure("webhook", format!("listener stopped: {e}"), "restart `pollis-bot run`");
            }
        });
    } else {
        log::info("webhook", "off (enable with config.json `webhook` or `run --webhook`)");
    }

    // A host that was asleep catches up on everything missed, approvals included.
    runtime.start_all();
    shutdown_signal().await;
    log::info("runtime", "stopping");
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// `post`: send through the running runtime's own webhook endpoint.
pub async fn post(
    host: &dyn BotHost,
    data_dir: &Path,
    config: &RuntimeConfig,
    installation_id: &str,
    channel: &str,
    text: &str,
) -> Result<serde_json::Value> {
    let registry = Registry::new(data_dir);
    let record = registry
        .get(installation_id)?
        .ok_or_else(|| anyhow!("no installation {installation_id} on this host. Fix: use an id from `pollis-bot status`"))?;
    // The running runtime's bind (it may come from `run --webhook`), else the file's.
    let bind: SocketAddr = StatusFile::read(data_dir)
        .and_then(|s| s.webhook_bind)
        .and_then(|b| b.parse().ok())
        .or_else(|| config.webhook.as_ref().map(|w| w.bind))
        .ok_or_else(|| anyhow!("the webhook is off. Fix: add `\"webhook\": {{}}` to config.json (or `run --webhook`) and restart `pollis-bot run`"))?;
    let dir = registry.dir_for(&record.installation_id)?;
    let slot = host.secrets(&record, &dir).await?;
    let secret = slot.load(installation_id).await?.ok_or_else(|| {
        anyhow!("installation {installation_id} has no webhook secret yet. Fix: start `pollis-bot run` with the webhook on once")
    })?;
    let answer = crate::post::post(bind, installation_id, &secret, channel, text).await?;
    if !answer.posted() {
        let detail = answer.body.get("fix").or_else(|| answer.body.get("refused")).cloned().unwrap_or_default();
        bail!("the runtime answered {} {}: {detail}", answer.status, answer.body.get("status").cloned().unwrap_or_default());
    }
    Ok(answer.body)
}

/// `forget`: remove an installation from this host — its record, its store and
/// its keystore slots. For a revoked host or a deleted bot; the bot itself is
/// unchanged on the DS (an admin revokes or deletes it in Pollis).
pub async fn forget(host: &dyn BotHost, data_dir: &Path, installation_id: &str) -> Result<()> {
    let registry = Registry::new(data_dir);
    let record = registry
        .get(installation_id)?
        .ok_or_else(|| anyhow!("no installation {installation_id} on this host. Fix: use an id from `pollis-bot status`"))?;
    let dir = registry.dir_for(&record.installation_id)?;
    host.forget(&record, &dir).await?;
    std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
    log::info(&log::scope(installation_id), "forgotten: its record, store and keys are gone from this host");
    Ok(())
}

/// `rotate-secret`: a new secret, printed once. The running runtime picks it up on restart.
pub async fn rotate_secret(host: &dyn BotHost, data_dir: &Path, installation_id: &str) -> Result<String> {
    let registry = Registry::new(data_dir);
    let record = registry
        .get(installation_id)?
        .ok_or_else(|| anyhow!("no installation {installation_id} on this host. Fix: use an id from `pollis-bot status`"))?;
    let dir = registry.dir_for(&record.installation_id)?;
    let slot = host.secrets(&record, &dir).await?;
    let secret = secrets::rotate(slot.as_ref(), installation_id).await?;
    Ok(String::from_utf8_lossy(&secret).into_owned())
}

/// `status`: the installations on disk and, if `run` has written one, its status.
pub fn status(data_dir: &Path, config: &RuntimeConfig) -> Result<String> {
    use std::fmt::Write as _;
    let records = Registry::new(data_dir).list()?;
    let running = StatusFile::read(data_dir);
    let mut out = String::new();
    let _ = writeln!(out, "data dir: {}", data_dir.display());
    let _ = writeln!(out, "installations: {} of at most {}", records.len(), config.max_installations);
    match &running {
        Some(s) => {
            let _ = writeln!(
                out,
                "runtime: last reported by pid {} (started {}); webhook {}",
                s.pid,
                s.started_at,
                s.webhook_bind.as_deref().unwrap_or("off")
            );
        }
        None => {
            let _ = writeln!(out, "runtime: has not run yet (start it with `pollis-bot run`)");
        }
    }
    if records.is_empty() {
        let _ = writeln!(out, "\nno installations. Fix: `pollis-bot pair`");
    }
    for r in &records {
        let _ = writeln!(out, "\n{}  bot {}  group {}  handler {:?}", r.installation_id, r.bot_user_id, r.group_id, r.handler);
        if let Some(st) = running.as_ref().and_then(|s| s.installations.get(&r.installation_id)) {
            let _ = writeln!(out, "    {} since {}", st.state.label(), st.since);
            if let (Some(e), Some(f)) = (&st.last_error, &st.fix) {
                let _ = writeln!(out, "    last error: {e}\n    fix: {f}");
            }
        }
    }
    Ok(out)
}
