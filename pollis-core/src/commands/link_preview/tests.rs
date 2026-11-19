//! Preview building against a local server (#1987). The server is plain http
//! on 127.0.0.1, reachable only through `Reach::LOOPBACK_TEST`, which exists
//! only in this crate's unit tests.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::fetch::Reach;
use super::*;

struct Route {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn page(html: &str) -> Route {
    Route {
        status: 200,
        headers: vec![("Content-Type".into(), "text/html; charset=utf-8".into())],
        body: html.as_bytes().to_vec(),
    }
}

fn redirect(to: &str) -> Route {
    Route {
        status: 302,
        headers: vec![("Location".into(), to.into())],
        body: Vec::new(),
    }
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

fn image_route(bytes: Vec<u8>) -> Route {
    Route {
        status: 200,
        headers: vec![("Content-Type".into(), "image/png".into())],
        body: bytes,
    }
}

/// Serve `routes` on 127.0.0.1; returns the base URL.
async fn serve(routes: HashMap<&'static str, Route>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let routes = Arc::new(routes);
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let routes = routes.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                let (status, headers, body) = match routes.get(path.as_str()) {
                    Some(r) => (r.status, r.headers.clone(), r.body.clone()),
                    None => (404, Vec::new(), Vec::new()),
                };
                let mut head = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
                for (k, v) in headers {
                    head.push_str(&format!("{k}: {v}\r\n"));
                }
                head.push_str("\r\n");
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(&body).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    base
}

#[tokio::test]
async fn builds_a_preview_with_a_bounded_thumbnail() {
    let routes = HashMap::from([
        (
            "/a",
            page(r#"<html><head><meta property="og:title" content="Hello &amp; welcome"><meta property="og:description" content="A page"><meta property="og:image" content="/img.png"></head></html>"#),
        ),
        ("/img.png", image_route(png(1600, 900))),
    ]);
    let base = serve(routes).await;
    let url = format!("{base}/a");
    let built = build(&url, Reach::LOOPBACK_TEST).await.expect("a preview");
    assert_eq!(built.preview.url, url);
    assert_eq!(built.preview.title, "Hello & welcome");
    assert_eq!(built.preview.description.as_deref(), Some("A page"));
    let thumb = built.thumbnail.expect("a thumbnail");
    assert_eq!((thumb.width, thumb.height), (600, 338));
    assert!(thumb.jpeg.len() <= MAX_THUMBNAIL_BYTES);
    assert_eq!(image::guess_format(&thumb.jpeg).unwrap(), image::ImageFormat::Jpeg);
    assert!(thumb.blurhash.is_some());
}

#[tokio::test]
async fn follows_redirects_but_never_into_a_private_address() {
    let routes = HashMap::from([
        ("/hop", redirect("/a")),
        ("/a", page("<title>Landed</title>")),
        ("/private", redirect("http://10.0.0.1/")),
        ("/metadata", redirect("http://169.254.169.254/latest/meta-data")),
        ("/loop", redirect("/loop")),
    ]);
    let base = serve(routes).await;
    let landed = build(&format!("{base}/hop"), Reach::LOOPBACK_TEST).await.unwrap();
    assert_eq!(landed.preview.title, "Landed");
    for path in ["/private", "/metadata", "/loop"] {
        assert!(build(&format!("{base}{path}"), Reach::LOOPBACK_TEST).await.is_none(), "{path}");
    }
}

#[tokio::test]
async fn an_image_on_a_private_address_costs_only_the_thumbnail() {
    let routes = HashMap::from([(
        "/a",
        page(r#"<meta property="og:title" content="T"><meta property="og:image" content="http://192.168.1.1/x.png">"#),
    )]);
    let base = serve(routes).await;
    let built = build(&format!("{base}/a"), Reach::LOOPBACK_TEST).await.unwrap();
    assert_eq!(built.preview.title, "T");
    assert!(built.thumbnail.is_none());
}

#[tokio::test]
async fn no_title_or_not_html_is_no_preview() {
    let routes = HashMap::from([
        ("/untitled", page("<html><head><meta property=\"og:description\" content=\"d\"></head></html>")),
        ("/png", image_route(png(10, 10))),
        (
            "/json",
            Route {
                status: 200,
                headers: vec![("Content-Type".into(), "application/json".into())],
                body: br#"{"title":"x"}"#.to_vec(),
            },
        ),
        ("/gone", Route { status: 410, headers: Vec::new(), body: Vec::new() }),
    ]);
    let base = serve(routes).await;
    for path in ["/untitled", "/png", "/json", "/gone", "/missing"] {
        assert!(build(&format!("{base}{path}"), Reach::LOOPBACK_TEST).await.is_none(), "{path}");
    }
}

#[tokio::test]
async fn an_oversized_image_is_not_read() {
    let routes = HashMap::from([
        ("/a", page(r#"<meta property="og:title" content="T"><meta property="og:image" content="/big">"#)),
        ("/big", image_route(vec![0u8; fetch::MAX_IMAGE_BYTES + 1])),
    ]);
    let base = serve(routes).await;
    let built = build(&format!("{base}/a"), Reach::LOOPBACK_TEST).await.unwrap();
    assert!(built.thumbnail.is_none());
}

/// A long page still yields its `<head>`: the fetcher keeps the first bytes.
#[tokio::test]
async fn a_long_page_is_read_up_to_the_cap() {
    let mut html = String::from("<head><title>Big</title></head><body>");
    html.push_str(&"x".repeat(fetch::MAX_PAGE_BYTES * 2));
    let routes = HashMap::from([("/a", page(&html))]);
    let base = serve(routes).await;
    assert_eq!(build(&format!("{base}/a"), Reach::LOOPBACK_TEST).await.unwrap().preview.title, "Big");
}

/// Production reach never accepts the loopback server.
#[tokio::test]
async fn production_reach_refuses_the_test_server() {
    let routes = HashMap::from([("/a", page("<title>T</title>"))]);
    let base = serve(routes).await;
    assert!(build(&format!("{base}/a"), Reach::PUBLIC).await.is_none());
}

#[test]
fn thumbnail_rejects_non_images_and_keeps_small_ones_small() {
    assert!(thumbnail(b"not an image").is_none());
    let small = thumbnail(&png(40, 20)).unwrap();
    assert_eq!((small.width, small.height), (40, 20), "never upscaled");
}

/// The renderer's draft is input: held to the received-preview rules, and its
/// staged thumbnail re-encoded, not trusted.
#[test]
fn outgoing_holds_a_draft_to_the_wire_rules() {
    let staged = crate::commands::staging::stage_attachment(png(900, 900)).unwrap();
    let draft = LinkPreviewDraft {
        url: "https://example.com/x".into(),
        title: "\u{202E}Title".into(),
        description: None,
        image: Some(DraftImage {
            staged_id: staged.id.clone(),
            width: 1,
            height: 1,
            data_url: String::new(),
        }),
    };
    assert!(outgoing(&draft, "nothing linked").is_none(), "the link must be in the body");
    let out = outgoing(&draft, "see https://example.com/x").unwrap();
    assert_eq!(out.preview.title, "Title");
    let thumb = out.thumbnail.unwrap();
    assert_eq!((thumb.width, thumb.height), (600, 600), "re-encoded at our bound, not the claimed size");
    release_draft(&draft);
    assert!(crate::commands::staging::peek_staged(&staged.id).is_none());

    let staged_junk = crate::commands::staging::stage_attachment(b"junk".to_vec()).unwrap();
    let junk = LinkPreviewDraft {
        image: Some(DraftImage { staged_id: staged_junk.id, width: 1, height: 1, data_url: String::new() }),
        ..draft
    };
    let out = outgoing(&junk, "https://example.com/x").unwrap();
    assert!(out.thumbnail.is_none(), "junk bytes cost the thumbnail, not the card");
}
