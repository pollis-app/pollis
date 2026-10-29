/**
 * The composer's model: one plain `string` in the `<:shortcode:content_hash>`
 * wire grammar, with the `contentEditable` DOM as a projection of it, read back
 * after every edit.
 *
 * Serialization can only emit text and well-formed tokens: text nodes give
 * their characters; a `data-emoji-token` element gives that attribute, only if
 * it parses; `data-ghost` gives nothing; a `<br>` gives a newline unless it is
 * its parent's last child; anything else gives its descendants'. No branch
 * emits markup, and `pastedPlainText` never reads `text/html`.
 *
 * Imports nothing (Node's test runner can't resolve extensionless imports), so
 * the token pattern is a copy of `emojiTokens.ts`'s; `rich-input-model.test.ts`
 * fails if the two splitters disagree.
 */

/** Same grammar as `emojiTokens.ts` and `parse_emoji_tokens` in pollis-core. */
const TOKEN_RE = /<:([a-z0-9_]{2,32}):([0-9a-f]{64})>/g;

type ValueSegment =
  | { kind: "text"; text: string }
  | { kind: "emoji"; shortcode: string; contentHash: string };

/**
 * The same partition as `splitEmojiSegments`, but an empty value yields no
 * segments (it must project to no text node).
 */
function splitValue(text: string): ValueSegment[] {
  const segments: ValueSegment[] = [];
  let cursor = 0;
  for (const match of text.matchAll(TOKEN_RE)) {
    const start = match.index ?? 0;
    if (start > cursor) {
      segments.push({ kind: "text", text: text.slice(cursor, start) });
    }
    segments.push({ kind: "emoji", shortcode: match[1], contentHash: match[2] });
    cursor = start + match[0].length;
  }
  if (cursor < text.length) {
    segments.push({ kind: "text", text: text.slice(cursor) });
  }
  return segments;
}

/** EXACTLY one token and nothing else; the serializer's check before trusting `data-emoji-token`. */
export function isEmojiToken(text: string): boolean {
  return /^<:[a-z0-9_]{2,32}:[0-9a-f]{64}>$/.test(text);
}

/** Carries a custom emoji node's exact wire token. */
export const EMOJI_TOKEN_ATTR = "data-emoji-token";

/** Marks the terminal skin's inline autosuggest, never part of the value. */
export const GHOST_ATTR = "data-ghost";

/** The serialized value mirrored onto the host (a `contentEditable` has no `.value`); specs assert on it. */
export const VALUE_ATTR = "data-value";

const ELEMENT_NODE = 1;
const TEXT_NODE = 3;

/** Tags an editing host generates for a new line (not every HTML block). */
const BLOCK_TAGS = new Set([
  "DIV",
  "P",
  "LI",
  "TR",
  "BLOCKQUOTE",
  "PRE",
  "H1",
  "H2",
  "H3",
  "H4",
  "H5",
  "H6",
]);

export interface SerializableNode {
  nodeType: number;
  nodeName: string;
  textContent: string | null;
  childNodes: ArrayLike<SerializableNode>;
  getAttribute?: (name: string) => string | null;
}

function attributeOf(node: SerializableNode, name: string): string | null {
  return node.getAttribute ? node.getAttribute(name) : null;
}

function walk(parent: SerializableNode, out: string[]): void {
  const children = parent.childNodes;
  for (let index = 0; index < children.length; index += 1) {
    const node = children[index];
    if (node.nodeType === TEXT_NODE) {
      out.push(node.textContent ?? "");
      continue;
    }
    if (node.nodeType !== ELEMENT_NODE) {
      continue;
    }
    if (attributeOf(node, GHOST_ATTR) !== null) {
      continue;
    }
    const token = attributeOf(node, EMOJI_TOKEN_ATTR);
    // Validated, not trusted: a forged attribute can't smuggle forbidden characters.
    if (token !== null && isEmojiToken(token)) {
      out.push(token);
      continue;
    }
    const tag = node.nodeName.toUpperCase();
    if (tag === "BR") {
      // A trailing <br> is the editing host's end-of-line filler, not a newline.
      if (index === children.length - 1) {
        continue;
      }
      out.push("\n");
      continue;
    }
    if (BLOCK_TAGS.has(tag) && out.length > 0 && !out[out.length - 1].endsWith("\n")) {
      out.push("\n");
    }
    walk(node, out);
  }
}

export function serializeRichInput(root: SerializableNode): string {
  const out: string[] = [];
  walk(root, out);
  return out.join("");
}

export type RichRun =
  | { kind: "text"; text: string }
  | { kind: "emoji"; token: string; shortcode: string; contentHash: string };

/** Split wire text into render runs, dropping empty text runs (browsers misplace carets in them). */
export function richRuns(value: string): RichRun[] {
  const runs: RichRun[] = [];
  for (const segment of splitValue(value)) {
    if (segment.kind === "text") {
      if (segment.text.length > 0) {
        runs.push({ kind: "text", text: segment.text });
      }
      continue;
    }
    runs.push({
      kind: "emoji",
      token: `<:${segment.shortcode}:${segment.contentHash}>`,
      shortcode: segment.shortcode,
      contentHash: segment.contentHash,
    });
  }
  return runs;
}

export interface OffsetRange {
  start: number;
  end: number;
}

function segmentLength(segment: ValueSegment): number {
  if (segment.kind === "text") {
    return segment.text.length;
  }
  return segment.shortcode.length + segment.contentHash.length + 4;
}

/**
 * The emoji token ending exactly at `caret`, or null. Lets Backspace remove a
 * whole token deterministically instead of per-engine `contenteditable=false`
 * behaviour.
 */
export function emojiTokenBefore(value: string, caret: number): OffsetRange | null {
  let cursor = 0;
  for (const segment of splitValue(value)) {
    const length = segmentLength(segment);
    if (segment.kind === "emoji" && cursor + length === caret) {
      return { start: cursor, end: caret };
    }
    cursor += length;
  }
  return null;
}

/** The emoji token starting exactly at `caret`, or null (forward delete). */
export function emojiTokenAfter(value: string, caret: number): OffsetRange | null {
  let cursor = 0;
  for (const segment of splitValue(value)) {
    const length = segmentLength(segment);
    if (segment.kind === "emoji" && cursor === caret) {
      return { start: caret, end: caret + length };
    }
    cursor += length;
  }
  return null;
}

export interface ClipboardTextSource {
  getData: (type: string) => string;
}

/**
 * What a paste contributes: `text/plain` only, never `text/html`. CRLF folds to
 * LF and other C0 controls except tab are dropped. A pasted raw token still
 * becomes an emoji node via re-projection.
 */
export function pastedPlainText(data: ClipboardTextSource): string {
  const raw = data.getData("text/plain") ?? "";
  return raw
    .replace(/\r\n?/g, "\n")
    .replace(/[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F]/g, "");
}
