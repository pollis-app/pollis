//! Custom per-group emoji (#848): re-encoding, upload, listing, and the "which
//! emoji may this user send" rule. See `.codesight/wiki/commands.md` § emoji.
//!
//! Emoji objects are content-addressed and deliberately unencrypted in R2: a
//! member of group A may use A's emoji in group B, and B's members must render
//! it with no key to A, so an encrypted emoji couldn't cross a group boundary.
//! Message content stays E2EE; only `<:shortcode:hash>` leaves the envelope.
//!
//! [`encode_emoji`] is the one place that parses an image a stranger chose, and
//! its output is always under [`EMOJI_MAX_BYTES`] or an error. Defences, in
//! order: input size cap; format sniffed from magic bytes and allowlisted (no
//! MIME or extension consulted); header dimensions checked before decoding;
//! decode under `image::Limits`; frame cap; anything that fails to re-encode is
//! rejected, so nothing reaches R2 this module didn't produce.
//!
//! Shortcodes are `[a-z0-9_]{2,32}`: no `<`, `&`, `:`, `/` or whitespace, so they
//! can neither carry markup nor break the token grammar.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::commands::r2;
use crate::error::{Error, Result};
use crate::state::AppState;

/// Hard ceiling on one stored emoji object. Must equal
/// `pollis_delivery::emoji::EMOJI_MAX_BYTES`; the DS binds it into the presigned
/// PUT, so a mismatch is a refused upload.
pub const EMOJI_MAX_BYTES: usize = 48 * 1024;

/// Largest source file [`encode_emoji`] looks at; checked before parsing.
pub const EMOJI_MAX_INPUT_BYTES: usize = 8 * 1024 * 1024;

/// Largest source dimension, checked from the header before a full decode.
pub const EMOJI_MAX_SOURCE_DIM: u32 = 4096;

/// Cap on decoded animation frames, bounding a GIF bomb's decode time.
pub const EMOJI_MAX_SOURCE_FRAMES: usize = 300;

/// Decode-side allocation ceiling for `image::Limits`; catches a lying header.
const EMOJI_MAX_DECODE_ALLOC: u64 = 256 * 1024 * 1024;

/// Largest square the re-encoder targets: 64 px survives a 2× display without
/// paying for unseen pixels.
const EMOJI_TARGET_DIM: u32 = 64;

/// The degrade ladder [`encode_emoji`] walks until the output fits.
const SIZE_LADDER: [u32; 4] = [EMOJI_TARGET_DIM, 56, 48, 32];

/// Frame budgets paired with the size ladder for animations.
const FRAME_LADDER: [usize; 4] = [60, 40, 24, 12];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CustomEmoji {
    pub group_id: String,
    /// The group's display name, denormalised so the picker needs no per-group round
    /// trip.
    pub group_name: String,
    pub shortcode: String,
    pub content_hash: String,
    pub content_type: String,
    pub animated: bool,
    pub size_bytes: u64,
    pub created_by: String,
}

/// `bytes` is guaranteed `<= EMOJI_MAX_BYTES`.
#[derive(Debug, Clone)]
pub struct EncodedEmoji {
    pub bytes: Vec<u8>,
    /// `image/webp` or `image/gif`, exactly what the DS allowlists.
    pub content_type: &'static str,
    pub animated: bool,
    pub width: u32,
    pub height: u32,
    pub frames: usize,
}

/// `[a-z0-9_]{2,32}`. Duplicated from `pollis_delivery::emoji::shortcode_is_valid`
/// (no dependency on the DS crate); the DS re-validates, so drift can only make
/// the client stricter.
pub fn shortcode_is_valid(shortcode: &str) -> bool {
    let len = shortcode.len();
    if !(2..=32).contains(&len) {
        return false;
    }
    shortcode
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// 64 lowercase hex characters. Case-sensitive so one image can't occupy two
/// rows.
pub fn content_hash_is_valid(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Mirrors `pollis_delivery::emoji::emoji_r2_key`.
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmojiToken {
    /// Byte range of the whole `<:name:hash>` run.
    pub start: usize,
    pub end: usize,
    pub shortcode: String,
    pub content_hash: String,
}

/// Scan `text` for `<:shortcode:content_hash>` references. Anything not fully
/// well-formed stays ordinary text. Unambiguous because neither field's alphabet
/// contains `:` or `>`.
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

pub fn emoji_token_text(shortcode: &str, content_hash: &str) -> String {
    format!("<:{shortcode}:{content_hash}>")
}

/// Re-encode untrusted image bytes into a small emoji object: always
/// `<= EMOJI_MAX_BYTES` or an error, never the input passed through.
///
/// An animation that won't fit even at the bottom of both ladders falls back to
/// its first frame as a static image, which makes the bound unconditional.
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

struct RawFrame {
    image: image::RgbaImage,
    delay_ms: u32,
}

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
    // The decoder's message can quote attacker-controlled bytes, so it isn't
    // surfaced.
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

/// Decode every frame (capped) as RGBA. A still GIF/WebP yields one frame and
/// takes the static path: "animated" means it actually moves.
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
            // GIFs use delay 0 for "as fast as possible"; normalise rather than trust it.
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

    // Limited reader, so a lying header can't force a large allocation.
    let mut reader = image::ImageReader::new(Cursor::new(input));
    reader.set_format(format);
    reader.limits(decode_limits());
    let image = reader.decode().map_err(decode_err)?;
    Ok(vec![RawFrame {
        image: image.to_rgba8(),
        delay_ms: 100,
    }])
}

/// Scale to fit `target`, preserving aspect ratio; never scales up.
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
/// `image`'s WebP encoder is lossless-only, which suits flat-colour emoji art.
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

/// Encode an animation as GIF, walking the (size, frame-budget) ladder. `Ok(None)`
/// means no rung fit; the caller falls back to a static frame.
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

/// Reduce `frames` to at most `budget`, folding dropped frames' delays forward so
/// playback speed is unchanged.
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
        // Speed 30, the fastest quantiser: no visible difference for tiny flat emoji.
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

/// Every custom emoji `user_id` may use, across all their groups: both the
/// picker's source and the send-permission rule, so they can't drift. Membership
/// of any registering group grants use in any conversation (#848); there is
/// deliberately no conversation argument.
pub async fn list_usable_emoji(_user_id: String, state: &Arc<AppState>) -> Result<Vec<CustomEmoji>> {
    let body = pollis_api::account_reads::EmojiReadBody {
        want_usable: true,
        content_hashes: Vec::new(),
    };
    let resp = crate::commands::mls::ds_post_json(state, &body).await?;
    Ok(resp.usable.into_iter().map(emoji_from_wire).collect())
}

/// One group's emoji, for the management page. Members-only (#917), which doesn't
/// contradict #848: rendering resolves a single hash via `get_emoji_url` with no
/// membership check, while enumerating a group's set is group metadata.
pub async fn list_group_emoji(
    group_id: String,
    requester_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<CustomEmoji>> {
    // Membership and the emoji set in one request; the DS applies the members-only
    // rule.
    let resp = crate::commands::ds_reads::group(state, &group_id, &requester_id, true).await?;
    if !resp.authorized {
        return Err(Error::Other(anyhow::anyhow!(
            crate::commands::groups::authz::NOT_A_MEMBER
        )));
    }
    Ok(resp.emoji.into_iter().map(emoji_from_wire).collect())
}

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


/// Add an emoji to a group from a file on disk (size-checked from metadata before
/// reading).
///
/// Re-encode first so dedup keys on the stored bytes; skip the upload if the
/// hash already exists; register last, since a row pointing at a missing object
/// is a broken emoji while an unregistered object is garbage the sweep collects.
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
    // The postcondition, asserted at the boundary rather than trusted.
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

    // `created_by` is the DS's no-auth fallback actor (`resolve_actor`); when
    // signed, it must equal the authenticated user.
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

/// Remove an emoji from a group, then sweep what that orphaned (event-driven, no
/// timer). A failed blob delete is logged; it strands at most a 48 KiB object.
pub async fn remove_group_emoji(
    group_id: String,
    shortcode: String,
    state: &Arc<AppState>,
) -> Result<()> {
    // Same no-auth fallback contract as `/v1/emoji/create`.
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

/// Concurrent blob deletions in a GC sweep (#915): each is a DS presign plus an
/// R2 DELETE, so this also bounds load on our own DS.
const EMOJI_GC_DELETE_CONCURRENCY: usize = 8;

/// Ask the DS to collect unreferenced emoji objects, then delete their R2 blobs.
/// Idempotent.
pub async fn collect_orphaned_emoji(state: &Arc<AppState>) -> Result<()> {
    let parsed =
        crate::commands::mls::ds_post_json(state, &pollis_api::emoji::EmojiGcBody {}).await?;

    // Bounded concurrency, best-effort per item: the row is already gone, and one
    // failure must not abandon the sweep.
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

/// Resolve an emoji to a loopback URL for `<img src>`, via the same on-disk cache
/// and media server as `get_media_url`. The content type is looked up from the
/// row rather than passed in, since the wire token carries only the hash and a
/// wrong guess derives the wrong R2 key. Skipped on a cache hit.
pub async fn get_emoji_url(content_hash: String, state: &Arc<AppState>) -> Result<String> {
    if !content_hash_is_valid(&content_hash) {
        return Err(Error::Other(anyhow::anyhow!("malformed emoji content hash")));
    }
    let url = crate::media_server::loopback_url(state, &content_hash).await?;

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

    // Atomic write: a half-written cache file would be served as a corrupt image.
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

/// Fetch an emoji object from R2 and verify it against its content hash, for
/// [`get_emoji_url`] and mobile's `get_emoji_path`. The object is unencrypted, so
/// this hash check is the only thing attesting the bytes; every consumer goes
/// through it.
pub async fn download_verified_emoji(
    content_hash: &str,
    state: &Arc<AppState>,
) -> Result<(Vec<u8>, String)> {
    if !content_hash_is_valid(content_hash) {
        return Err(Error::Other(anyhow::anyhow!("malformed emoji content hash")));
    }
    let (r2_key, content_type) = {
        // Deliberately not membership-gated (#848); see `list_group_emoji`.
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
    // Re-derive the key rather than follow a stored string into an arbitrary path.
    let expected_key = emoji_r2_key(content_hash, &content_type)?;
    if r2_key != expected_key {
        return Err(Error::Other(anyhow::anyhow!(
            "emoji {content_hash} has an unexpected object key; refusing to fetch it"
        )));
    }

    let get_url = r2::presign_r2_with_length(state, "get", &expected_key, None).await?;
    let overlay = state.overlay_handle();
    // The ceiling is passed in: reading an unbounded body before measuring it would
    // be a memory bomb.
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
    // `Vec::from` moves a uniquely-owned `Bytes` allocation (#915).
    Ok((Vec::from(bytes), content_type))
}

/// Strip custom-emoji tokens `user_id` may not use from outgoing text, leaving
/// `:name:`. Enforced on the sender's device because the DS never sees E2EE
/// content. Degrades rather than refuses the send.
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

/// The pure half of [`prepare_emoji_text`], testable without I/O.
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

    /// Markup inside a would-be token can't produce a token.
    #[test]
    fn markup_cannot_ride_inside_a_token() {
        let hash = h('a');
        let hostile = format!("<:<img src=x onerror=alert(1)>:{hash}>");
        assert!(parse_emoji_tokens(&hostile).is_empty());

        assert!(!shortcode_is_valid("<img src=x>"));
        assert!(!shortcode_is_valid("a:b"));
        assert!(!shortcode_is_valid("../../etc/passwd"));
    }

    /// The sender-side #848 rule on the ticket's example: B (in group A) keeps A's
    /// emoji while typing in group B; C (in neither) degrades to `:parrot:`.
    #[test]
    fn a_disallowed_emoji_degrades_to_its_shortcode_and_nothing_else_moves() {
        let parrot = h('a');
        let stranger = h('b');
        let allowed: std::collections::HashSet<&str> =
            [parrot.as_str()].into_iter().collect();

        let b_text = format!("morning all <:parrot:{parrot}> — standup in 5");
        assert_eq!(strip_disallowed_emoji(&b_text, &allowed), b_text);

        let c_text = format!("morning all <:parrot:{stranger}> — standup in 5");
        assert_eq!(
            strip_disallowed_emoji(&c_text, &allowed),
            "morning all :parrot: — standup in 5"
        );

        let mixed = format!("<:parrot:{parrot}><:parrot:{stranger}>");
        assert_eq!(
            strip_disallowed_emoji(&mixed, &allowed),
            format!("<:parrot:{parrot}>:parrot:")
        );

        for untouched in ["", "no emoji here", "<:oops:>", "a :parrot: b"] {
            assert_eq!(strip_disallowed_emoji(untouched, &allowed), untouched);
        }

        // Multi-byte text next to a stripped token must not be split.
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

    /// Enough colour variation that the size bound is actually exercised.
    fn noisy_png(w: u32, h: u32) -> Vec<u8> {
        use image::ImageEncoder as _;
        let mut img = image::RgbaImage::new(w, h);
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

    /// A flat PNG: the only way to reach the 4096 px ceiling without exceeding the
    /// 8 MiB input cap (4096² of noise is 55 MB).
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

    /// Full-size noise frames, the worst case for the size bound.
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

    /// The size bound (#848) on every shape of input, including hostile ones.
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

    /// Dedup relies on the re-encoder being deterministic.
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

    #[test]
    fn hostile_input_is_refused_before_it_is_decoded() {
        assert!(encode_emoji(b"<svg onload=alert(1)/>").is_err());
        assert!(encode_emoji(b"").is_err());
        assert!(encode_emoji(b"\x00\x01\x02\x03").is_err());
        let mut truncated = noisy_png(64, 64);
        truncated.truncate(40);
        assert!(encode_emoji(&truncated).is_err());
        let huge = vec![0u8; EMOJI_MAX_INPUT_BYTES + 1];
        assert!(encode_emoji(&huge).is_err());
    }

    /// A source over the dimension ceiling is refused from its header, before pixels
    /// are allocated.
    #[test]
    fn oversized_dimensions_are_refused_from_the_header() {
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
