/** Composer model: one plain wire string; serialization emits only text and validated tokens, never markup. Imports nothing (node test runner), so TOKEN_RE copies `emojiTokens.ts`; rich-input-model.test.ts checks they agree. */

/** Same grammar as `emojiTokens.ts` and pollis-core `parse_emoji_tokens`. */
const TOKEN_RE = /<:([a-z0-9_]{2,32}):([0-9a-f]{64})>/g;

type ValueSegment =
  | { kind: "text"; text: string }
  | { kind: "emoji"; shortcode: string; contentHash: string };

/** Same partition as `splitEmojiSegments`, but an empty value yields no segments. */
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

/** Anchored: exactly one token and nothing else. */
export function isEmojiToken(text: string): boolean {
  return /^<:[a-z0-9_]{2,32}:[0-9a-f]{64}>$/.test(text);
}

export const EMOJI_TOKEN_ATTR = "data-emoji-token";

export const GHOST_ATTR = "data-ghost";

/** Mirrors the serialized value onto the host (a contentEditable has no `.value`). */
export const VALUE_ATTR = "data-value";

const ELEMENT_NODE = 1;
const TEXT_NODE = 3;

/** Tags an editing host generates for new lines. */
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
    // Validated, so a forged attribute cannot smuggle forbidden characters.
    if (token !== null && isEmojiToken(token)) {
      out.push(token);
      continue;
    }
    const tag = node.nodeName.toUpperCase();
    if (tag === "BR") {
      // Last child: the end-of-line filler, not a newline.
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

/** Empty text runs dropped: browsers place carets in empty text nodes inconsistently. */
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
  // `<:` + shortcode + `:` + hash + `>`
  return segment.shortcode.length + segment.contentHash.length + 4;
}

/** The token ending exactly at `caret`, so Backspace removes it deterministically. */
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

/** The token starting exactly at `caret`, or null. */
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

/** `text/plain` only, never `text/html`. CRLF folds to LF; C0 controls except tab/newline dropped. */
export function pastedPlainText(data: ClipboardTextSource): string {
  const raw = data.getData("text/plain") ?? "";
  return raw
    .replace(/\r\n?/g, "\n")
    .replace(/[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F]/g, "");
}
