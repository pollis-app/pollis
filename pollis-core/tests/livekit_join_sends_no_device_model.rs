//! A LiveKit join must not tell the SFU what hardware the user is on (#1320).
//!
//! livekit 0.9's signal client reports a device model in every join — on desktop
//! the SMBIOS product name, e.g. the motherboard model "MS-7E59" — and has no
//! option to stop it. The SFU operator is untrusted for metadata, so the
//! workspace patches the crate it reads that from (`patches/device-info`) to
//! report nothing.
//!
//! ## The seam
//!
//! The SDK builds the join URL in private functions (`livekit-signaling`'s
//! `get_livekit_url`), so this test goes through the real public entry point
//! instead: `Room::connect`, with the same `RoomOptions::default()` the realtime
//! path uses, against a fake SFU on loopback. The fake answers every request
//! with 404, which walks the SDK through every request it makes on a failed
//! join: the `/rtc/v1` WebSocket (join request as a protobuf in `join_request`),
//! the `/rtc` v0 fallback (the model as a `device_model` query parameter), and
//! the `/validate` probes. Every captured request is decoded and checked, and
//! the test insists it saw both join forms, so it cannot pass by capturing
//! nothing.
//!
//! Without the patch this fails on every desktop OS: the real crate never
//! returns an empty model (it falls back to the string "Unknown").

#![cfg(feature = "media")]

use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use flate2::read::GzDecoder;
use livekit::{Room, RoomOptions};
use livekit_protocol as proto;
use prost::Message;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Accepts connections forever, records each request target, answers 404.
async fn fake_sfu(listener: TcpListener, seen: Arc<Mutex<Vec<String>>>) {
    loop {
        let Ok((mut sock, _)) = listener.accept().await else {
            return;
        };
        let seen = seen.clone();
        tokio::spawn(async move {
            let mut head = Vec::new();
            let mut buf = [0u8; 4096];
            while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                match sock.read(&mut buf).await {
                    Ok(0) | Err(_) => {
                        return;
                    }
                    Ok(n) => head.extend_from_slice(&buf[..n]),
                }
            }
            let head = String::from_utf8_lossy(&head);
            // "GET /rtc/v1?join_request=... HTTP/1.1"
            if let Some(target) = head.lines().next().and_then(|l| l.split(' ').nth(1)) {
                seen.lock().unwrap().push(target.to_string());
            }
            let _ = sock
                .write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                .await;
        });
    }
}

/// Decodes the `join_request` parameter exactly as the SFU does.
fn decode_join_request(param: &str) -> proto::JoinRequest {
    let wrapped_bytes = base64::engine::general_purpose::URL_SAFE
        .decode(param)
        .expect("join_request is base64url");
    let wrapped =
        proto::WrappedJoinRequest::decode(wrapped_bytes.as_slice()).expect("WrappedJoinRequest");
    let bytes = if wrapped.compression == proto::wrapped_join_request::Compression::Gzip as i32 {
        let mut out = Vec::new();
        GzDecoder::new(wrapped.join_request.as_slice())
            .read_to_end(&mut out)
            .expect("gzip join request");
        out
    } else {
        wrapped.join_request
    };
    proto::JoinRequest::decode(bytes.as_slice()).expect("JoinRequest")
}

#[tokio::test(flavor = "multi_thread")]
async fn livekit_join_sends_no_device_model() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let server = tokio::spawn(fake_sfu(listener, seen.clone()));

    let url = format!("ws://127.0.0.1:{port}");
    let connect = Room::connect(&url, "not-a-real-token", RoomOptions::default());
    let result = tokio::time::timeout(Duration::from_secs(60), connect)
        .await
        .expect("Room::connect against a 404 SFU should fail fast, not hang");
    assert!(result.is_err(), "a 404 SFU cannot be joined");
    server.abort();

    let seen = seen.lock().unwrap().clone();
    let mut v1_joins = 0;
    let mut v0_joins = 0;
    for target in &seen {
        let url = url::Url::parse(&format!("http://sfu{target}")).unwrap();
        for (key, value) in url.query_pairs() {
            if key == "join_request" {
                let info = decode_join_request(&value)
                    .client_info
                    .expect("join request carries client info");
                // Positive control: the decode really reached the SDK's ClientInfo.
                assert_eq!(info.sdk, proto::client_info::Sdk::Rust as i32, "{target}");
                assert_eq!(info.device_model, "", "device model sent in {target}");
                v1_joins += 1;
            }
            if key == "device_model" {
                assert_eq!(value, "", "device model sent in {target}");
                v0_joins += 1;
            }
        }
    }
    assert!(v1_joins > 0, "no /rtc/v1 join request captured: {seen:?}");
    assert!(v0_joins > 0, "no /rtc v0 join request captured: {seen:?}");
}
