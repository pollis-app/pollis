// Message formatting (#2142): the one parser desktop and mobile render from. Pure and
// import-free, like theme.ts beside it, so both apps and both `node --test` suites load it as is.
// pollis-core/src/message_format.rs is the Rust twin (notification and search previews, the
// terminal client); both run the case table in messageFormat.cases.json, so edit them together.
//
// Exactly five styles: **bold**, *italic* or _italic_, __underline__ (Discord's meaning, not
// GitHub's), ~~strikethrough~~, and ``` fenced code blocks. The output is a token tree; each
// platform builds its own elements from it, never an HTML string.
//
// Rules (Discord's behaviour, GitHub's flanking where Discord is loose):
// - A fence is a line of ``` with an optional language word; a later line of ``` closes it.
//   Inside, nothing is parsed. A fence that never closes is ordinary text.
// - `\` before \ * _ ~ or ` keeps that character literal.
// - Markers need content: the text inside may not start or end with whitespace.
// - `_` and `__` never open or close inside a word; `*` and `**` do (GitHub), except next to a
//   digit, so `2*3*4` stays arithmetic.
// - URLs, @mentions and emoji tokens are atoms: a marker inside one is part of it, so
//   `https://x.example/_a_` stays a link and `@_bob_` a mention.
//
// Indices are code points (Array.from), the same units the Rust twin's Vec<char> uses.

export type FormatStyle = "bold" | "italic" | "underline" | "strike";

export type FormatNode =
  | { type: "text"; text: string }
  | { type: FormatStyle; children: FormatNode[] }
  | { type: "code"; lang: string | null; text: string };

/** Nesting deeper than this stays literal. */
const MAX_DEPTH = 16;

const ESCAPABLE = new Set(["\\", "*", "_", "~", "`"]);
// Rust's `char::is_whitespace` (Unicode White_Space); JS `\s` differs at U+0085 and U+FEFF.
const WS = /[\t\n\v\f\r \u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]/;
// Rust's `char::is_alphanumeric` and `char::is_numeric`.
const ALNUM = /[\p{Alphabetic}\p{N}]/u;
const NUM = /\p{N}/u;
// A URL or mention glued to one of these is part of a longer word, not an atom.
const GLUE = /[\p{Alphabetic}\p{N}.@/+-]/u;
const MENTION_BODY = /[\p{Alphabetic}\p{N}_.-]/u;
const LANG = /^[A-Za-z0-9_+#.-]*$/;
const URL_STOP = new Set(["<", ">", '"', "'", "`"]);
const SHORTCODE = /^[a-z0-9_+-]$/;
const CUSTOM_EMOJI_NAME = /^[a-z0-9_]$/;
const HEX = /^[0-9a-f]$/;

const isWs = (c: string | undefined): boolean => c !== undefined && WS.test(c);

/** The token tree of a message body. Plain text is one text node; empty text is none. */
export function parseFormatting(text: string): FormatNode[] {
  if (text === "") {
    return [];
  }
  if (!/[*_~\\`]/.test(text)) {
    return [{ type: "text", text }];
  }
  const out: FormatNode[] = [];
  const lines = text.split("\n");
  let from = 0;
  let i = 0;
  while (i < lines.length) {
    const lang = fenceLang(lines[i]);
    if (lang === null) {
      i += 1;
      continue;
    }
    let close = -1;
    for (let j = i + 1; j < lines.length; j++) {
      if (fenceLang(lines[j]) === "") {
        close = j;
        break;
      }
    }
    if (close < 0) {
      // Every later fence would only close this one, so the rest is text.
      break;
    }
    if (i > from) {
      out.push(...parseInlineText(lines.slice(from, i).join("\n")));
    }
    out.push({ type: "code", lang: lang === "" ? null : lang, text: lines.slice(i + 1, close).join("\n") });
    from = close + 1;
    i = close + 1;
  }
  if (from < lines.length) {
    out.push(...parseInlineText(lines.slice(from).join("\n")));
  }
  return out;
}

/** The words without the markers, for previews: code keeps its text, set off by line breaks. */
export function stripFormatting(text: string): string {
  return plainText(parseFormatting(text));
}

/** `stripFormatting` over a tree already parsed. */
export function plainText(nodes: readonly FormatNode[]): string {
  let out = "";
  let afterCode = false;
  for (const node of nodes) {
    if (node.type === "code") {
      if (out !== "") {
        out += "\n";
      }
      out += node.text;
      afterCode = true;
      continue;
    }
    if (afterCode) {
      out += "\n";
      afterCode = false;
    }
    out += node.type === "text" ? node.text : plainText(node.children);
  }
  return out;
}

/**
 * The text outside code blocks, one line per formatted run: what mention and link detection
 * read, so a mention or URL counts exactly where the renderer draws one, and never in code.
 */
export function scanText(text: string): string {
  const runs: string[] = [];
  collectRuns(parseFormatting(text), runs);
  return runs.join("\n");
}

function collectRuns(nodes: readonly FormatNode[], runs: string[]): void {
  for (const node of nodes) {
    if (node.type === "text") {
      runs.push(node.text);
    } else if (node.type !== "code") {
      collectRuns(node.children, runs);
    }
  }
}

/** The language word of a fence line (`""` for none), or null when the line is not a fence. */
function fenceLang(line: string): string | null {
  const trimmed = line.replace(/\r$/, "").replace(/^[ \t]+|[ \t]+$/g, "");
  if (!trimmed.startsWith("```")) {
    return null;
  }
  const lang = trimmed.slice(3);
  return LANG.test(lang) ? lang : null;
}

function parseInlineText(text: string): FormatNode[] {
  const cs = Array.from(text);
  return parseInline(cs, 0, cs.length, 0);
}

function parseInline(cs: string[], lo: number, hi: number, depth: number): FormatNode[] {
  const out: FormatNode[] = [];
  // Per marker, the lowest start a closer search already failed from: a later one fails too.
  const failed = new Map<string, number>();
  let buf = "";
  let i = lo;
  while (i < hi) {
    const c = cs[i];
    if (c === "\\" && i + 1 < hi && ESCAPABLE.has(cs[i + 1])) {
      buf += cs[i + 1];
      i += 2;
      continue;
    }
    const atomEnd = atom(cs, i, lo, hi);
    if (atomEnd > i) {
      buf += cs.slice(i, atomEnd).join("");
      i = atomEnd;
      continue;
    }
    if (depth < MAX_DEPTH && (c === "*" || c === "_" || c === "~")) {
      const span = delimited(cs, i, lo, hi, depth, failed);
      if (span !== null) {
        if (buf !== "") {
          out.push({ type: "text", text: buf });
          buf = "";
        }
        out.push(span.node);
        i = span.next;
        continue;
      }
    }
    buf += c;
    i += 1;
  }
  if (buf !== "") {
    out.push({ type: "text", text: buf });
  }
  return out;
}

function delimited(
  cs: string[],
  i: number,
  lo: number,
  hi: number,
  depth: number,
  failed: Map<string, number>,
): { node: FormatNode; next: number } | null {
  const c = cs[i];
  const double = i + 1 < hi && cs[i + 1] === c;
  let type: FormatStyle;
  if (c === "~") {
    if (!double) {
      return null;
    }
    type = "strike";
  } else if (c === "*") {
    type = double ? "bold" : "italic";
  } else {
    type = double ? "underline" : "italic";
  }
  const len = double ? 2 : 1;
  const prev = i > lo ? cs[i - 1] : undefined;
  if (c === "*" && prev !== undefined && NUM.test(prev)) {
    return null;
  }
  // An underscore after an underscore is still inside the word (`x__y_`).
  if (c === "_" && prev !== undefined && (ALNUM.test(prev) || prev === "_")) {
    return null;
  }
  const start = i + len;
  if (start >= hi || isWs(cs[start])) {
    return null;
  }
  const key = c + len;
  const failedFrom = failed.get(key);
  if (failedFrom !== undefined && start >= failedFrom) {
    return null;
  }
  const close = findCloser(cs, c, len, start, lo, hi);
  if (close < 0) {
    failed.set(key, Math.min(failedFrom ?? start, start));
    return null;
  }
  return {
    node: { type, children: parseInline(cs, start, close, depth + 1) },
    next: close + len,
  };
}

function findCloser(cs: string[], c: string, len: number, from: number, lo: number, hi: number): number {
  let k = from;
  while (k < hi) {
    const ch = cs[k];
    if (ch === "\\" && k + 1 < hi && ESCAPABLE.has(cs[k + 1])) {
      k += 2;
      continue;
    }
    const atomEnd = atom(cs, k, lo, hi);
    if (atomEnd > k) {
      k = atomEnd;
      continue;
    }
    if (ch !== c) {
      k += 1;
      continue;
    }
    const pair = k + 1 < hi && cs[k + 1] === c;
    if (len === 2) {
      if (pair && k > from && !isWs(cs[k - 1]) && !(k + 2 < hi && cs[k + 2] === c) && closeFlank(c, cs, k + 2, hi)) {
        return k;
      }
      k += 1;
      continue;
    }
    if (pair) {
      // A double marker inside single-marker text is its own span, never half a closer.
      k += 2;
      continue;
    }
    if (k > from && !isWs(cs[k - 1]) && closeFlank(c, cs, k + 1, hi)) {
      return k;
    }
    k += 1;
  }
  return -1;
}

function closeFlank(c: string, cs: string[], after: number, hi: number): boolean {
  const next = after < hi ? cs[after] : undefined;
  if (next === undefined) {
    return true;
  }
  if (c === "*") {
    return !NUM.test(next);
  }
  if (c === "_") {
    return !ALNUM.test(next);
  }
  return true;
}

/** The end of a URL, mention or emoji token starting at `k`, or `k` when none starts there. */
function atom(cs: string[], k: number, lo: number, hi: number): number {
  const c = cs[k];
  const prev = k > lo ? cs[k - 1] : undefined;
  if ((c === "h" || c === "H" || c === "w" || c === "W") && (prev === undefined || !GLUE.test(prev))) {
    const prefix = urlPrefix(cs, k, hi);
    if (prefix > 0) {
      let end = k + prefix;
      while (end < hi && !isWs(cs[end]) && !URL_STOP.has(cs[end])) {
        end += 1;
      }
      while (end > k + prefix && (cs[end - 1] === "*" || cs[end - 1] === "_" || cs[end - 1] === "~")) {
        end -= 1;
      }
      return end > k + prefix ? end : k;
    }
    return k;
  }
  if (c === "@" && (prev === undefined || !GLUE.test(prev))) {
    let end = k + 1;
    while (end < hi && MENTION_BODY.test(cs[end])) {
      end += 1;
    }
    while (end > k + 1 && (cs[end - 1] === "." || cs[end - 1] === "-" || cs[end - 1] === "_")) {
      end -= 1;
    }
    return end > k + 1 ? end : k;
  }
  if (c === "<" && k + 1 < hi && cs[k + 1] === ":") {
    let end = k + 2;
    while (end < hi && end - (k + 2) < 32 && CUSTOM_EMOJI_NAME.test(cs[end])) {
      end += 1;
    }
    const nameLen = end - (k + 2);
    if (nameLen < 2 || end >= hi || cs[end] !== ":") {
      return k;
    }
    let h = end + 1;
    while (h < hi && h - (end + 1) < 64 && HEX.test(cs[h])) {
      h += 1;
    }
    if (h - (end + 1) !== 64 || h >= hi || cs[h] !== ">") {
      return k;
    }
    return h + 1;
  }
  if (c === ":") {
    let end = k + 1;
    while (end < hi && end - (k + 1) < 64 && SHORTCODE.test(cs[end])) {
      end += 1;
    }
    if (end > k + 1 && end < hi && cs[end] === ":") {
      return end + 1;
    }
    return k;
  }
  return k;
}

/** Length of `http://`, `https://` or `www.` at `k` (any case), or 0. */
function urlPrefix(cs: string[], k: number, hi: number): number {
  for (const p of ["https://", "http://", "www."]) {
    if (k + p.length > hi) {
      continue;
    }
    let ok = true;
    for (let n = 0; n < p.length; n++) {
      // ASCII case only, as Rust's `eq_ignore_ascii_case`.
      if (cs[k + n] !== p[n] && cs[k + n] !== p[n].toUpperCase()) {
        ok = false;
        break;
      }
    }
    if (ok) {
      return p.length;
    }
  }
  return 0;
}
