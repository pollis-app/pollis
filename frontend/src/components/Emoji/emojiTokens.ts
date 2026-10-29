/** `<:shortcode:content_hash>` wire grammar (#848); must agree with pollis-core `parse_emoji_tokens`. Malformed runs stay text; the alphabets can't carry markup. */

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

// Anchored inside the delimiters, so a near-miss stays text.
const TOKEN_RE = /<:([a-z0-9_]{2,32}):([0-9a-f]{64})>/g;

export function emojiTokenText(shortcode: string, contentHash: string): string {
  return `<:${shortcode}:${contentHash}>`;
}

/** Always at least one segment. */
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
  // Fresh regex: the module one is stateful (/g).
  return /<:[a-z0-9_]{2,32}:[0-9a-f]{64}>/.test(text);
}
