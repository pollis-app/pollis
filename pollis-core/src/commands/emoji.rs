//! Custom per-group emoji (#848) — re-encoding, upload, listing, and the
//! "which emoji may this user send" rule.
//!
//! Unencrypted on purpose: members of B must render A's emoji (#848).
//! [`encode_emoji`] is the decoder attack surface; output is always <= [`EMOJI_MAX_BYTES`].

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::commands::r2;
use crate::error::{Error, Result};
use crate::state::AppState;

// Bounds: must agree with the DS (authoritative).

/// Hard ceiling on one stored emoji. Must equal `pollis_delivery::emoji::EMOJI_MAX_BYTES`.
pub const EMOJI_MAX_BYTES: usize = 48 * 1024;

/// Largest source file [`encode_emoji`] will parse.
pub const EMOJI_MAX_INPUT_BYTES: usize = 8 * 1024 * 1024;

/// Largest source dimension, checked from the header before a full decode.
pub const EMOJI_MAX_SOURCE_DIM: u32 = 4096;

/// Cap on decoded animation frames (bounds a GIF bomb's decode time).
pub const EMOJI_MAX_SOURCE_FRAMES: usize = 300;

/// Decode-side allocation cap for `image::Limits`; catches a lying header.
const EMOJI_MAX_DECODE_ALLOC: u64 = 256 * 1024 * 1024;

/// Largest square the re-encoder targets (survives a 2× display).
const EMOJI_TARGET_DIM: u32 = 64;

/// Degrade ladder [`encode_emoji`] walks until the output fits.
const SIZE_LADDER: [u32; 4] = [EMOJI_TARGET_DIM, 56, 48, 32];

/// Frame budgets paired with the size ladder for animations.
const FRAME_LADDER: [usize; 4] = [60, 40, 24, 12];

// Wire types (keep in sync with `frontend/src/types`).

/// One emoji as the UI sees it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CustomEmoji {
    pub group_id: String,
    /// The group's display name, denormalised for the picker's section headers.
    pub group_name: String,
    pub shortcode: String,
    pub content_hash: String,
    pub content_type: String,
    pub animated: bool,
    pub size_bytes: u64,
    pub created_by: String,
}

/// The output of [`encode_emoji`]. `bytes` is guaranteed `<= EMOJI_MAX_BYTES`.
#[derive(Debug, Clone)]
pub struct EncodedEmoji {
    pub bytes: Vec<u8>,
    /// `image/webp` or `image/gif`, the two the DS allowlists.
    pub content_type: &'static str,
    pub animated: bool,
    pub width: u32,
    pub height: u32,
    /// 1 for a static image.
    pub frames: usize,
}

// Validation (mirrors the DS, which is authoritative).

/// `[a-z0-9_]{2,32}`. Mirrors `pollis_delivery::emoji::shortcode_is_valid`; the DS re-validates.
pub fn shortcode_is_valid(shortcode: &str) -> bool {
    let len = shortcode.len();
    if !(2..=32).contains(&len) {
        return false;
    }
    shortcode
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// 64 lowercase hex chars. Case-sensitive so one image cannot occupy two rows.
pub fn content_hash_is_valid(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The R2 key an emoji object lives at; mirrors `pollis_delivery::emoji::emoji_r2_key`.
fn emoji_r2_key(content_hash: &str, content_type: &str) -> Result<String> {
    let ext = match content_type {
        "image/webp" => "webp",
        "image/gif" => "gif",
        other => {
            return Err(Error::Other(anyhow::anyhow!(
                "not an emoji content type: {other}"
            )))
        }
    };
    Ok(format!("emoji/{content_hash}.{ext}"))
}

/// One custom-emoji reference found in message text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmojiToken {
    /// Byte range of the whole `<:name:hash>` run in the source text.
    pub start: usize,
    pub end: usize,
    pub shortcode: String,
    pub content_hash: String,
}

/// Scan `text` for `<:shortcode:content_hash>` references.
///
/// Malformed runs stay literal text. Neither field's alphabet has `:` or `>`, so no nesting.
pub fn parse_emoji_tokens(text: &str) -> Vec<EmojiToken> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 2 < bytes.len() {
        if bytes[i] != b'<' || bytes[i + 1] != b':' {
            i += 1;
            continue;
        }
        let name_start = i + 2;
        let Some(rel_colon) = bytes[name_start..].iter().position(|&b| b == b':') else {
            break;
        };
        let name_end = name_start + rel_colon;
        let hash_start = name_end + 1;
        let Some(rel_gt) = bytes[hash_start..].iter().position(|&b| b == b'>') else {
            break;
        };
        let hash_end = hash_start + rel_gt;

        let (Ok(shortcode), Ok(hash)) = (
            std::str::from_utf8(&bytes[name_start..name_end]),
            std::str::from_utf8(&bytes[hash_start..hash_end]),
        ) else {
            i += 1;
            continue;
        };
        if shortcode_is_valid(shortcode) && content_hash_is_valid(hash) {
            out.push(EmojiToken {
                start: i,
                end: hash_end + 1,
                shortcode: shortcode.to_string(),
                content_hash: hash.to_string(),
            });
            i = hash_end + 1;
        } else {
            i += 1;
        }
    }
    out
}

/// Render `token` back to its wire form.
pub fn emoji_token_text(shortcode: &str, content_hash: &str) -> String {
    format!("<:{shortcode}:{content_hash}>")
}

/// Re-encode arbitrary, untrusted image bytes into a small emoji object.
///
/// Output is always `<= EMOJI_MAX_BYTES` or an error. Animations that cannot fit fall back
/// to their first frame as a static image.
pub fn encode_emoji(input: &[u8]) -> Result<EncodedEmoji> {
    use image::ImageFormat;

    if input.is_empty() {
        return Err(Error::Other(anyhow::anyhow!("emoji file is empty")));
    }
    if input.len() > EMOJI_MAX_INPUT_BYTES {
        return Err(Error::Other(anyhow::anyhow!(
            "emoji source is {} bytes; the limit is {EMOJI_MAX_INPUT_BYTES}",
            input.len()
        )));
    }

    // Sniff the format from magic bytes; no MIME type is ever consulted.
    let format = image::guess_format(input)
        .map_err(|_| Error::Other(anyhow::anyhow!("unrecognised image format")))?;
    if !matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::WebP
    ) {
        return Err(Error::Other(anyhow::anyhow!(
            "emoji must be a PNG, JPEG, GIF or WebP"
        )));
    }

    let (src_w, src_h) = probe_dimensions(input, format)?;
    if src_w == 0 || src_h == 0 {
        return Err(Error::Other(anyhow::anyhow!("image has a zero dimension")));
    }
    if src_w > EMOJI_MAX_SOURCE_DIM || src_h > EMOJI_MAX_SOURCE_DIM {
        return Err(Error::Other(anyhow::anyhow!(
            "emoji source is {src_w}x{src_h}; the limit is \
             {EMOJI_MAX_SOURCE_DIM}x{EMOJI_MAX_SOURCE_DIM}"
        )));
    }

    let frames = decode_frames(input, format)?;
    if frames.is_empty() {
        return Err(Error::Other(anyhow::anyhow!("image contains no frames")));
    }

    if frames.len() > 1 {
        if let Some(encoded) = encode_animated(&frames)? {
            return Ok(encoded);
        }
        // Fell off both ladders: keep the first frame.
    }
    encode_static(&frames[0].image)
}

/// One decoded frame: RGBA pixels plus how long it is shown.
struct RawFrame {
    image: image::RgbaImage,
    delay_ms: u32,
}

/// Read `(width, height)` from the header without decoding pixels.
fn probe_dimensions(input: &[u8], format: image::ImageFormat) -> Result<(u32, u32)> {
    use image::ImageDecoder as _;
    use std::io::Cursor;

    let cursor = Cursor::new(input);
    let dims = match format {
        image::ImageFormat::Png => image::codecs::png::PngDecoder::new(cursor)
            .map_err(decode_err)?
            .dimensions(),
        image::ImageFormat::Jpeg => image::codecs::jpeg::JpegDecoder::new(cursor)
            .map_err(decode_err)?
            .dimensions(),
        image::ImageFormat::Gif => image::codecs::gif::GifDecoder::new(cursor)
            .map_err(decode_err)?
            .dimensions(),
        image::ImageFormat::WebP => image::codecs::webp::WebPDecoder::new(cursor)
            .map_err(decode_err)?
            .dimensions(),
        _ => return Err(Error::Other(anyhow::anyhow!("unsupported emoji format"))),
    };
    Ok(dims)
}

fn decode_err(e: image::ImageError) -> Error {
    // The decoder's message can quote attacker bytes, so it is not surfaced.
    let _ = e;
    Error::Other(anyhow::anyhow!("could not decode that image"))
}

fn decode_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(EMOJI_MAX_SOURCE_DIM);
    limits.max_image_height = Some(EMOJI_MAX_SOURCE_DIM);
    limits.max_alloc = Some(EMOJI_MAX_DECODE_ALLOC);
    limits
}

/// Decode every frame (capped at [`EMOJI_MAX_SOURCE_FRAMES`]) as RGBA.
fn decode_frames(input: &[u8], format: image::ImageFormat) -> Result<Vec<RawFrame>> {
    use image::AnimationDecoder as _;
    use std::io::Cursor;

    let animated_frames = match format {
        image::ImageFormat::Gif => {
            let decoder = image::codecs::gif::GifDecoder::new(Cursor::new(input))
                .map_err(decode_err)?;
            Some(decoder.into_frames())
        }
        image::ImageFormat::WebP => {
            let decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(input))
                .map_err(decode_err)?;
            if decoder.has_animation() {
                Some(decoder.into_frames())
            } else {
                None
            }
        }
        _ => None,
    };

    if let Some(frames) = animated_frames {
        let mut out = Vec::new();
        for frame in frames.take(EMOJI_MAX_SOURCE_FRAMES) {
            let frame = frame.map_err(decode_err)?;
            let (numer, denom) = frame.delay().numer_denom_ms();
            // GIFs use 0 for "as fast as possible"; normalise like browsers do.
            let delay_ms = numer
                .checked_div(denom)
                .map_or(100, |ms| ms.clamp(20, 10_000));
            out.push(RawFrame {
                image: frame.into_buffer(),
                delay_ms,
            });
        }
        if !out.is_empty() {
            return Ok(out);
        }
    }

    // Limited reader so a lying header cannot force a large allocation.
    let mut reader = image::ImageReader::new(Cursor::new(input));
    reader.set_format(format);
    reader.limits(decode_limits());
    let image = reader.decode().map_err(decode_err)?;
    Ok(vec![RawFrame {
        image: image.to_rgba8(),
        delay_ms: 100,
    }])
}

/// Scale to fit inside `target`×`target`, preserving aspect ratio; never scales up.
fn fit(image: &image::RgbaImage, target: u32) -> image::RgbaImage {
    let (w, h) = (image.width(), image.height());
    if w <= target && h <= target {
        return image.clone();
    }
    let scale = f64::from(target) / f64::from(w.max(h));
    let nw = ((f64::from(w) * scale).round() as u32).max(1);
    let nh = ((f64::from(h) * scale).round() as u32).max(1);
    image::imageops::resize(image, nw, nh, image::imageops::FilterType::Lanczos3)
}

/// Encode one frame as lossless WebP, walking the size ladder until it fits.
///
/// `image`'s WebP encoder is lossless-only.
fn encode_static(source: &image::RgbaImage) -> Result<EncodedEmoji> {
    for target in SIZE_LADDER {
        let scaled = fit(source, target);
        let bytes = webp_lossless(&scaled)?;
        if bytes.len() <= EMOJI_MAX_BYTES {
            return Ok(EncodedEmoji {
                bytes,
                content_type: "image/webp",
                animated: false,
                width: scaled.width(),
                height: scaled.height(),
                frames: 1,
            });
        }
    }
    Err(Error::Other(anyhow::anyhow!(
        "could not compress that image below {EMOJI_MAX_BYTES} bytes"
    )))
}

fn webp_lossless(image: &image::RgbaImage) -> Result<Vec<u8>> {
    use image::ImageEncoder as _;
    let mut out = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut out)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| Error::Other(anyhow::anyhow!("webp encode: {e}")))?;
    Ok(out)
}

/// Encode an animation as GIF, walking the (size, frame-budget) ladder.
///
/// `Ok(None)`: every rung was too big.
fn encode_animated(frames: &[RawFrame]) -> Result<Option<EncodedEmoji>> {
    for (target, budget) in SIZE_LADDER.into_iter().zip(FRAME_LADDER) {
        let kept = subsample(frames, budget);
        let scaled: Vec<(image::RgbaImage, u32)> = kept
            .iter()
            .map(|(frame, delay)| (fit(&frame.image, target), *delay))
            .collect();
        let bytes = gif_encode(&scaled)?;
        if bytes.len() <= EMOJI_MAX_BYTES {
            let (w, h) = scaled
                .first()
                .map(|(f, _)| (f.width(), f.height()))
                .unwrap_or((target, target));
            return Ok(Some(EncodedEmoji {
                bytes,
                content_type: "image/gif",
                animated: true,
                width: w,
                height: h,
                frames: scaled.len(),
            }));
        }
    }
    Ok(None)
}

/// Reduce `frames` to at most `budget` by taking an evenly-spaced subset, and
/// fold the dropped frames' delays into the frame that replaces them.
fn subsample(frames: &[RawFrame], budget: usize) -> Vec<(&RawFrame, u32)> {
    if frames.len() <= budget || budget == 0 {
        return frames.iter().map(|f| (f, f.delay_ms)).collect();
    }
    let stride = frames.len().div_ceil(budget);
    let mut out: Vec<(&RawFrame, u32)> = Vec::with_capacity(budget);
    let mut carried = 0u32;
    for (i, frame) in frames.iter().enumerate() {
        carried = carried.saturating_add(frame.delay_ms);
        if i % stride == stride - 1 || i + 1 == frames.len() {
            out.push((frame, carried.clamp(20, 10_000)));
            carried = 0;
        }
    }
    out
}

fn gif_encode(frames: &[(image::RgbaImage, u32)]) -> Result<Vec<u8>> {
    use image::codecs::gif::{GifEncoder, Repeat};
    use image::{Delay, Frame};

    let mut out = Vec::new();
    {
        // Speed 30: fastest quantiser; the quality loss is invisible on emoji.
        let mut encoder = GifEncoder::new_with_speed(&mut out, 30);
        encoder
            .set_repeat(Repeat::Infinite)
            .map_err(|e| Error::Other(anyhow::anyhow!("gif repeat: {e}")))?;
        for (image, delay_ms) in frames {
            let frame = Frame::from_parts(
                image.clone(),
                0,
                0,
                Delay::from_numer_denom_ms(*delay_ms, 1),
            );
            encoder
                .encode_frame(frame)
                .map_err(|e| Error::Other(anyhow::anyhow!("gif encode: {e}")))?;
        }
    }
    Ok(out)
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(data))
}

/// Every custom emoji `user_id` may use: the picker's source and the permission rule in one.
/// No conversation argument on purpose: any registering group grants use anywhere (#848).
pub async fn list_usable_emoji(_user_id: String, state: &Arc<AppState>) -> Result<Vec<CustomEmoji>> {
    let body = pollis_api::account_reads::EmojiReadBody {
        want_usable: true,
        content_hashes: Vec::new(),
    };
    let resp = crate::commands::mls::ds_post_json(state, &body).await?;
    Ok(resp.usable.into_iter().map(emoji_from_wire).collect())
}

/// Every custom emoji registered to one group — the management page's list.
///
/// Members-only (#917); cross-group rendering (#848) goes through `get_emoji_url` instead.
pub async fn list_group_emoji(
    group_id: String,
    requester_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<CustomEmoji>> {
    // Membership and the emoji set in ONE request; the DS applies the members-only rule.
    let resp = crate::commands::ds_reads::group(state, &group_id, &requester_id, true).await?;
    if !resp.authorized {
        return Err(Error::Other(anyhow::anyhow!(
            crate::commands::groups::authz::NOT_A_MEMBER
        )));
    }
    Ok(resp.emoji.into_iter().map(emoji_from_wire).collect())
}

/// Wire → domain for one custom emoji.
fn emoji_from_wire(e: pollis_api::directory::CustomEmojiWire) -> CustomEmoji {
    CustomEmoji {
        group_id: e.group_id,
        group_name: e.group_name,
        shortcode: e.shortcode,
        content_hash: e.content_hash,
        content_type: e.content_type,
        animated: e.animated,
        size_bytes: e.size_bytes.max(0) as u64,
        created_by: e.created_by,
    }
}


/// Add an emoji to a group from a file on disk.
///
/// Re-encode first so dedup keys on the stored bytes; register last, so a failure leaves
/// an orphaned object (swept) rather than a row pointing at nothing.
pub async fn upload_group_emoji(
    group_id: String,
    shortcode: String,
    path: String,
    state: &Arc<AppState>,
) -> Result<CustomEmoji> {
    if !shortcode_is_valid(&shortcode) {
        return Err(Error::Other(anyhow::anyhow!(
            "a shortcode must be 2-32 characters of a-z, 0-9 and _"
        )));
    }

    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("read {path}: {e}")))?;
    if meta.len() > EMOJI_MAX_INPUT_BYTES as u64 {
        return Err(Error::Other(anyhow::anyhow!(
            "that image is {} bytes; the limit is {EMOJI_MAX_INPUT_BYTES}",
            meta.len()
        )));
    }
    let raw = tokio::fs::read(&path)
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("read {path}: {e}")))?;

    let encoded = encode_emoji(&raw)?;
    // Assert the postcondition rather than trust it: the bucket is public.
    if encoded.bytes.len() > EMOJI_MAX_BYTES {
        return Err(Error::Other(anyhow::anyhow!(
            "internal: re-encoded emoji is {} bytes, over the {EMOJI_MAX_BYTES} ceiling",
            encoded.bytes.len()
        )));
    }

    let content_hash = sha256_hex(&encoded.bytes);
    let r2_key = emoji_r2_key(&content_hash, encoded.content_type)?;

    let already_stored = crate::commands::ds_reads::object_exists(
        state,
        &content_hash,
        pollis_api::account_reads::ObjectKind::Emoji,
    )
    .await?;

    if !already_stored {
        let put_url = r2::presign_r2_with_length(
            state,
            "put",
            &r2_key,
            Some(encoded.bytes.len() as u64),
        )
        .await?;
        let overlay = state.overlay_handle();
        r2::r2_put_url(
            overlay.as_deref(),
            &put_url,
            encoded.bytes.clone(),
            encoded.content_type,
        )
        .await?;
    }

    // `created_by` is the DS's no-auth actor fallback; when signed it must equal the signer.
    let actor_id = crate::commands::mls::current_user_id(state).await?;
    crate::commands::mls::ds_post_ok(
        state,
        &pollis_api::emoji::CreateEmojiBody {
            group_id: group_id.clone(),
            shortcode: shortcode.clone(),
            content_hash: content_hash.clone(),
            content_type: encoded.content_type.to_string(),
            size_bytes: encoded.bytes.len() as u64,
            animated: encoded.animated,
            created_by: Some(actor_id),
        },
    )
    .await?;

    Ok(CustomEmoji {
        group_id,
        group_name: String::new(),
        shortcode,
        content_hash,
        content_type: encoded.content_type.to_string(),
        animated: encoded.animated,
        size_bytes: encoded.bytes.len() as u64,
        created_by: String::new(),
    })
}

/// Remove an emoji from a group, then sweep whatever that orphaned.
///
/// A failed blob delete is logged and ignored: the row is gone, so it strands <= 48 KiB.
pub async fn remove_group_emoji(
    group_id: String,
    shortcode: String,
    state: &Arc<AppState>,
) -> Result<()> {
    // `actor_id`: same no-auth fallback contract as `/v1/emoji/create` above.
    let actor_id = crate::commands::mls::current_user_id(state).await?;
    crate::commands::mls::ds_post_ok(
        state,
        &pollis_api::emoji::RemoveEmojiBody {
            group_id,
            shortcode,
            actor_id: Some(actor_id),
        },
    )
    .await?;
    collect_orphaned_emoji(state).await
}

/// How many collected emoji blobs to delete at once (#915).
///
/// Bounds load on our own DS (each deletion is a presign plus an R2 DELETE).
const EMOJI_GC_DELETE_CONCURRENCY: usize = 8;

/// Ask the DS to collect unreferenced emoji objects, then delete their R2 blobs. Idempotent.
pub async fn collect_orphaned_emoji(state: &Arc<AppState>) -> Result<()> {
    let parsed =
        crate::commands::mls::ds_post_json(state, &pollis_api::emoji::EmojiGcBody {}).await?;

    // Best-effort per item: the row is gone, so a failure strands a <=48 KiB blob.
    use futures_util::stream::StreamExt as _;
    futures_util::stream::iter(parsed.r2_keys)
        .for_each_concurrent(EMOJI_GC_DELETE_CONCURRENCY, |key| async move {
            if let Err(e) = r2::delete_r2_object(state, &key).await {
                crate::diag!("[emoji] could not delete collected object {key}: {e}");
            }
        })
        .await;
    Ok(())
}

/// Resolve an emoji to a loopback URL the webview can use as `<img src>`.
///
/// Shares `get_media_url`'s cache. The content type is looked up from the row, since the
/// token carries only the hash; the lookup is skipped on a cache hit.
pub async fn get_emoji_url(content_hash: String, state: &Arc<AppState>) -> Result<String> {
    if !content_hash_is_valid(&content_hash) {
        return Err(Error::Other(anyhow::anyhow!("malformed emoji content hash")));
    }
    let url = crate::media_server::loopback_url(state, &content_hash).await?;

    // A cache hit is served by its file extension; no content type needed.
    if r2::find_cached_file(&content_hash).is_some() {
        return Ok(url);
    }

    let (bytes, content_type) = download_verified_emoji(&content_hash, state).await?;
    let target = r2::cache_file_path(&content_hash, &content_type)?;

    let db_key = {
        let guard = state.unlock.lock().await;
        match guard.as_ref() {
            Some(u) => u.db_key.to_vec(),
            None => {
                return Err(Error::Other(anyhow::anyhow!(
                    "cannot cache an emoji without an active unlock"
                )))
            }
        }
    };
    let encrypted = r2::cache_encrypt(bytes, &db_key, content_hash.as_bytes())?;

    // Atomic write: a half-written cache file would be served as corrupt.
    let mut tmp = target.clone();
    let final_ext = target.extension().and_then(|s| s.to_str()).unwrap_or("enc");
    tmp.set_extension(format!("{final_ext}.tmp"));
    crate::private_fs::write_async(&tmp, encrypted)
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("write emoji cache: {e}")))?;
    if let Err(e) = tokio::fs::rename(&tmp, &target).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(Error::Other(anyhow::anyhow!("rename emoji cache: {e}")));
    }
    Ok(url)
}

/// Fetch an emoji from R2 and verify it against its content hash. Objects are stored in the
/// clear, so this check is the only integrity proof; every consumer must go through it.
pub async fn download_verified_emoji(
    content_hash: &str,
    state: &Arc<AppState>,
) -> Result<(Vec<u8>, String)> {
    if !content_hash_is_valid(content_hash) {
        return Err(Error::Other(anyhow::anyhow!("malformed emoji content hash")));
    }
    let (r2_key, content_type) = {
        // Deliberately NOT membership-gated (#848); `list_group_emoji` is.
        let body = pollis_api::account_reads::EmojiReadBody {
            want_usable: false,
            content_hashes: vec![content_hash.to_string()],
        };
        let resp = crate::commands::mls::ds_post_json(state, &body).await?;
        match resp.objects.into_iter().next() {
            Some(o) => (o.r2_key, o.content_type),
            None => {
                return Err(Error::Other(anyhow::anyhow!(
                    "no such emoji object: {content_hash}"
                )))
            }
        }
    };
    // Re-derive the key rather than trusting the stored string.
    let expected_key = emoji_r2_key(content_hash, &content_type)?;
    if r2_key != expected_key {
        return Err(Error::Other(anyhow::anyhow!(
            "emoji {content_hash} has an unexpected object key; refusing to fetch it"
        )));
    }

    let get_url = r2::presign_r2_with_length(state, "get", &expected_key, None).await?;
    let overlay = state.overlay_handle();
    // Ceiling passed IN: reading an unbounded body before measuring it is a memory bomb.
    let Some(bytes) =
        r2::r2_get_url(overlay.as_deref(), &get_url, EMOJI_MAX_BYTES as u64).await?
    else {
        return Err(Error::Other(anyhow::anyhow!(
            "emoji object {content_hash} is over the {EMOJI_MAX_BYTES}-byte ceiling — refusing it"
        )));
    };
    let actual = sha256_hex(&bytes);
    if actual != content_hash {
        return Err(Error::Other(anyhow::anyhow!(
            "emoji object {content_hash} failed its content-hash check (got {actual}); \
             refusing to render substituted bytes"
        )));
    }
    // `Vec::from` moves the `Bytes` allocation when uniquely owned (#915).
    Ok((Vec::from(bytes), content_type))
}

/// Strip any custom-emoji token `user_id` is not permitted to use from outgoing
/// message text, leaving the shortcode as plain `:name:` text.
///
/// Enforced on the sender's device because the DS cannot see message content. Degrades
/// rather than refuses: the recipient sees plain `:name:`.
pub async fn prepare_emoji_text(
    user_id: String,
    text: String,
    state: &Arc<AppState>,
) -> Result<String> {
    let tokens = parse_emoji_tokens(&text);
    if tokens.is_empty() {
        return Ok(text);
    }
    let usable = list_usable_emoji(user_id, state).await?;
    let allowed: std::collections::HashSet<&str> =
        usable.iter().map(|e| e.content_hash.as_str()).collect();
    Ok(strip_disallowed_emoji(&text, &allowed))
}

/// The pure half of [`prepare_emoji_text`]: rewrite `text`, keeping every token
/// whose hash is in `allowed` and degrading the rest to plain `:shortcode:`.
pub fn strip_disallowed_emoji(
    text: &str,
    allowed: &std::collections::HashSet<&str>,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    for token in parse_emoji_tokens(text) {
        out.push_str(&text[cursor..token.start]);
        if allowed.contains(token.content_hash.as_str()) {
            out.push_str(&text[token.start..token.end]);
        } else {
            out.push(':');
            out.push_str(&token.shortcode);
            out.push(':');
        }
        cursor = token.end;
    }
    out.push_str(&text[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(seed: char) -> String {
        seed.to_string().repeat(64)
    }

    #[test]
    fn well_formed_tokens_are_found_and_nothing_else_is() {
        let hash = h('a');
        let text = format!("hi <:parrot:{hash}> there <:parrot:{hash}>!");
        let tokens = parse_emoji_tokens(&text);
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].shortcode, "parrot");
        assert_eq!(tokens[0].content_hash, hash);
        assert_eq!(&text[tokens[0].start..tokens[0].end], emoji_token_text("parrot", &hash));
    }

    /// A malformed run must stay literal text.
    #[test]
    fn malformed_runs_are_left_alone() {
        for text in [
            "<:parrot:>",
            "<:parrot:short>",
            "<::>",
            "<:UPPER:aaaa>",
            "<:parrot aaa>",
            "plain :parrot: text",
            "<:parrot:",
            "<:",
            "",
        ] {
            assert!(
                parse_emoji_tokens(text).is_empty(),
                "{text:?} must not parse as a token"
            );
        }
    }

    #[test]
    fn markup_cannot_ride_inside_a_token() {
        let hash = h('a');
        let hostile = format!("<:<img src=x onerror=alert(1)>:{hash}>");
        assert!(parse_emoji_tokens(&hostile).is_empty());

        // …and the alphabet check is what does it, not a filter on the output.
        assert!(!shortcode_is_valid("<img src=x>"));
        assert!(!shortcode_is_valid("a:b"));
        assert!(!shortcode_is_valid("../../etc/passwd"));
    }

    /// The #848 worked example: membership, never the conversation, decides.
    #[test]
    fn a_disallowed_emoji_degrades_to_its_shortcode_and_nothing_else_moves() {
        let parrot = h('a');
        let stranger = h('b');
        let allowed: std::collections::HashSet<&str> =
            [parrot.as_str()].into_iter().collect();

        // B, permitted: the token survives verbatim.
        let b_text = format!("morning all <:parrot:{parrot}> — standup in 5");
        assert_eq!(strip_disallowed_emoji(&b_text, &allowed), b_text);

        // C, not permitted: only the token is rewritten.
        let c_text = format!("morning all <:parrot:{stranger}> — standup in 5");
        assert_eq!(
            strip_disallowed_emoji(&c_text, &allowed),
            "morning all :parrot: — standup in 5"
        );

        // Mixed, adjacent, and at the very edges of the string — the cases where
        // a slicing bug eats a character.
        let mixed = format!("<:parrot:{parrot}><:parrot:{stranger}>");
        assert_eq!(
            strip_disallowed_emoji(&mixed, &allowed),
            format!("<:parrot:{parrot}>:parrot:")
        );

        // No tokens: returned byte-for-byte.
        for untouched in ["", "no emoji here", "<:oops:>", "a :parrot: b"] {
            assert_eq!(strip_disallowed_emoji(untouched, &allowed), untouched);
        }

        // Multi-byte text around a stripped token survives.
        let unicode = format!("héllo 🎉 <:parrot:{stranger}> wörld");
        assert_eq!(
            strip_disallowed_emoji(&unicode, &allowed),
            "héllo 🎉 :parrot: wörld"
        );
    }

    #[test]
    fn with_nothing_permitted_every_token_degrades() {
        let allowed: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let text = format!("<:a_b:{}> <:c_d:{}>", h('a'), h('b'));
        assert_eq!(strip_disallowed_emoji(&text, &allowed), ":a_b: :c_d:");
    }

    /// Deterministic noise, so it does not compress to nothing.
    fn noisy_png(w: u32, h: u32) -> Vec<u8> {
        use image::ImageEncoder as _;
        let mut img = image::RgbaImage::new(w, h);
        // A cheap deterministic PRNG — worst-case content for a lossless codec.
        let mut seed: u32 = 0x1234_5678;
        for pixel in img.pixels_mut() {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let b = seed.to_le_bytes();
            *pixel = image::Rgba([b[0], b[1], b[2], 255]);
        }
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgba8)
            .unwrap();
        out
    }

    /// Flat PNG: the only way to reach 4096px without exceeding the 8 MiB input ceiling.
    fn flat_png(w: u32, h: u32) -> Vec<u8> {
        use image::ImageEncoder as _;
        let mut img = image::RgbaImage::new(w, h);
        for (x, y, pixel) in img.enumerate_pixels_mut() {
            *pixel = image::Rgba([(x % 256) as u8, (y % 256) as u8, 180, 255]);
        }
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgba8)
            .unwrap();
        out
    }

    /// A deterministic animated GIF of full-size noise.
    fn noisy_gif(w: u32, h: u32, frames: usize) -> Vec<u8> {
        use image::codecs::gif::GifEncoder;
        use image::{Delay, Frame};
        let mut out = Vec::new();
        {
            let mut encoder = GifEncoder::new_with_speed(&mut out, 30);
            let mut seed: u32 = 0xdead_beef;
            for _ in 0..frames {
                let mut img = image::RgbaImage::new(w, h);
                for pixel in img.pixels_mut() {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    let b = seed.to_le_bytes();
                    *pixel = image::Rgba([b[0], b[1], b[2], 255]);
                }
                encoder
                    .encode_frame(Frame::from_parts(
                        img,
                        0,
                        0,
                        Delay::from_numer_denom_ms(40, 1),
                    ))
                    .unwrap();
            }
        }
        out
    }

    /// The size bound (#848) holds on every shape of input, including the hostile ones.
    #[test]
    fn the_size_bound_holds_for_every_input() {
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("1024x1024 noise png", noisy_png(1024, 1024)),
            ("4096x4096 flat png (at the dimension ceiling)", flat_png(4096, 4096)),
            ("512x512 noise png", noisy_png(512, 512)),
            ("16x16 noise png (smaller than the target)", noisy_png(16, 16)),
            ("128x128 x 120-frame noise gif", noisy_gif(128, 128, 120)),
            ("64x64 x 8-frame noise gif", noisy_gif(64, 64, 8)),
            ("112x112 x 300-frame noise gif (at the frame cap)", noisy_gif(112, 112, 300)),
        ];
        for (label, input) in cases {
            let encoded = encode_emoji(&input).unwrap_or_else(|e| panic!("{label}: {e}"));
            assert!(
                encoded.bytes.len() <= EMOJI_MAX_BYTES,
                "{label}: re-encoded to {} bytes, over the {EMOJI_MAX_BYTES} ceiling",
                encoded.bytes.len()
            );
            assert!(
                encoded.width <= EMOJI_TARGET_DIM && encoded.height <= EMOJI_TARGET_DIM,
                "{label}: {}x{} exceeds the render size",
                encoded.width,
                encoded.height
            );
            assert!(matches!(encoded.content_type, "image/webp" | "image/gif"));
            println!(
                "  {label}: {} bytes in -> {} bytes out ({}, {}x{}, {} frames)",
                input.len(),
                encoded.bytes.len(),
                encoded.content_type,
                encoded.width,
                encoded.height,
                encoded.frames
            );
        }
    }

    /// Dedup rests on the re-encoder being deterministic.
    #[test]
    fn re_encoding_is_deterministic_which_is_what_makes_dedup_work() {
        let png = noisy_png(256, 256);
        let a = encode_emoji(&png).unwrap();
        let b = encode_emoji(&png).unwrap();
        assert_eq!(a.bytes, b.bytes, "same input must give byte-identical output");
        assert_eq!(sha256_hex(&a.bytes), sha256_hex(&b.bytes));

        let gif = noisy_gif(96, 96, 20);
        assert_eq!(
            encode_emoji(&gif).unwrap().bytes,
            encode_emoji(&gif).unwrap().bytes
        );
    }

    #[test]
    fn a_modest_animation_survives_as_an_animation() {
        // Flat-coloured frames, i.e. what a real animated emoji looks like.
        use image::codecs::gif::GifEncoder;
        use image::{Delay, Frame};
        let mut input = Vec::new();
        {
            let mut encoder = GifEncoder::new_with_speed(&mut input, 30);
            for i in 0..24u8 {
                let mut img = image::RgbaImage::new(128, 128);
                for pixel in img.pixels_mut() {
                    *pixel = image::Rgba([i.wrapping_mul(10), 80, 200, 255]);
                }
                encoder
                    .encode_frame(Frame::from_parts(img, 0, 0, Delay::from_numer_denom_ms(40, 1)))
                    .unwrap();
            }
        }
        let encoded = encode_emoji(&input).unwrap();
        assert!(encoded.animated, "a small animation must stay animated");
        assert_eq!(encoded.content_type, "image/gif");
        assert!(encoded.frames > 1);
        assert!(encoded.bytes.len() <= EMOJI_MAX_BYTES);
    }

    /// The decoder-attack defences, each rejected at the layer it is aimed at.
    #[test]
    fn hostile_input_is_refused_before_it_is_decoded() {
        // Not an image at all.
        assert!(encode_emoji(b"<svg onload=alert(1)/>").is_err());
        assert!(encode_emoji(b"").is_err());
        assert!(encode_emoji(b"\x00\x01\x02\x03").is_err());
        // Truncated PNG: a valid signature with nothing behind it.
        let mut truncated = noisy_png(64, 64);
        truncated.truncate(40);
        assert!(encode_emoji(&truncated).is_err());
        // Over the input ceiling — refused on length, before any parse.
        let huge = vec![0u8; EMOJI_MAX_INPUT_BYTES + 1];
        assert!(encode_emoji(&huge).is_err());
    }

    #[test]
    fn oversized_dimensions_are_refused_from_the_header() {
        // A 5000x8 PNG is tiny on disk but over the ceiling in one axis.
        let wide = noisy_png(5000, 8);
        let err = encode_emoji(&wide).unwrap_err().to_string();
        assert!(err.contains("limit is"), "unexpected error: {err}");
    }

    #[test]
    fn a_small_source_is_not_scaled_up() {
        let encoded = encode_emoji(&noisy_png(16, 16)).unwrap();
        assert_eq!((encoded.width, encoded.height), (16, 16));
    }

    #[test]
    fn aspect_ratio_is_preserved() {
        let encoded = encode_emoji(&noisy_png(512, 128)).unwrap();
        assert_eq!(encoded.width, EMOJI_TARGET_DIM);
        assert_eq!(encoded.height, EMOJI_TARGET_DIM / 4);
    }

    #[test]
    fn subsampling_preserves_total_duration() {
        let frames: Vec<RawFrame> = (0..40)
            .map(|_| RawFrame {
                image: image::RgbaImage::new(1, 1),
                delay_ms: 50,
            })
            .collect();
        let kept = subsample(&frames, 10);
        assert!(kept.len() <= 10);
        let total: u32 = kept.iter().map(|(_, d)| *d).sum();
        assert_eq!(
            total, 2000,
            "40 frames x 50ms is 2s; the subsampled animation must still last 2s"
        );
    }
}
