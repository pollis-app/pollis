/**
 * The composer's model, and the two conversions that let a `contentEditable`
 * be a faithful stand-in for a `<textarea>` (#848 follow-up).
 *
 * The source of truth stays exactly what it was before the rich composer: one
 * plain `string` in the `<:shortcode:content_hash>` wire grammar, indexed by
 * integer offsets. Nothing about mentions, `:shortcode:` completion, drafts or
 * sending had to learn a document model, because there is no document model —
 * the DOM is a *projection* of the string and is read back into one after every
 * edit.
 *
 * That makes serialization the contract, and it is deliberately built so the
 * only two things that can come out of it are text and well-formed tokens:
 *
 *   - a text node contributes its characters, verbatim;
 *   - an element carrying `data-emoji-token` contributes THAT ATTRIBUTE, and
 *     only when it parses as a token — never its rendered contents;
 *   - the inline autosuggest (`data-ghost`) contributes nothing;
 *   - a `<br>` contributes a newline unless it is the last child of its parent,
 *     where it is the filler an editing host keeps at the end of a line;
 *   - anything else contributes only what its descendants contribute.
 *
 * There is no branch that emits an element's markup, so a paste of arbitrary
 * rich text cannot put a `<`, a `>` or an `&` into the wire text that was not
 * typed as a literal character. `pastedPlainText` closes the same door from the
 * other side by never reading `text/html` at all.
 *
 * Everything here is pure and DOM-free — `SerializableNode` is the handful of
 * `Node` members the walker touches, which real DOM nodes satisfy structurally
 * and plain objects can satisfy in a test. `frontend/tests/` has no browser,
 * and — the reason this module imports nothing at all — Node's test runner
 * cannot resolve an extensionless relative import, so every module the unit
 * tier covers has to stand alone. That is why the token pattern below is a
 * second copy of `emojiTokens.ts`'s rather than an import of it. The copy is
 * not left to review: `rich-input-model.test.ts` runs this splitter and
 * `splitEmojiSegments` over one corpus and fails if they ever disagree about
 * what a token is.
 */

/**
 * `[a-z0-9_]{2,32}` then 64 lowercase hex, same as `emojiTokens.ts` and same as
 * `parse_emoji_tokens` in `pollis-core/src/commands/emoji.rs`.
 */
const TOKEN_RE = /<:([a-z0-9_]{2,32}):([0-9a-f]{64})>/g;

/** One parsed run of the value: ordinary text, or a custom-emoji reference. */
type ValueSegment =
  | { kind: "text"; text: string }
  | { kind: "emoji"; shortcode: string; contentHash: string };

/**
 * Split wire text into alternating text and token segments.
 *
 * Byte-for-byte the same partition `splitEmojiSegments` produces, minus its
 * "always return at least one segment" convenience, which the projection does
 * not want — an empty value must project to no text node at all.
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

/**
 * True when `text` is EXACTLY one custom-emoji reference and nothing else.
 *
 * Anchored, so neither leading nor trailing characters can ride along. This is
 * what the serializer checks before it will trust an element's
 * `data-emoji-token` and copy it into the wire text.
 */
export function isEmojiToken(text: string): boolean {
  return /^<:[a-z0-9_]{2,32}:[0-9a-f]{64}>$/.test(text);
}

/** Carries a custom emoji node's exact wire token. The atom of the editor. */
export const EMOJI_TOKEN_ATTR = "data-emoji-token";

/** Marks the terminal skin's inline autosuggest, which is never part of the value. */
export const GHOST_ATTR = "data-ghost";

/**
 * Mirrors the serialized value onto the host element.
 *
 * A `contentEditable` has no `.value`, so this is what a browser spec asserts
 * against in place of Playwright's `toHaveValue` — and what makes "what did the
 * composer actually think the message was" answerable from devtools.
 */
export const VALUE_ATTR = "data-value";

const ELEMENT_NODE = 1;
const TEXT_NODE = 3;

/**
 * Elements that start a new line when the browser (or a stray paste) produces
 * one. Not an exhaustive HTML block list — just the tags an editing host
 * actually generates.
 */
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

/** The subset of `Node` the serializer reads. */
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
    // The autosuggest is a rendering, not content.
    if (attributeOf(node, GHOST_ATTR) !== null) {
      continue;
    }
    const token = attributeOf(node, EMOJI_TOKEN_ATTR);
    // Validated rather than trusted: an element claiming a token it cannot have
    // is read as ordinary markup, so a forged attribute cannot smuggle
    // characters the grammar forbids into the wire text.
    if (token !== null && isEmojiToken(token)) {
      out.push(token);
      continue;
    }
    const tag = node.nodeName.toUpperCase();
    if (tag === "BR") {
      // Last child: the filler an editing host keeps at the end of a line so
      // the caret has somewhere to sit. Counting it would append a newline to
      // every message that ends in one.
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

/** Read a projected (or browser-mangled) subtree back into wire text. */
export function serializeRichInput(root: SerializableNode): string {
  const out: string[] = [];
  walk(root, out);
  return out.join("");
}

/** One atom of the projection: a run of text, or a single custom emoji. */
export type RichRun =
  | { kind: "text"; text: string }
  | { kind: "emoji"; token: string; shortcode: string; contentHash: string };

/**
 * Split wire text into the nodes to render.
 *
 * Empty text runs are dropped so the projection never contains an empty text
 * node, which browsers place carets inside inconsistently.
 */
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

/** A half-open offset range within the value. */
export interface OffsetRange {
  start: number;
  end: number;
}

/** Wire length of one segment — the token's own length, not the image's. */
function segmentLength(segment: ValueSegment): number {
  if (segment.kind === "text") {
    return segment.text.length;
  }
  // `<:` + shortcode + `:` + hash + `>`
  return segment.shortcode.length + segment.contentHash.length + 4;
}

/**
 * The emoji token ending exactly at `caret`, or null.
 *
 * Backspace is the reason this exists: a ~70-character token has to leave in
 * one keystroke, and asking the browser what "one character back" means next to
 * a `contenteditable=false` element gets a different answer per engine. Deciding
 * it against the model instead makes the keystroke deterministic.
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

/** The emoji token starting exactly at `caret`, or null. Forward-delete's half. */
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

/** Just enough of `DataTransfer` to read a paste. */
export interface ClipboardTextSource {
  getData: (type: string) => string;
}

/**
 * What a paste contributes to the message.
 *
 * `text/plain` and nothing else. A message is plain text plus tokens, so there
 * is no formatting worth preserving and therefore no reason to ever look at the
 * `text/html` flavour — the safest sanitizer for markup is the one that never
 * parses any. A raw `<:shortcode:hash>` in the pasted text still becomes an
 * emoji node, because it goes through the model and the model is re-projected.
 *
 * CRLF folds to LF (a paste from a Windows app would otherwise put a stray
 * carriage return on the wire) and the remaining C0 controls are dropped; tab
 * and newline stay, because both are things a person can type.
 */
export function pastedPlainText(data: ClipboardTextSource): string {
  const raw = data.getData("text/plain") ?? "";
  return raw
    .replace(/\r\n?/g, "\n")
    .replace(/[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F]/g, "");
}
