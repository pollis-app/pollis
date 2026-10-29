/**
 * The `<:shortcode:content_hash>` wire grammar for custom emoji (#848); must
 * agree with `parse_emoji_tokens` in `pollis-core/src/commands/emoji.rs`.
 *
 * Anything not well-formed stays literal text. Neither alphabet can contain
 * `<`, `>`, `&` or `:`, so a token can't carry markup or nest.
 */

export interface EmojiTokenSegment {
  kind: "emoji";
  shortcode: string;
  contentHash: string;
}

export interface TextSegment {
  kind: "text";
  text: string;
}

export type EmojiSegment = EmojiTokenSegment | TextSegment;

const TOKEN_RE = /<:([a-z0-9_]{2,32}):([0-9a-f]{64})>/g;

export function emojiTokenText(shortcode: string, contentHash: string): string {
  return `<:${shortcode}:${contentHash}>`;
}

/** Split `text` into alternating text and emoji segments; one text segment when there are none. */
export function splitEmojiSegments(text: string): EmojiSegment[] {
  const segments: EmojiSegment[] = [];
  let cursor = 0;

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

export function hasEmojiTokens(text: string): boolean {
  // A fresh regex: the module-level one is stateful (/g).
  return /<:[a-z0-9_]{2,32}:[0-9a-f]{64}>/.test(text);
}
