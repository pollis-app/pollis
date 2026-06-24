//! Pollis MLS Delivery Service entrypoint.
//!
//! Env:
//!   TURSO_URL    libsql://… (required)
//!   TURSO_TOKEN  scoped write token for the MLS tables (required)
//!   PORT         listen port (default 8788)
//!
//! Terminate TLS at a reverse proxy in front (this serves plain HTTP), and run
//! it beside the LiveKit container behind `api.pollis.com`.

use std::sync::Arc;

use anyhow::{Context, Result};
use pollis_delivery::{build_router, db::Db};

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
    let app = build_router(db);

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

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}
