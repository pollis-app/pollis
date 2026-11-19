//! `pollis-bot post --installation <id> --channel <c> <text>`: a client of the
//! running runtime's own webhook endpoint, so the CLI and an alert sender take
//! exactly the same path (auth, rate limit, job loop, claim, grants).

use std::net::SocketAddr;

use anyhow::{bail, Context, Result};
use serde_json::json;

use crate::webhook::{sign, SIGNATURE_HEADER};

/// The answer the runtime gave, already judged.
pub struct PostAnswer {
    pub status: u16,
    pub body: serde_json::Value,
}

impl PostAnswer {
    pub fn posted(&self) -> bool {
        matches!(self.body.get("status").and_then(|s| s.as_str()), Some("posted" | "claimed_elsewhere" | "accepted"))
    }
}

/// Sign and send one call to `http://<bind>/hooks/<installation_id>`.
pub async fn post(
    bind: SocketAddr,
    installation_id: &str,
    secret: &[u8],
    channel: &str,
    text: &str,
) -> Result<PostAnswer> {
    if text.trim().is_empty() {
        bail!("nothing to post: the text is empty");
    }
    // A wildcard bind is reached on loopback.
    let host = if bind.ip().is_unspecified() {
        match bind {
            SocketAddr::V4(_) => SocketAddr::from(([127, 0, 0, 1], bind.port())),
            SocketAddr::V6(_) => SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, bind.port())),
        }
    } else {
        bind
    };
    let body = serde_json::to_vec(&json!({"channel": channel, "text": text}))?;
    let url = format!("http://{host}/hooks/{installation_id}");
    let response = reqwest::Client::new()
        .post(&url)
        .header("content-type", "application/json")
        .header(SIGNATURE_HEADER, sign(secret, &body))
        .body(body)
        .send()
        .await
        .with_context(|| {
            format!("could not reach the runtime at {url}. Fix: start it with `pollis-bot run` and enable the webhook (config.json `webhook`, or `run --webhook`)")
        })?;
    let status = response.status().as_u16();
    let body = response.json::<serde_json::Value>().await.unwrap_or(serde_json::Value::Null);
    Ok(PostAnswer { status, body })
}
