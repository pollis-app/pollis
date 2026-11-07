//! A hand-rolled HTTP/1.1 LiveKit for the #1751 connection tests: axum cannot
//! leave one connection open and silent while serving the rest.

#![allow(dead_code)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// Read one HTTP/1.1 request (head + `content-length` body); `None` on EOF.
async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_ascii_lowercase();
    let len: usize = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    while buf.len() < head_end + len {
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Some(buf[head_end..head_end + len].to_vec())
}

async fn answer(sock: &mut tokio::net::TcpStream) {
    let _ = sock
        .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: keep-alive\r\n\r\n{}")
        .await;
}

/// A LiveKit whose FIRST connection answers once and then goes silent while
/// staying open — a connection the network dropped without telling either
/// end. Every later connection is healthy. Returns the URL, the number of
/// connections accepted, and every request body answered.
pub async fn livekit_with_a_dead_pooled_connection() -> (String, Arc<AtomicUsize>, Arc<Mutex<Vec<serde_json::Value>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted = Arc::new(AtomicUsize::new(0));
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let (acc, seen) = (Arc::clone(&accepted), Arc::clone(&bodies));
    tokio::spawn(async move {
        let mut parked = Vec::new();
        while let Ok((mut sock, _)) = listener.accept().await {
            let n = acc.fetch_add(1, Ordering::SeqCst);
            let seen = Arc::clone(&seen);
            if n == 0 {
                let body = read_request(&mut sock).await.unwrap_or_default();
                seen.lock().unwrap().push(serde_json::from_slice(&body).unwrap_or_default());
                answer(&mut sock).await;
                // Swallow the next request and never answer it.
                let _ = read_request(&mut sock).await;
                parked.push(sock);
                continue;
            }
            tokio::spawn(async move {
                while let Some(body) = read_request(&mut sock).await {
                    seen.lock().unwrap().push(serde_json::from_slice(&body).unwrap_or_default());
                    answer(&mut sock).await;
                }
            });
        }
    });
    (format!("ws://{addr}"), accepted, bodies)
}

