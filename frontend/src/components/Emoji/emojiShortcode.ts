/* Custom-emoji upload rules. Mirror pollis-core (`commands/emoji.rs`) and the DS: never laxer. */

/** Mirrors `shortcode_is_valid` in pollis-core and the DS. */
export const SHORTCODE_RE = /^[a-z0-9_]{2,32}$/;

const SHORTCODE_MAX = 32;

/** Rust sniffs magic bytes; this only refuses obvious misses early and types the preview blob. */
const EMOJI_MIME_BY_EXTENSION: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
};

export const EMOJI_IMAGE_EXTENSIONS = Object.keys(EMOJI_MIME_BY_EXTENSION);

/** Must agree with `EMOJI_MAX_INPUT_BYTES` in pollis-core. */
export const EMOJI_MAX_INPUT_BYTES = 8 * 1024 * 1024;

export function fileName(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] ?? path;
}

function extension(path: string): string {
  const name = fileName(path).toLowerCase();
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(dot + 1) : "";
}

export function isSupportedEmojiFile(path: string): boolean {
  return extension(path) in EMOJI_MIME_BY_EXTENSION;
}

export function emojiMimeType(path: string): string {
  return EMOJI_MIME_BY_EXTENSION[extension(path)] ?? "image/png";
}

/** Coerce into `[a-z0-9_]`, runs collapsed to `_`, trimmed, max 32; "" when nothing survives. */
export function sanitizeShortcode(input: string): string {
  const collapsed = input
    .toLowerCase()
    .replace(/[^a-z0-9_]+/g, "_")
    .replace(/_+/g, "_")
    .replace(/^_+|_+$/g, "");
  return collapsed.slice(0, SHORTCODE_MAX).replace(/_+$/g, "");
}

/** Shortcode from a file name: `Party Parrot (1).GIF` → `party_parrot_1`. */
export function shortcodeFromFileName(path: string): string {
  const name = fileName(path);
  const dot = name.lastIndexOf(".");
  const stem = dot > 0 ? name.slice(0, dot) : name;
  return sanitizeShortcode(stem);
}
