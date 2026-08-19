/*
 * Shortcode + source-file rules for the custom-emoji upload flow.
 *
 * Pure, dependency-free helpers so the upload page's naming behaviour is
 * testable without a browser (`frontend/tests/emoji-shortcode.test.ts`).
 *
 * Every bound here MIRRORS pollis-core (`commands/emoji.rs`) and the DS, which
 * are authoritative. Client-side they exist so the user is told what is wrong
 * before a round trip — they may only ever be as strict, never laxer.
 */

/** Mirrors `shortcode_is_valid` in pollis-core and the DS. */
export const SHORTCODE_RE = /^[a-z0-9_]{2,32}$/;

/** Longest shortcode `SHORTCODE_RE` accepts — the truncation point below. */
const SHORTCODE_MAX = 32;

/**
 * Extensions a drop accepts, and the MIME type the local preview blob is given.
 *
 * The Rust side never trusts an extension — it sniffs magic bytes — so this is
 * purely so an obviously-wrong drop (a PDF, a folder) is refused on the spot
 * instead of after an upload attempt, and so the pre-upload preview renders
 * (including a GIF's animation, which the re-encoder also preserves).
 */
const EMOJI_MIME_BY_EXTENSION: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
};

/** The accepted extensions, in the order the file picker offers them. */
export const EMOJI_IMAGE_EXTENSIONS = Object.keys(EMOJI_MIME_BY_EXTENSION);

/**
 * Largest source file the upload will accept. Must agree with
 * `EMOJI_MAX_INPUT_BYTES` in pollis-core, which rejects before parsing.
 */
export const EMOJI_MAX_INPUT_BYTES = 8 * 1024 * 1024;

/** The trailing path segment of a native path, on either path separator. */
export function fileName(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] ?? path;
}

/** The extension of `path`, lowercased, or "" when it has none. */
function extension(path: string): string {
  const name = fileName(path).toLowerCase();
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(dot + 1) : "";
}

/** True when `path` ends in one of the accepted image extensions. */
export function isSupportedEmojiFile(path: string): boolean {
  return extension(path) in EMOJI_MIME_BY_EXTENSION;
}

/**
 * MIME type for the local preview blob. Only ever called for a path
 * `isSupportedEmojiFile` accepted; the fallback keeps the return total.
 */
export function emojiMimeType(path: string): string {
  return EMOJI_MIME_BY_EXTENSION[extension(path)] ?? "image/png";
}

/**
 * Coerce arbitrary text into the shortcode alphabet: lowercase, `[a-z0-9_]`,
 * illegal runs collapsed to a single `_`, no leading or trailing `_`, capped at
 * 32 characters.
 *
 * Returns "" when nothing survives — the caller then leaves the field empty and
 * the inline validation asks for a name, which is a better outcome than
 * inventing one.
 */
export function sanitizeShortcode(input: string): string {
  const collapsed = input
    .toLowerCase()
    .replace(/[^a-z0-9_]+/g, "_")
    .replace(/_+/g, "_")
    .replace(/^_+|_+$/g, "");
  return collapsed.slice(0, SHORTCODE_MAX).replace(/_+$/g, "");
}

/**
 * The shortcode a dropped or picked file is named after — Discord's model,
 * where the image comes first and the name is derived from it.
 *
 * Directory and extension are stripped before sanitising, so
 * `~/Pictures/Party Parrot (1).GIF` becomes `party_parrot_1`.
 */
export function shortcodeFromFileName(path: string): string {
  const name = fileName(path);
  const dot = name.lastIndexOf(".");
  const stem = dot > 0 ? name.slice(0, dot) : name;
  return sanitizeShortcode(stem);
}
