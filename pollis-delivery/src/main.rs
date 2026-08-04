//! Pollis MLS Delivery Service entrypoint.
//!
//! Env:
//!   TURSO_URL           libsql://… (required)
//!   TURSO_TOKEN         scoped write token for the MLS tables (required)
//!   LOG_DB_URL          libsql://… for the separate commit-log DB (optional)
//!   LOG_DB_ADMIN_TOKEN  read-write token for the commit-log DB (optional)
//!   PORT                listen port (default 8788)
//!   RESEND_API_KEY      Resend key for sending sign-in OTP emails (the client no
//!                       longer ships this). Unset → OTP email is not sent.
//!   DEV_OTP             dev/harness override — skip the email send and force this
//!                       exact OTP code (optional).
//!   OTP_TTL_SECS        OTP lifetime in seconds (optional, default 600).
//!
//! `RESEND_API_KEY` / `DEV_OTP` / `OTP_TTL_SECS` are read by
//! `OtpConfig::from_env` inside `build_router_with_log_db`.
//!
//! When both `LOG_DB_URL` and `LOG_DB_ADMIN_TOKEN` are set, the MLS
//! control-plane tables (`mls_commit_log`, `mls_group_info`, `mls_welcome`) are
//! written to/read from that separate "commit-log" database — the DS is its
//! sole writer. When they're absent, the DS falls back to the single `TURSO_*`
//! database (existing single-DB deploys and the test harness are unaffected).
//!
//! Terminate TLS at a reverse proxy in front (this serves plain HTTP), and run
//! it beside the LiveKit container behind `api.pollis.com`.

use std::sync::Arc;

use anyhow::{Context, Result};
use pollis_delivery::messages::{
    gc_sweep_secs, watermark_stale_modifier, RetentionMetricsHandle, RetentionSnapshot,
};
use pollis_delivery::{build_app_state, build_router_with_state, db::Db};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "pollis_delivery=info,tower_http=info".into()),
        )
        .init();

    let url = std::env::var("TURSO_URL").context("TURSO_URL must be set")?;
    let token = std::env::var("TURSO_TOKEN").context("TURSO_TOKEN must be set")?;
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8788);

    let db = Arc::new(
        Db::connect_remote(&url, &token)
            .await
            .context("connect to Turso")?,
    );

    // Optional separate commit-log DB. When both vars are present, the DS writes
    // the MLS control-plane tables there (it holds the read-write admin token and
    // is the sole writer). Otherwise fall back to the single `db` above.
    let log_url = std::env::var("LOG_DB_URL").ok().filter(|s| !s.is_empty());
    let log_token = std::env::var("LOG_DB_ADMIN_TOKEN")
        .ok()
        .filter(|s| !s.is_empty());
    let log_db = match (log_url, log_token) {
        (Some(log_url), Some(log_token)) => {
            tracing::info!("pollis-delivery: using separate commit-log DB (LOG_DB_URL)");
            Arc::new(
                Db::connect_remote(&log_url, &log_token)
                    .await
                    .context("connect to commit-log Turso")?,
            )
        }
        _ => {
            tracing::info!("pollis-delivery: LOG_DB_* unset — commit log shares the main TURSO DB");
            Arc::clone(&db)
        }
    };

    // Build the state first so the sweep and the router share one
    // `retention_metrics` cell: the sweep writes the growth snapshot, the
    // `/v1/retention/metrics` endpoint serves it (#720 checkbox 1).
    let state = build_app_state(Arc::clone(&db), log_db);

    // Detach the envelope-GC trigger from member activity (#689): a DS-internal
    // task sweeps every conversation on a fixed cadence. Started before the
    // router so it also runs on a scale-to-zero wake.
    spawn_envelope_gc_sweep(Arc::clone(&db), state.retention_metrics.clone());

    let app = build_router_with_state(state);

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .with_context(|| format!("bind 0.0.0.0:{port}"))?;
    tracing::info!("pollis-delivery listening on 0.0.0.0:{port}");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;
    Ok(())
}

/// Spawn the envelope-GC sweep (#689) — the server-side GC *trigger*.
///
/// Envelope GC's deletion predicate is many-member correct (bounded by the
/// slowest member device's watermark, invariant I3), but it used to *fire* only
/// from whichever member device happened to ingest. A conversation whose members
/// all went quiet therefore never ran GC, and a chatty one ran it far more than
/// needed. This sweeps every conversation with envelopes on a fixed cadence,
/// decoupling the trigger from member activity. The predicate is untouched.
///
/// A timer — not an event — because the state that makes envelopes collectible
/// (the slowest device's watermark advancing, or the roster shrinking on a
/// revocation) can settle with no further request to hang an event on, and the
/// quiet-conversation case has by definition no event to fire on. It runs inside
/// the sole-writer DS process (no new infra, secrets, or routes), and because the
/// first tick fires immediately it also catches up on every scale-to-zero wake.
/// Cadence via `POLLIS_DS_GC_SWEEP_SECS` (default 3600); `0` disables it.
///
/// The sweep also carries the #720 device-liveness window
/// (`watermark_stale_modifier`) and, on the same walk, gathers the identity-free
/// growth snapshot it stores in `metrics` and logs (#720 checkbox 1).
fn spawn_envelope_gc_sweep(db: Arc<Db>, metrics: RetentionMetricsHandle) {
    let secs = gc_sweep_secs();
    if secs == 0 {
        tracing::info!("pollis-delivery: envelope-GC sweep disabled (POLLIS_DS_GC_SWEEP_SECS=0)");
        return;
    }
    let stale = watermark_stale_modifier();
    tracing::info!("pollis-delivery: envelope-GC sweep every {secs}s (staleness window {stale})");
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(secs));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match db.conn() {
                Ok(conn) => match pollis_delivery::messages::sweep_envelope_gc(&conn, &stale).await {
                    Ok(report) => {
                        emit_retention_metrics(report.visited, &report.metrics);
                        *metrics.lock().unwrap() = Some(report.metrics);
                        // #762: aged-out audit rows and dead push tokens, on the
                        // same schedule so there is one sweep to reason about.
                        // A failure here must not lose the envelope-GC result
                        // above, so it is logged rather than propagated.
                        match pollis_delivery::messages::sweep_aged_records(
                            &conn,
                            pollis_delivery::messages::security_event_retention_days(),
                            pollis_delivery::messages::push_token_retention_days(),
                        )
                        .await {
                            Ok((events, tokens)) if events > 0 || tokens > 0 => tracing::info!(
                                "retention sweep: aged out {events} security_event, {tokens} push_token row(s)"
                            ),
                            Ok(_) => {}
                            Err(e) => tracing::warn!("aged-record sweep failed: {e}"),
                        }
                    }
                    Err(e) => tracing::warn!("envelope-GC sweep failed: {e}"),
                },
                Err(e) => tracing::warn!("envelope-GC sweep: DB connection failed: {e}"),
            }
        }
    });
}

/// Log the identity-free growth snapshot from a sweep (#720 checkbox 1). This is
/// the source of truth the owner scrapes; deliberately carries NO conversation or
/// user id — only counts and the worst offender's SHAPE. Alert routing
/// (thresholds/paging in Cloudflare/Grafana/email) is the owner's console and is
/// out of scope: this only EMITS the numbers, on both a log line and
/// `GET /v1/retention/metrics`.
fn emit_retention_metrics(visited: usize, m: &RetentionSnapshot) {
    tracing::info!(
        visited,
        total_envelopes = m.total_envelopes,
        conversations_with_envelopes = m.conversations_with_envelopes,
        oldest_sent_at = m.oldest_sent_at.as_deref().unwrap_or("-"),
        largest_conversation_envelopes = m.largest_conversation_envelopes,
        largest_conversation_oldest_sent_at =
            m.largest_conversation_oldest_sent_at.as_deref().unwrap_or("-"),
        "envelope-GC sweep complete"
    );
}

/// Resolve when the process is asked to terminate — SIGTERM (orchestrator) or
/// SIGINT (local `ctrl_c`).
///
/// Cloudflare Containers do drain-then-replace: on a deploy they SIGTERM the
/// running instance, wait up to ~15 min for a graceful exit, then SIGKILL, then
/// start the new one. This binary is PID 1 in the container, so with no SIGTERM
/// handler the kernel does NOT apply the default terminate action — SIGTERM is
/// IGNORED and the OLD instance keeps serving for the full grace window,
/// stalling the swap (and holding the sole-writer role) for up to 15 minutes.
/// Awaiting SIGTERM here collapses that to seconds.
///
/// Once a signal arrives we also arm a detached hard-exit backstop: axum's
/// graceful drain waits for in-flight requests, and a single slow/hung request
/// must not keep the OLD writer alive — that is exactly the stale-instance
/// failure. Cutting an in-flight request is safe: commit writes are atomic
/// IMMEDIATE transactions against remote Turso, so an uncommitted tx simply
/// never commits and the client retries — no corruption (the commit-log CAS
/// rejects any stale/out-of-order write regardless).
#[cfg(unix)]
async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = sigterm.recv() => {}
    }
    tracing::info!("shutdown signal received; draining (hard-exit in 5s)");
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        tracing::warn!("graceful drain deadline hit; forcing exit");
        std::process::exit(0);
    });
}

#[cfg(not(unix))]
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutdown signal received");
}
