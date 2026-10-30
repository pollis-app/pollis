//! Pollis MLS Delivery Service entrypoint.
//!
//! Env:
//!   TURSO_URL           libsql://… (required)
//!   TURSO_TOKEN         scoped write token for the MLS tables (required)
//!   LOG_DB_URL          libsql://… for the separate commit-log DB (optional)
//!   LOG_DB_ADMIN_TOKEN  read-write token for the commit-log DB (optional)
//!   PORT                listen port (default 8788)
//!   RESEND_API_KEY      Resend key for OTP emails. Unset → OTP email is not sent.
//!   DEV_OTP             dev/harness override: skip the email, force this OTP code (optional).
//!   OTP_TTL_SECS        OTP lifetime in seconds (optional, default 600).
//!   APP_REVIEW_LOGIN    `<email>:<6 digits>` — the store-review account signs in
//!                       with this fixed code and no email is sent (optional).
//!   EXPO_TOKEN          Expo token for push fan-out (#707); required once Enhanced Security is on.
//!
//! With both `LOG_DB_*` set, the MLS control-plane tables live in that DB; else in `TURSO_*`.
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
    use tracing_subscriber::prelude::*;
    tracing_subscriber::registry()
        .with(
            // No `tower_http` target: the DS has no request-tracing layer, and one
            // must not be added — it would log request headers, client IP
            // included (tests/no_client_ip_exposure.rs).
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "pollis_delivery=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        // Copies a request's warn!/error! lines into its ds_error_log row (#1305).
        .with(pollis_delivery::error_log::CaptureLayer)
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

    // Optional separate commit-log DB; otherwise fall back to the single `db` above.
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

    // Built first so the sweep and the router share one `retention_metrics` cell (#720).
    let state = build_app_state(Arc::clone(&db), log_db);
    // Load the client policy now rather than on the first request (#1611).
    {
        let mirror = state.client_policy.clone();
        tokio::spawn(async move { mirror.refresh().await });
    }

    // Envelope-GC sweep (#689); started before the router so it also runs on a scale-to-zero wake.
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
/// Decouples GC's trigger from member activity; the deletion predicate is untouched.
///
/// A timer: collectible state can settle with no request to hook. `POLLIS_DS_GC_SWEEP_SECS`.
///
/// Also carries the #720 liveness window and gathers the growth snapshot.
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
            match db.conn().await {
                Ok(conn) => match pollis_delivery::messages::sweep_envelope_gc(&conn, &stale).await {
                    Ok(report) => {
                        emit_retention_metrics(report.visited, &report.metrics);
                        *metrics.lock().unwrap() = Some(report.metrics);
                        // #762: aged-out audit rows and dead push tokens; logged so the GC result isn't lost.
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
                        // #1122: expired push handles; the sweep is the mitigation, not housekeeping.
                        match pollis_delivery::push::sweep_push_handles(&conn).await {
                            Ok(n) if n > 0 => {
                                tracing::info!("retention sweep: aged out {n} push_handle row(s)")
                            }
                            Ok(_) => {}
                            Err(e) => tracing::warn!("push-handle sweep failed: {e}"),
                        }
                        // #1423: fetch Expo receipts, prune dead tokens, drop answered tickets.
                        match pollis_delivery::push::check_push_receipts(&conn).await {
                            Ok(r) if r.checked > 0 || r.expired > 0 => tracing::info!(
                                "push receipts: {} checked ({} error), {} expired unanswered",
                                r.checked,
                                r.errors,
                                r.expired
                            ),
                            Ok(_) => {}
                            Err(e) => tracing::warn!("push receipt check failed: {e}"),
                        }
                    }
                    Err(e) => tracing::warn!("envelope-GC sweep failed: {e}"),
                },
                Err(e) => tracing::warn!("envelope-GC sweep: DB connection failed: {e}"),
            }
        }
    });
}

/// Log the identity-free growth snapshot (#720): counts only, no conversation or user id.
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
/// PID 1 in the container: without a handler SIGTERM is IGNORED for the ~15 min grace.
///
/// Then a hard-exit backstop, so a hung request can't keep the old writer alive.
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
