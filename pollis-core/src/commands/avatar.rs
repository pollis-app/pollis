//! Profile pictures for the mobile app, which has no loopback media server.
//!
//! Avatars are public, content-addressed objects (`avatars/<user_id>/<sha256>.<ext>`,
//! #874): desktop resolves one through `r2::get_public_file_url`. Mobile instead
//! gets small square renditions written once to a sandbox directory and reused on
//! every later render, so a revisit costs no network and no full-size decode.
//!
//! - The original is fetched through the same device-signed presign as desktop and
//!   hash-checked before it is decoded, so substituted bytes are never rendered.
//! - Every rendition size is cut from one decode, so a list row and a profile header
//!   never download the same avatar twice.
//! - A file is named `<user_id>_<sha256>_<px>.<ext>`, which lets the app map a user to
//!   the picture it last drew with one directory listing, before any profile read.
//! - Uploads are re-encoded here, which drops EXIF (location, camera) and applies the
//!   photo's orientation.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::commands::r2;
use crate::error::{Error, Result};
use crate::state::AppState;

/// The square sizes written to disk, in pixels. 128 covers a 40pt row at 3x.
pub const AVATAR_RENDITIONS: [u32; 3] = [128, 256, 512];

/// Longest side of an uploaded avatar, as desktop's `resizeImage`.
pub const AVATAR_UPLOAD_DIM: u32 = 1024;

/// Largest source file an upload will read.
const AVATAR_MAX_SOURCE_BYTES: u64 = 32 * 1024 * 1024;

/// Largest source dimension the decoder accepts.
const AVATAR_MAX_SOURCE_DIM: u32 = 8192;

/// Decode allocation cap; catches a header that lies about its size.
const AVATAR_MAX_DECODE_ALLOC: u64 = 512 * 1024 * 1024;

/// JPEG quality for uploads and renditions.
const JPEG_QUALITY: u8 = 85;

/// The parts of a content-addressed avatar key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvatarKey {
    pub owner: String,
    pub hash: String,
    pub ext: String,
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// A user id safe to put in a file name: the DS key charset minus `.` and `~`.
pub fn is_valid_file_user_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// Parse `avatars/<user_id>/<hex64>.<ext>`. `None` for anything else, including the
/// legacy mutable `avatars/<user_id>` key and an empty value (no avatar).
pub fn parse_avatar_key(key: &str) -> Option<AvatarKey> {
    let mut parts = key.split('/');
    let (Some("avatars"), Some(owner), Some(name), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    if !is_valid_file_user_id(owner) {
        return None;
    }
    let (hash, ext) = name.rsplit_once('.')?;
    if !is_hex64(hash) || !matches!(ext, "png" | "jpg" | "jpeg" | "gif" | "webp") {
        return None;
    }
    Some(AvatarKey {
        owner: owner.to_string(),
        hash: hash.to_string(),
        ext: ext.to_string(),
    })
}

/// The smallest rendition at least `px` wide; the largest when none is.
pub fn rendition_for(px: u32) -> u32 {
    AVATAR_RENDITIONS
        .iter()
        .copied()
        .find(|&r| r >= px)
        .unwrap_or(AVATAR_RENDITIONS[AVATAR_RENDITIONS.len() - 1])
}

/// File name of one rendition. `ext` is `jpg`, or `png` when the image has transparency.
pub fn rendition_file_name(user_id: &str, hash: &str, px: u32, ext: &str) -> String {
    format!("{user_id}_{hash}_{px}.{ext}")
}

fn decode_err(e: image::ImageError) -> Error {
    // The decoder's message can quote attacker bytes, so it is not surfaced.
    let _ = e;
    Error::Other(anyhow::anyhow!("could not decode that image"))
}

/// Decode untrusted bytes under limits and turn them upright.
fn decode_upright(input: &[u8]) -> Result<image::DynamicImage> {
    use image::{ImageDecoder as _, ImageFormat};

    let format = image::guess_format(input)
        .map_err(|_| Error::Other(anyhow::anyhow!("unrecognised image format")))?;
    if !matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::WebP
    ) {
        return Err(Error::Other(anyhow::anyhow!(
            "a profile picture must be a PNG, JPEG, GIF or WebP"
        )));
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(AVATAR_MAX_SOURCE_DIM);
    limits.max_image_height = Some(AVATAR_MAX_SOURCE_DIM);
    limits.max_alloc = Some(AVATAR_MAX_DECODE_ALLOC);

    let mut reader = image::ImageReader::new(Cursor::new(input));
    reader.set_format(format);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(decode_err)?;
    let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(decoder).map_err(decode_err)?;
    img.apply_orientation(orientation);
    if img.width() == 0 || img.height() == 0 {
        return Err(Error::Other(anyhow::anyhow!("image has a zero dimension")));
    }
    Ok(img)
}

/// The centred square of `img`.
fn square(img: &image::DynamicImage) -> image::DynamicImage {
    let side = img.width().min(img.height());
    let x = (img.width() - side) / 2;
    let y = (img.height() - side) / 2;
    img.crop_imm(x, y, side, side)
}

fn has_transparency(img: &image::DynamicImage) -> bool {
    if !img.color().has_alpha() {
        return false;
    }
    img.to_rgba8().pixels().any(|p| p.0[3] != 255)
}

/// Encode as JPEG, or PNG when the image has transparency. Returns `(bytes, ext)`.
/// Re-encoding writes no metadata, so EXIF never leaves the device.
fn encode(img: &image::DynamicImage) -> Result<(Vec<u8>, &'static str)> {
    let mut out = Vec::new();
    if has_transparency(img) {
        img.to_rgba8()
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .map_err(|e| Error::Other(anyhow::anyhow!("encode png: {e}")))?;
        return Ok((out, "png"));
    }
    let rgb = img.to_rgb8();
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY);
    enc.encode_image(&rgb)
        .map_err(|e| Error::Other(anyhow::anyhow!("encode jpeg: {e}")))?;
    Ok((out, "jpg"))
}

fn resize_square(img: &image::DynamicImage, px: u32) -> image::DynamicImage {
    if img.width() <= px {
        return img.clone();
    }
    img.resize_exact(px, px, image::imageops::FilterType::CatmullRom)
}

/// Every rendition of an avatar, from one decode: `(px, bytes, ext)`.
pub fn render_renditions(input: &[u8]) -> Result<Vec<(u32, Vec<u8>, &'static str)>> {
    let sq = square(&decode_upright(input)?);
    let mut out = Vec::with_capacity(AVATAR_RENDITIONS.len());
    for px in AVATAR_RENDITIONS {
        let (bytes, ext) = encode(&resize_square(&sq, px))?;
        out.push((px, bytes, ext));
    }
    Ok(out)
}

/// An upload: the centred square, at most [`AVATAR_UPLOAD_DIM`], re-encoded.
/// Returns `(bytes, content_type)`.
pub fn prepare_upload(input: &[u8]) -> Result<(Vec<u8>, &'static str)> {
    if input.is_empty() {
        return Err(Error::Other(anyhow::anyhow!("image file is empty")));
    }
    if input.len() as u64 > AVATAR_MAX_SOURCE_BYTES {
        return Err(Error::Other(anyhow::anyhow!("that image is too large")));
    }
    let sq = resize_square(&square(&decode_upright(input)?), AVATAR_UPLOAD_DIM);
    let (bytes, ext) = encode(&sq)?;
    let content_type = if ext == "png" { "image/png" } else { "image/jpeg" };
    if bytes.len() as u64 > pollis_api::broker::R2_PUBLIC_IMAGE_MAX_BYTES {
        return Err(Error::Other(anyhow::anyhow!("that image is too large")));
    }
    Ok((bytes, content_type))
}

/// Files in `dir` for `user_id`: `(file name, hash, px)`.
fn files_for_user(dir: &Path, user_id: &str) -> Vec<(String, String, u32)> {
    let prefix = format!("{user_id}_");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|name| {
            let rest = name.strip_prefix(&prefix)?;
            let (stem, ext) = rest.rsplit_once('.')?;
            if !matches!(ext, "jpg" | "png") {
                return None;
            }
            let (hash, px) = stem.split_once('_')?;
            if !is_hex64(hash) {
                return None;
            }
            let px: u32 = px.parse().ok()?;
            Some((name.clone(), hash.to_string(), px))
        })
        .collect()
}

/// The written rendition of `hash` at `px`, if it is on disk.
/// Two `stat`s, no directory listing: this is the hot path of every render.
fn existing_rendition(dir: &Path, user_id: &str, hash: &str, px: u32) -> Option<PathBuf> {
    ["jpg", "png"]
        .into_iter()
        .map(|ext| dir.join(rendition_file_name(user_id, hash, px, ext)))
        .find(|p| p.is_file())
}

/// Write every rendition atomically, then drop the user's older pictures.
/// Returns the path of the one at `want_px`.
async fn write_renditions(
    dir: &Path,
    user_id: &str,
    hash: &str,
    renditions: Vec<(u32, Vec<u8>, &'static str)>,
    want_px: u32,
) -> Result<PathBuf> {
    crate::private_fs::create_dir_all(dir)
        .map_err(|e| Error::Other(anyhow::anyhow!("create avatar dir: {e}")))?;
    let mut wanted = None;
    for (px, bytes, ext) in renditions {
        let path = dir.join(rendition_file_name(user_id, hash, px, ext));
        // A half-written file would be drawn as a broken image.
        let tmp = dir.join(format!(".{}.tmp", rendition_file_name(user_id, hash, px, ext)));
        crate::private_fs::write_async(&tmp, bytes)
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!("write avatar: {e}")))?;
        if let Err(e) = tokio::fs::rename(&tmp, &path).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(Error::Other(anyhow::anyhow!("rename avatar: {e}")));
        }
        if px == want_px {
            wanted = Some(path);
        }
    }
    for (name, old_hash, _) in files_for_user(dir, user_id) {
        if old_hash != hash {
            let _ = std::fs::remove_file(dir.join(name));
        }
    }
    wanted.ok_or_else(|| Error::Other(anyhow::anyhow!("no avatar rendition at {want_px}px")))
}

fn to_uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

fn strip_file_scheme(dir: &str) -> &Path {
    Path::new(dir.strip_prefix("file://").unwrap_or(dir))
}

/// A local `file://` URI of `user_id`'s avatar `key`, about `px` wide.
///
/// On disk already → returned with no network. Otherwise the original is fetched,
/// hash-checked, cut into every rendition and written under `dest_dir`.
pub async fn get_avatar_path(
    user_id: String,
    key: String,
    px: u32,
    dest_dir: String,
    state: &Arc<AppState>,
) -> Result<String> {
    if !is_valid_file_user_id(&user_id) {
        return Err(Error::Other(anyhow::anyhow!("not a user id")));
    }
    let Some(parsed) = parse_avatar_key(&key) else {
        return Err(Error::Other(anyhow::anyhow!("not a profile picture key")));
    };
    let dir = strip_file_scheme(&dest_dir).to_path_buf();
    let want = rendition_for(px);
    if let Some(path) = existing_rendition(&dir, &user_id, &parsed.hash, want) {
        return Ok(to_uri(&path));
    }

    let get_url = r2::presign_r2_with_length(state, "get", &key, None).await?;
    let overlay = state.overlay_handle();
    let Some(bytes) = r2::r2_get_url(
        overlay.as_deref(),
        &get_url,
        pollis_api::broker::R2_PUBLIC_IMAGE_MAX_BYTES,
    )
    .await?
    else {
        return Err(Error::Other(anyhow::anyhow!("profile picture is over the size limit")));
    };
    // Public objects are unencrypted, so the bytes must match their address.
    let actual = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&bytes));
    if actual != parsed.hash {
        return Err(Error::Other(anyhow::anyhow!(
            "profile picture failed its content-hash check; refusing to draw it"
        )));
    }
    let renditions = tokio::task::spawn_blocking(move || render_renditions(&bytes))
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("render avatar: {e}")))??;
    let path = write_renditions(&dir, &user_id, &parsed.hash, renditions, want).await?;
    Ok(to_uri(&path))
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AvatarUploadResult {
    /// The new `avatar_url` value.
    pub key: String,
    /// A local rendition of the new picture at the requested size, when `dest_dir` was given.
    pub uri: Option<String>,
}

/// Set the signed-in user's profile picture from a local image file.
///
/// The image is cropped to its centred square, scaled to at most
/// [`AVATAR_UPLOAD_DIM`] and re-encoded, uploaded under the same content-addressed
/// key desktop uses, then saved on the profile. Its renditions are written under
/// `dest_dir` too, so the new picture shows without downloading it back.
pub async fn upload_avatar(
    user_id: String,
    path: String,
    px: u32,
    dest_dir: Option<String>,
    state: &Arc<AppState>,
) -> Result<AvatarUploadResult> {
    if !is_valid_file_user_id(&user_id) {
        return Err(Error::Other(anyhow::anyhow!("not a user id")));
    }
    let source = strip_file_scheme(&path).to_path_buf();
    let meta = tokio::fs::metadata(&source)
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("read image: {e}")))?;
    if meta.len() > AVATAR_MAX_SOURCE_BYTES {
        return Err(Error::Other(anyhow::anyhow!("that image is too large")));
    }
    let input = tokio::fs::read(&source)
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("read image: {e}")))?;
    let (bytes, content_type) = tokio::task::spawn_blocking(move || prepare_upload(&input))
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("prepare avatar: {e}")))??;

    let for_renditions = bytes.clone();
    let uploaded = r2::upload_public_file(
        format!("avatars/{user_id}"),
        bytes,
        content_type.to_string(),
        state,
    )
    .await?;
    crate::commands::user::update_user_profile(
        user_id.clone(),
        None,
        None,
        None,
        Some(uploaded.key.clone()),
        state,
    )
    .await?;

    let mut uri = None;
    if let (Some(dest_dir), Some(parsed)) = (dest_dir, parse_avatar_key(&uploaded.key)) {
        let dir = strip_file_scheme(&dest_dir).to_path_buf();
        let rendered = tokio::task::spawn_blocking(move || render_renditions(&for_renditions))
            .await
            .map_err(|e| Error::Other(anyhow::anyhow!("render avatar: {e}")))?;
        // Saved already; a local write failure only means the next render downloads it.
        match rendered {
            Ok(r) => match write_renditions(&dir, &user_id, &parsed.hash, r, rendition_for(px)).await
            {
                Ok(p) => uri = Some(to_uri(&p)),
                Err(e) => crate::diag!("[avatar] local copy not written: {e}"),
            },
            Err(e) => crate::diag!("[avatar] local copy not rendered: {e}"),
        }
    }
    Ok(AvatarUploadResult {
        key: uploaded.key,
        uri,
    })
}

/// Clear the signed-in user's profile picture. An empty `avatar_url` reads as none
/// on every client (the DS update is a COALESCE, so `None` would leave it set).
pub async fn remove_avatar(user_id: String, state: &Arc<AppState>) -> Result<()> {
    crate::commands::user::update_user_profile(user_id, None, None, None, Some(String::new()), state)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn parses_content_addressed_keys_only() {
        let k = parse_avatar_key(&format!("avatars/u1/{H}.jpg")).unwrap();
        assert_eq!(k.owner, "u1");
        assert_eq!(k.hash, H);
        assert_eq!(k.ext, "jpg");
        assert!(parse_avatar_key("").is_none());
        assert!(parse_avatar_key("avatars/u1").is_none(), "legacy mutable key");
        assert!(parse_avatar_key(&format!("media/{H}.enc")).is_none());
        assert!(parse_avatar_key(&format!("avatars/../{H}.jpg")).is_none());
        assert!(parse_avatar_key(&format!("avatars/u1/{H}.svg")).is_none());
        assert!(parse_avatar_key(&format!("avatars/u1/{}.jpg", H.to_uppercase())).is_none());
        assert!(parse_avatar_key(&format!("avatars/u1/x/{H}.jpg")).is_none());
        assert!(parse_avatar_key(&format!("avatars/u_1/{H}.jpg")).is_none(), "`_` splits file names");
    }

    #[test]
    fn picks_the_smallest_rendition_that_covers_the_view() {
        assert_eq!(rendition_for(1), 128);
        assert_eq!(rendition_for(120), 128);
        assert_eq!(rendition_for(129), 256);
        assert_eq!(rendition_for(288), 512);
        assert_eq!(rendition_for(4000), 512);
    }

    fn jpeg(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, _| image::Rgb([(x % 256) as u8, 40, 90]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Jpeg)
            .unwrap();
        out
    }

    #[test]
    fn renditions_are_square_and_sized() {
        let r = render_renditions(&jpeg(900, 600)).unwrap();
        assert_eq!(r.iter().map(|(px, _, _)| *px).collect::<Vec<_>>(), AVATAR_RENDITIONS);
        for (px, bytes, ext) in r {
            assert_eq!(ext, "jpg");
            let img = image::load_from_memory(&bytes).unwrap();
            assert_eq!((img.width(), img.height()), (px, px));
        }
    }

    #[test]
    fn a_small_source_is_never_scaled_up() {
        let r = render_renditions(&jpeg(100, 300)).unwrap();
        for (_, bytes, _) in r {
            let img = image::load_from_memory(&bytes).unwrap();
            assert_eq!((img.width(), img.height()), (100, 100));
        }
    }

    #[test]
    fn upload_is_a_bounded_square_jpeg() {
        let (bytes, ct) = prepare_upload(&jpeg(3000, 2000)).unwrap();
        assert_eq!(ct, "image/jpeg");
        let img = image::load_from_memory(&bytes).unwrap();
        assert_eq!((img.width(), img.height()), (AVATAR_UPLOAD_DIM, AVATAR_UPLOAD_DIM));
    }

    #[test]
    fn transparency_survives_as_png() {
        let img = image::RgbaImage::from_fn(64, 64, |x, _| image::Rgba([10, 20, 30, if x < 32 { 0 } else { 255 }]));
        let mut src = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut src), image::ImageFormat::Png)
            .unwrap();
        let (_, ct) = prepare_upload(&src).unwrap();
        assert_eq!(ct, "image/png");
    }

    #[test]
    fn upload_drops_exif() {
        // A JPEG with an APP1 Exif segment spliced in after SOI.
        let plain = jpeg(40, 40);
        let mut with_exif = vec![0xFF, 0xD8];
        let payload = b"Exif\0\0GPS-SECRET";
        with_exif.extend_from_slice(&[0xFF, 0xE1, 0, (payload.len() + 2) as u8]);
        with_exif.extend_from_slice(payload);
        with_exif.extend_from_slice(&plain[2..]);
        let (bytes, _) = prepare_upload(&with_exif).unwrap();
        assert!(!bytes.windows(10).any(|w| w == b"GPS-SECRET"));
        assert!(!bytes.windows(4).any(|w| w == b"Exif"));
    }

    #[test]
    fn garbage_is_refused() {
        assert!(prepare_upload(b"not an image").is_err());
        assert!(prepare_upload(&[]).is_err());
    }

    #[tokio::test]
    async fn written_renditions_replace_the_users_older_picture() {
        let dir = tempfile::tempdir().unwrap();
        let old = "f".repeat(64);
        let r = render_renditions(&jpeg(300, 300)).unwrap();
        write_renditions(dir.path(), "u1", &old, r.clone(), 128).await.unwrap();
        write_renditions(dir.path(), "u2", &old, r.clone(), 128).await.unwrap();
        let p = write_renditions(dir.path(), "u1", H, r, 256).await.unwrap();
        assert!(p.ends_with(format!("u1_{H}_256.jpg")));
        let u1 = files_for_user(dir.path(), "u1");
        assert_eq!(u1.len(), AVATAR_RENDITIONS.len());
        assert!(u1.iter().all(|(_, h, _)| h == H));
        assert_eq!(files_for_user(dir.path(), "u2").len(), AVATAR_RENDITIONS.len(), "others untouched");
        assert!(existing_rendition(dir.path(), "u1", H, 512).is_some());
        assert!(existing_rendition(dir.path(), "u1", &old, 128).is_none());
    }
}
