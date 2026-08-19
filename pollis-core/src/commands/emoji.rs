//! Custom per-group emoji (#848) — re-encoding, upload, listing, and the
//! "which emoji may this user send" rule.
//!
//! ## What is stored, and why it is not encrypted
//!
//! An emoji is a **content-addressed, unencrypted R2 object** plus a metadata
//! row per group that uses it (see the DS module `pollis_delivery::emoji` and
//! migration `000015_custom_emoji.sql`). Everything else Pollis puts in R2 is
//! convergently encrypted; this deliberately is not, and the reason is
//! structural. #848 asks for Discord's rule: a member of group A may use A's
//! emoji while talking in group B, and everyone in B must RENDER it even though
//! they have no access to A. There is no key a B-only member could hold that
//! would decrypt an A-scoped secret — an encrypted emoji is an emoji that cannot
//! cross a group boundary. Message CONTENT stays E2EE exactly as before; what
//! leaves the envelope is `<:shortcode:hash>`, and the operator could already
//! see that an object exists.
//!
//! ## The re-encoder is the whole size story
//!
//! [`encode_emoji`] never stores what the user picked off disk. It decodes,
//! shrinks, and re-encodes to WebP (static) or GIF (animated), degrading
//! resolution and then frame count until the result fits [`EMOJI_MAX_BYTES`]
//! (48 KiB). That ceiling is a **postcondition**, not a check: the function
//! either returns bytes under it or returns an error, and
//! `the_size_bound_holds_for_every_input` in the tests below pins that.
//!
//! It is also the app's main **decoder attack surface** — it is the one place
//! that parses an image a stranger chose. So, in order:
//!
//!   1. The input is size-capped ([`EMOJI_MAX_INPUT_BYTES`]) before anything
//!      parses it.
//!   2. The format is SNIFFED from magic bytes and allowlisted. No declared MIME
//!      and no file extension is consulted anywhere in this module — the caller
//!      does not even pass one.
//!   3. Dimensions are read from the header and rejected above
//!      [`EMOJI_MAX_SOURCE_DIM`] *before* a full decode, so a 60000×60000 PNG
//!      header never becomes a 14 GB allocation.
//!   4. The decode itself runs under `image::Limits`, which caps allocation
//!      independently of what the header claimed.
//!   5. Animations are capped at [`EMOJI_MAX_SOURCE_FRAMES`] decoded frames, so
//!      a GIF bomb is bounded in time as well as space.
//!   6. Anything that fails to decode, or that decodes but cannot be re-encoded,
//!      is REJECTED rather than passed through. Nothing reaches R2 that this
//!      module did not itself produce.
//!
//! ## Shortcodes cannot be an injection vector
//!
//! A shortcode is `[a-z0-9_]{2,32}`, validated here and again at the DS
//! chokepoint (which is authoritative). The charset contains no `<`, `&`, `:`,
//! `/` or whitespace, so a shortcode can neither carry markup into rendered
//! message text nor break the `<:name:hash>` token grammar
//! [`parse_emoji_tokens`] scans for. Rendering never interpolates a shortcode
//! into markup anyway — but a grammar whose alphabet makes the attack
//! unrepresentable is worth more than a rule about how to render it.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::commands::r2;
use crate::error::{Error, Result};
use crate::state::AppState;

// ── Bounds (the DS is authoritative; these must agree) ───────────────────────

/// Hard ceiling on ONE stored emoji object. Must equal
/// `pollis_delivery::emoji::EMOJI_MAX_BYTES` — the DS re-checks it on
/// registration and binds it into the presigned PUT's signature, so a
/// disagreement shows up as a refused upload rather than as a silently larger
/// object.
pub const EMOJI_MAX_BYTES: usize = 48 * 1024;

/// Largest source file [`encode_emoji`] will even look at. Rejected before any
/// parsing, so a huge file costs a length check.
pub const EMOJI_MAX_INPUT_BYTES: usize = 8 * 1024 * 1024;

/// Largest source dimension accepted, checked from the header before a full
/// decode. Generous for a real emoji (nobody uploads a 4096px party parrot on
/// purpose) and small enough that the subsequent decode is bounded.
pub const EMOJI_MAX_SOURCE_DIM: u32 = 4096;

/// Cap on decoded animation frames. Bounds decode time for a GIF bomb — a file
/// whose frame count, not whose dimensions, is the weapon.
pub const EMOJI_MAX_SOURCE_FRAMES: usize = 300;

/// Ceiling on the DECODE-side allocation, handed to `image::Limits`. Independent
/// of the header check above: a lying header is caught here instead.
const EMOJI_MAX_DECODE_ALLOC: u64 = 256 * 1024 * 1024;

/// The rendered size an emoji is displayed at, and so the largest square the
/// re-encoder ever targets. Discord renders custom emoji at 32-48 px inline and
/// stores 128 px; 64 px is the compromise that survives a 2× display without
/// paying for pixels nobody sees.
const EMOJI_TARGET_DIM: u32 = 64;

/// The degrade ladder. [`encode_emoji`] walks it until the output fits, so
/// "under 48 KiB" is reached by making the image smaller rather than by giving
/// up. Static images essentially never leave the first rung; animations may.
const SIZE_LADDER: [u32; 4] = [EMOJI_TARGET_DIM, 56, 48, 32];

/// Frame budgets paired with the size ladder for animations. An emoji is a
/// gesture, not a film — 60 frames is already a long loop, and 12 still reads as
/// motion.
const FRAME_LADDER: [usize; 4] = [60, 40, 24, 12];

// ── Wire types (keep in sync with `frontend/src/types`) ──────────────────────

/// One emoji as the UI sees it: the group that registered it, its shortcode, and
/// the object it points at.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CustomEmoji {
    pub group_id: String,
    /// The group's display name, for the picker's section headers. Denormalised
    /// into the row on read because the picker groups by source group and would
    /// otherwise need a second round trip per group.
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
    /// `image/webp` or `image/gif` — the only two this module emits, and exactly
    /// the two the DS allowlists.
    pub content_type: &'static str,
    pub animated: bool,
    pub width: u32,
    pub height: u32,
    /// 1 for a static image.
    pub frames: usize,
}

// ── Validation (mirrors the DS; the DS is authoritative) ─────────────────────

/// `[a-z0-9_]{2,32}`. Duplicated from `pollis_delivery::emoji::shortcode_is_valid`
/// because pollis-core must not depend on the DS crate. The DS re-validates, so
/// a drift here can only ever make the client stricter, never the server looser.
pub fn shortcode_is_valid(shortcode: &str) -> bool {
    let len = shortcode.len();
    if !(2..=32).contains(&len) {
        return false;
    }
    shortcode
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// 64 lowercase hex characters — the exact form every content hash is written
/// in. Case-sensitive on purpose: accepting both cases would let one image
/// occupy two rows and two blobs.
pub fn content_hash_is_valid(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The R2 key an emoji object lives at. Mirrors `pollis_delivery::emoji::
/// emoji_r2_key`; the DS derives the same key and stores ITS version, so the two
/// agreeing is checked by the upload succeeding.
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

// ── The `<:shortcode:hash>` token grammar ────────────────────────────────────

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
/// Both halves must be well-formed for the run to count as a token; anything
/// else is ordinary text and is left alone. That is deliberate — a message
/// containing `<:not a real token:>` must render as those literal characters,
/// not disappear, and the parser must not be the thing that decides otherwise.
///
/// The grammar is unambiguous because neither field's alphabet contains `:` or
/// `>`, so a token cannot be nested inside another or run past its terminator.
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

        // `from_utf8` cannot fail here: the alphabet check below is ASCII-only,
        // and a multi-byte character would fail it anyway.
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

// ── The re-encoder ───────────────────────────────────────────────────────────

/// Re-encode arbitrary, untrusted image bytes into a small emoji object.
///
/// Returns bytes that are guaranteed `<= EMOJI_MAX_BYTES`, or an error. There is
/// no third outcome: the function never returns something oversized and never
/// passes the input through unmodified. See the module docs for the ordered list
/// of defences and why each one is where it is.
///
/// Animated input that cannot be squeezed under the ceiling even at the bottom
/// of both ladders falls back to its FIRST FRAME as a static image. A still
/// party-parrot is a worse emoji than an animated one and a much better outcome
/// than a refused upload or an unbounded object, and it makes the postcondition
/// unconditional: the static path's floor (a 32×32 lossless WebP) is a few
/// kilobytes for any input at all.
pub fn encode_emoji(input: &[u8]) -> Result<EncodedEmoji> {
    use image::ImageFormat;

    if input.is_empty() {
        return Err(Error::Other(anyhow::anyhow!("emoji file is empty")));
    }
    // (1) Cap the input before anything parses it.
    if input.len() > EMOJI_MAX_INPUT_BYTES {
        return Err(Error::Other(anyhow::anyhow!(
            "emoji source is {} bytes; the limit is {EMOJI_MAX_INPUT_BYTES}",
            input.len()
        )));
    }

    // (2) Sniff the format from magic bytes and allowlist it. The caller never
    // supplies a MIME type, so there is nothing here to be lied to about.
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

    // (3) Header dimensions, before a full decode.
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

    // (4)+(5) Decode under allocation limits, with a frame cap for animations.
    let frames = decode_frames(input, format)?;
    if frames.is_empty() {
        return Err(Error::Other(anyhow::anyhow!("image contains no frames")));
    }

    if frames.len() > 1 {
        if let Some(encoded) = encode_animated(&frames)? {
            return Ok(encoded);
        }
        // Fell off the bottom of both ladders — keep the first frame. See the
        // doc comment: this is what makes the postcondition unconditional.
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
    // The decoder's own message can quote attacker-controlled bytes, so it is
    // deliberately not surfaced verbatim; the shape of the failure is all a user
    // can act on anyway.
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
///
/// GIF and animated WebP go through `AnimationDecoder`; everything else is a
/// single frame. A still GIF/WebP simply yields one frame and takes the static
/// path, which is what we want — "animated" should mean "actually moves", not
/// "was stored in a container that can move".
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
            // A zero or absurd delay is normalised, not trusted: GIFs in the
            // wild carry 0 to mean "as fast as possible", and browsers clamp.
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

    // Static path: a limited reader so a lying header cannot force a large
    // allocation.
    let mut reader = image::ImageReader::new(Cursor::new(input));
    reader.set_format(format);
    reader.limits(decode_limits());
    let image = reader.decode().map_err(decode_err)?;
    Ok(vec![RawFrame {
        image: image.to_rgba8(),
        delay_ms: 100,
    }])
}

/// Scale to fit inside `target`×`target`, preserving aspect ratio. Never scales
/// UP — a 16×16 pixel-art emoji stays 16×16 rather than being blurred to 64.
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
/// Lossless rather than lossy because `image`'s WebP encoder is lossless-only,
/// and because emoji are flat-colour artwork with hard edges — exactly the
/// content lossless WebP compresses well and lossy compression ruins. At 64×64
/// that lands in the low single-digit kilobytes for real artwork.
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
/// `Ok(None)` means every rung was still too big — the caller falls back to a
/// static first frame. `Err` is a real encoder failure.
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
///
/// Folding the delays is the part that matters: dropping every other frame
/// without doubling the remaining delays makes the animation play twice as fast,
/// which is a visibly different emoji rather than a smaller one.
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
        // Speed 30 is the encoder's fastest quantiser. Emoji are tiny and
        // flat-coloured, so the quality difference against speed 1 is invisible
        // while the time difference on a 60-frame animation is not.
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

// ── Commands ─────────────────────────────────────────────────────────────────

/// Every custom emoji `user_id` may USE, across every group they are a member
/// of. This is the picker's source AND the permission rule in one query, which
/// is the point: the set the UI offers and the set the rule permits cannot drift
/// because they are the same set.
///
/// Discord's rule exactly (#848): membership of ANY registering group grants use
/// in ANY conversation. Note the absence of a conversation argument — scoping
/// this to the conversation is the obvious-looking rule and the wrong one.
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
/// #917: members-only, and that does **not** contradict #848.
///
/// #848 requires that a user who is in NONE of an emoji's groups still SEES it
/// when someone sends it. That requirement is discharged entirely by the render
/// path — `get_emoji_url` takes a content hash, verifies `sha256(bytes)` and
/// serves the object, with no group argument and deliberately no membership
/// check. Nothing in rendering reaches this function.
///
/// Resolving one hash you were handed and enumerating a group's whole emoji set
/// are different acts. The first is "show me the decoration in this message I
/// can already read"; the second is "tell me what that group has", which is
/// group metadata — the shortcodes alone (`:q3-layoffs-parrot:`) leak the same
/// way channel names do, plus `created_by` names members. So the hash stays
/// open and the LIST closes, which is the narrowest rule that keeps #848's
/// cross-group rendering intact.
///
/// The picker never used this: `list_usable_emoji` has always joined through
/// `group_member`, because it is simultaneously the picker's source and the
/// send-permission rule. This function is the management page's list, and the
/// management page is only reachable from inside the group.
pub async fn list_group_emoji(
    group_id: String,
    requester_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<CustomEmoji>> {
    // Membership and the emoji set in ONE request. The DS applies the same
    // members-only rule this preflight expressed, using the same predicate it
    // uses to authorize the WRITES to `group_emoji`.
    let resp = crate::commands::ds_reads::group(state, &group_id, &requester_id, true).await?;
    if !resp.authorized {
        return Err(Error::Other(anyhow::anyhow!(
            crate::commands::groups::authz::NOT_A_MEMBER
        )));
    }
    Ok(resp.emoji.into_iter().map(emoji_from_wire).collect())
}

/// Wire → domain for one custom emoji. One conversion, so the usable-set and
/// per-group paths cannot map the same row differently.
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

async fn collect_emoji(rows: &mut libsql::Rows) -> Result<Vec<CustomEmoji>> {
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(CustomEmoji {
            group_id: row.get(0)?,
            group_name: row.get(1)?,
            shortcode: row.get(2)?,
            content_hash: row.get(3)?,
            content_type: row.get(4)?,
            animated: row.get::<i64>(5)? != 0,
            size_bytes: row.get::<i64>(6)?.max(0) as u64,
            created_by: row.get(7)?,
        });
    }
    Ok(out)
}

/// Add an emoji to a group from a file on disk.
///
/// The file is read here rather than passed as bytes over IPC, matching
/// `upload_media` — and the read is size-checked from the filesystem metadata
/// first, so an 8 GB file costs a `stat` rather than 8 GB of RSS.
///
/// Order of operations, and why:
///
///   1. **Re-encode first.** The content hash is of the RE-ENCODED bytes, so
///      dedup keys on what is actually stored. Hashing the source instead would
///      mean two people uploading the same picture in different formats stored
///      it twice — the exact "fifteen party-parrots" failure.
///   2. **Probe Turso for the hash.** A hit means the bytes are already in R2;
///      skip the upload entirely.
///   3. **Upload under a length-bound presign,** then register through the DS.
///      Registration last, because a registered row pointing at a missing object
///      is a broken emoji, while an uploaded object with no row is merely
///      garbage the sweep collects.
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
    // The postcondition, asserted at the boundary rather than trusted. If the
    // encoder ever regresses, the upload fails here instead of putting an
    // oversized object in a public bucket.
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

    // `created_by` is the DS's no-auth fallback for the acting user
    // (`resolve_actor`): with request signing off, a body without it resolves to
    // no actor and the endpoint 403s outright. On the signed path it is bound to
    // — and must equal — the authenticated user, so naming it grants nothing.
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
/// The sweep runs here — event-driven, on the action that can create garbage —
/// rather than on a timer, per the repo's no-polling rule. The DS returns the R2
/// keys it collected, and this deletes those blobs; a failed blob delete is
/// logged and ignored, because the row is already gone and the next sweep will
/// not re-offer it. That leaves at worst an orphaned object, which is a cost
/// bound by the same 48 KiB ceiling as everything else here.
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
/// Each deletion costs a DS presign plus an R2 DELETE, so this is a concurrency
/// bound on our OWN delivery service as much as on R2. Eight keeps a large sweep
/// from serialising into hundreds of sequential round trips without turning it
/// into a burst.
const EMOJI_GC_DELETE_CONCURRENCY: usize = 8;

/// Ask the DS to collect every unreferenced emoji object, then delete the R2
/// blobs it collected. Safe to call at any time: it is idempotent and can only
/// touch objects no group references.
pub async fn collect_orphaned_emoji(state: &Arc<AppState>) -> Result<()> {
    let parsed =
        crate::commands::mls::ds_post_json(state, &pollis_api::emoji::EmojiGcBody {}).await?;

    // Delete the collected blobs with bounded concurrency (#915).
    //
    // Each deletion is TWO sequential network round trips — a DS presign, then
    // the R2 DELETE — and they were being issued strictly one after another, so
    // a sweep that collected 200 objects took 400 serialised round trips for
    // work with no ordering requirement between items. Bounded rather than
    // unbounded: the presign half hits our own DS, and a sweep is exactly the
    // kind of thing that should not look like a burst to it.
    //
    // Still best-effort per item. The Turso row is already gone, so a failure
    // strands a <=48 KiB blob rather than breaking anything, and one failure
    // must not abandon the rest of the sweep.
    use futures_util::stream::StreamExt as _;
    futures_util::stream::iter(parsed.r2_keys)
        .for_each_concurrent(EMOJI_GC_DELETE_CONCURRENCY, |key| async move {
            if let Err(e) = r2::delete_r2_object(state, &key).await {
                eprintln!("[emoji] could not delete collected object {key}: {e}");
            }
        })
        .await;
    Ok(())
}

/// Resolve an emoji to a loopback URL the webview can use as `<img src>`.
///
/// Same shape as `get_media_url`, and it writes into the SAME on-disk cache, so
/// the existing media server serves it with no changes. Two things differ.
///
/// First, the fetch: an emoji object is stored in the clear, so there is no
/// convergent decryption step — and that makes the integrity check load-bearing
/// rather than belt-and-braces. Nothing but the hash proves these bytes are the
/// emoji that was registered, so the hash is verified before the bytes are
/// cached, and an oversized response is refused outright.
///
/// Second, the content type is LOOKED UP, not passed in. It could have been a
/// parameter, but the wire token (`<:name:hash>`) carries only the hash, so
/// every caller would have had to guess — and a guess of `image/webp` for an
/// animated emoji derives the wrong R2 key and 404s. Deriving it from the row
/// that owns the object makes that mismatch unrepresentable. The lookup is
/// skipped entirely on a cache hit, which is the common case.
pub async fn get_emoji_url(content_hash: String, state: &Arc<AppState>) -> Result<String> {
    if !content_hash_is_valid(&content_hash) {
        return Err(Error::Other(anyhow::anyhow!("malformed emoji content hash")));
    }
    let port = state
        .media_server_port
        .lock()
        .await
        .ok_or_else(|| Error::Other(anyhow::anyhow!("media server not started")))?;
    let token = state
        .media_server_token
        .lock()
        .await
        .clone()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("media server token not set; not unlocked")))?;
    let url = format!("http://127.0.0.1:{port}/{token}/{content_hash}");

    // Cache hit needs no content type at all — the cached file's own extension
    // is what the media server serves it with.
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

    // Atomic write, mirroring `get_media_url`: a half-written cache file would
    // be served as a corrupt image rather than re-fetched.
    let mut tmp = target.clone();
    let final_ext = target.extension().and_then(|s| s.to_str()).unwrap_or("enc");
    tmp.set_extension(format!("{final_ext}.tmp"));
    tokio::fs::write(&tmp, &encrypted)
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("write emoji cache: {e}")))?;
    if let Err(e) = tokio::fs::rename(&tmp, &target).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(Error::Other(anyhow::anyhow!("rename emoji cache: {e}")));
    }
    Ok(url)
}

/// Fetch an emoji object from R2 and verify the bytes against their content
/// hash. The shared fetch core of [`get_emoji_url`] (desktop: cache-encrypts
/// the bytes and serves them over the loopback media server) and the mobile
/// bridge's `get_emoji_path` arm (no loopback server in a sandboxed RN app,
/// so the verified plaintext is materialised to a sandbox file instead).
///
/// An emoji object is stored in the clear, so the hash check here is the ONLY
/// thing attesting that these bytes are the emoji that was registered — every
/// consumer must go through it, which is why the check lives here and not in
/// the callers. Returns the verified bytes plus the row's content type.
pub async fn download_verified_emoji(
    content_hash: &str,
    state: &Arc<AppState>,
) -> Result<(Vec<u8>, String)> {
    if !content_hash_is_valid(content_hash) {
        return Err(Error::Other(anyhow::anyhow!("malformed emoji content hash")));
    }
    let (r2_key, content_type) = {
        // Deliberately NOT membership-gated (#848): resolving a hash you were
        // already handed is part of reading a message you can already read, and
        // is a different act from enumerating a group's emoji set — which IS
        // members-only, in `list_group_emoji`.
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
    // Re-derive the key rather than trusting the stored string: the DS derives
    // it the same way, so a disagreement is a bug worth failing on, not
    // something to follow into an arbitrary bucket path.
    let expected_key = emoji_r2_key(content_hash, &content_type)?;
    if r2_key != expected_key {
        return Err(Error::Other(anyhow::anyhow!(
            "emoji {content_hash} has an unexpected object key; refusing to fetch it"
        )));
    }

    let get_url = r2::presign_r2_with_length(state, "get", &expected_key, None).await?;
    let overlay = state.overlay_handle();
    let bytes = r2::r2_get_url(overlay.as_deref(), &get_url).await?;

    if bytes.len() > EMOJI_MAX_BYTES {
        return Err(Error::Other(anyhow::anyhow!(
            "emoji object {content_hash} is {} bytes, over the ceiling — refusing it",
            bytes.len()
        )));
    }
    let actual = sha256_hex(&bytes);
    if actual != content_hash {
        return Err(Error::Other(anyhow::anyhow!(
            "emoji object {content_hash} failed its content-hash check (got {actual}); \
             refusing to render substituted bytes"
        )));
    }
    // The public API owes an owned `Vec`; `Vec::from` moves the `Bytes`
    // allocation rather than copying it when it uniquely owns one (#915).
    Ok((Vec::from(bytes), content_type))
}

/// Strip any custom-emoji token `user_id` is not permitted to use from outgoing
/// message text, leaving the shortcode as plain `:name:` text.
///
/// This is the "use" rule applied at the last moment before a message is sealed.
/// It cannot live on the server: message content is E2EE, so the DS never learns
/// which emoji a message carries — that is the product working as designed, not
/// a gap. So the rule is enforced where the plaintext exists, on the sender's
/// device, against the same predicate that populates the picker
/// ([`list_usable_emoji`]).
///
/// It DEGRADES rather than refuses. A message is not worth failing over an
/// emoji, and `:parrot:` is what every other chat client shows for an emoji you
/// cannot render — the recipient sees a comprehensible message either way.
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
///
/// Split out from the I/O so the rule itself is directly testable — the DB
/// lookup is uninteresting, the rewriting is where an off-by-one would silently
/// eat a character of somebody's message.
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

    // ── The token grammar ────────────────────────────────────────────────────

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

    /// A malformed run must stay literal text. The parser is not allowed to
    /// "helpfully" interpret something that is not a token — a message
    /// containing `<:oops:>` has to render as those characters.
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

    /// The injection case, at the grammar level: markup inside a would-be token
    /// cannot produce a token, so a renderer that trusts tokens is never handed
    /// one containing markup.
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

    // ── The "use" rule, applied to outgoing text ─────────────────────────────

    /// The sender-side half of the #848 permission rule, on the exact worked
    /// example from the ticket.
    ///
    /// B is in group A, so B keeps group A's emoji **while typing in group B**;
    /// C is in neither, so C's copy of the same token degrades to plain
    /// `:parrot:` rather than failing the send. The membership set — never the
    /// conversation — is what decides, which is why B's line is the interesting
    /// assertion here.
    #[test]
    fn a_disallowed_emoji_degrades_to_its_shortcode_and_nothing_else_moves() {
        let parrot = h('a');
        let stranger = h('b');
        let allowed: std::collections::HashSet<&str> =
            [parrot.as_str()].into_iter().collect();

        // B, permitted: the token survives verbatim.
        let b_text = format!("morning all <:parrot:{parrot}> — standup in 5");
        assert_eq!(strip_disallowed_emoji(&b_text, &allowed), b_text);

        // C, not permitted: only the token is rewritten; every other byte of the
        // message is untouched.
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

        // Text with no tokens is returned byte-for-byte, including near-misses
        // that must not be touched.
        for untouched in ["", "no emoji here", "<:oops:>", "a :parrot: b"] {
            assert_eq!(strip_disallowed_emoji(untouched, &allowed), untouched);
        }

        // Multi-byte text around a stripped token survives — the rewrite slices
        // on byte offsets the parser produced, so a token adjacent to non-ASCII
        // must not split a character.
        let unicode = format!("héllo 🎉 <:parrot:{stranger}> wörld");
        assert_eq!(
            strip_disallowed_emoji(&unicode, &allowed),
            "héllo 🎉 :parrot: wörld"
        );
    }

    /// Nothing permitted at all — the empty-allow-set edge, which is what a user
    /// in no groups sees.
    #[test]
    fn with_nothing_permitted_every_token_degrades() {
        let allowed: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let text = format!("<:a_b:{}> <:c_d:{}>", h('a'), h('b'));
        assert_eq!(strip_disallowed_emoji(&text, &allowed), ":a_b: :c_d:");
    }

    // ── The re-encoder ───────────────────────────────────────────────────────

    /// A deterministic test image with enough colour variation that it does not
    /// compress to nothing — a solid square would prove nothing about the size
    /// bound.
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

    /// A flat-coloured PNG — what a real emoji actually looks like, and the only
    /// way to reach the 4096px dimension ceiling without exceeding the 8 MiB
    /// input ceiling first (4096² of noise is 55 MB on disk).
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

    /// A deterministic animated GIF: `frames` frames of full-size noise, which is
    /// as hostile as an animation gets for the size bound.
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

    /// **The size bound, on every shape of input including the hostile ones.**
    ///
    /// This is the assertion #848 asks for: "the size bound actually holds after
    /// re-encode". It is a postcondition of `encode_emoji`, so it is checked for
    /// large photographic noise (worst case for a lossless codec), for long
    /// animations, and for the degenerate small cases — not just for a
    /// well-behaved sample.
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

    /// Dedup rests on the re-encoder being deterministic: the same source must
    /// produce byte-identical output, or the "same image twice = one object"
    /// promise is decoration.
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

    /// An animation stays animated when it can, which is the whole point of
    /// paying for the GIF path.
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

    /// A source larger than the dimension ceiling is refused from its HEADER —
    /// the point being that the rejection happens before the pixels are
    /// allocated, so an image-bomb costs a header parse.
    #[test]
    fn oversized_dimensions_are_refused_from_the_header() {
        // A 5000x8 PNG is tiny on disk but over the ceiling in one axis.
        let wide = noisy_png(5000, 8);
        let err = encode_emoji(&wide).unwrap_err().to_string();
        assert!(err.contains("limit is"), "unexpected error: {err}");
    }

    /// Static output never scales a small source up: a 16×16 pixel-art emoji
    /// stays crisp rather than being resampled to 64×64 mush.
    #[test]
    fn a_small_source_is_not_scaled_up() {
        let encoded = encode_emoji(&noisy_png(16, 16)).unwrap();
        assert_eq!((encoded.width, encoded.height), (16, 16));
    }

    /// Aspect ratio survives the fit — a wide banner emoji must not be squashed
    /// into a square.
    #[test]
    fn aspect_ratio_is_preserved() {
        let encoded = encode_emoji(&noisy_png(512, 128)).unwrap();
        assert_eq!(encoded.width, EMOJI_TARGET_DIM);
        assert_eq!(encoded.height, EMOJI_TARGET_DIM / 4);
    }

    /// Subsampling folds the dropped frames' delays forward, so a halved frame
    /// count still plays at the original speed rather than at double.
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
