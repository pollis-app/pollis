/**
 * The `<:shortcode:content_hash>` wire grammar for custom emoji (#848).
 *
 * The authoritative implementation is `parse_emoji_tokens` in
 * `pollis-core/src/commands/emoji.rs`; this is the renderer's copy and must
 * agree with it. Kept tiny and dependency-free precisely so it can be read
 * against the Rust side line by line.
 *
 * Both halves must be well-formed for a run to count as a token. Anything else
 * is ordinary text and is left untouched — a message containing `<:oops:>` has
 * to render as those literal characters rather than vanish.
 *
 * The alphabets are what make this safe to render: neither a shortcode
 * (`[a-z0-9_]{2,32}`) nor a hash (64 lowercase hex) can contain `<`, `>`, `&`
 * or `:`, so a token can never carry markup and can never be nested inside
 * another. React escapes text anyway; the grammar means it does not have to.
 */

/** A parsed custom-emoji reference. */
export interface EmojiTokenSegment {
  kind: "emoji";
  shortcode: string;
  contentHash: string;
}

/** A run of ordinary text between (or around) tokens. */
export interface TextSegment {
  kind: "text";
  text: string;
}

export type EmojiSegment = EmojiTokenSegment | TextSegment;

// `[a-z0-9_]{2,32}` then 64 lowercase hex. Anchored inside the delimiters, so a
// near-miss simply fails to match and stays text.
const TOKEN_RE = /<:([a-z0-9_]{2,32}):([0-9a-f]{64})>/g;

/** Build the wire form of a custom-emoji reference. */
export function emojiTokenText(shortcode: string, contentHash: string): string {
  return `<:${shortcode}:${contentHash}>`;
}

/**
 * Split `text` into alternating text and custom-emoji segments.
 *
 * Returns a single text segment when there are no tokens, so callers can render
 * the common case without a special path.
 */
export function splitEmojiSegments(text: string): EmojiSegment[] {
  const segments: EmojiSegment[] = [];
  let cursor = 0;

  // `matchAll` needs the /g flag and gives us the index of each match, which is
  // what lets the untouched text between tokens survive verbatim.
  for (const match of text.matchAll(TOKEN_RE)) {
    const start = match.index ?? 0;
    if (start > cursor) {
      segments.push({ kind: "text", text: text.slice(cursor, start) });
    }
    segments.push({
      kind: "emoji",
      shortcode: match[1],
      contentHash: match[2],
    });
    cursor = start + match[0].length;
  }

  if (cursor < text.length) {
    segments.push({ kind: "text", text: text.slice(cursor) });
  }
  if (segments.length === 0) {
    segments.push({ kind: "text", text });
  }
  return segments;
}

/**
 * True when `text` is EXACTLY one custom-emoji reference and nothing else.
 *
 * `hasEmojiTokens` asks whether a message contains one; this asks whether a
 * string *is* one, which is what the rich composer needs before it will trust
 * an element's `data-emoji-token` attribute and copy it into the wire text. An
 * anchored match, so neither leading nor trailing characters can ride along.
 */
export function isEmojiToken(text: string): boolean {
  return /^<:[a-z0-9_]{2,32}:[0-9a-f]{64}>$/.test(text);
}

/** True when `text` carries at least one custom-emoji reference. */
export function hasEmojiTokens(text: string): boolean {
  // A fresh regex: the module-level one is stateful because of /g.
  return /<:[a-z0-9_]{2,32}:[0-9a-f]{64}>/.test(text);
}
