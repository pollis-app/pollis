// Mirror of `mentions_all()` in pollis-core/src/commands/messages/send.rs (the
// source of truth), for the composer's "@all notifies everyone" hint. A
// standalone, case-insensitive `@all` matches ("@all," yes; "@allison" no).
export function mentionsAll(content: string): boolean {
  return content.split(/\s+/).some((word) => {
    // Trim trailing non-alphanumeric, non-'@' characters, as the Rust matcher does.
    const trimmed = word.replace(/[^\p{L}\p{N}@]+$/u, "");
    return trimmed.toLowerCase() === "@all";
  });
}

// Characters INSIDE a username after '@', mirroring `is_mention_body_char()`.
const MENTION_BODY = /[\p{L}\p{N}_.-]/u;

// Opens only at string start or after whitespace (so "a@b.com" isn't one);
// callers trim trailing '.', '-', '_' ("@dana." → `dana`).
const MENTION_RE = /(^|\s)@([\p{L}\p{N}_.-]+)/gu;

const MENTION_TRAIL = /[.\-_]+$/;

export interface MentionMatch {
  raw: string;
  /** Lowercased; what matching compares on. */
  name: string;
  start: number;
  end: number;
}

/**
 * Every `@mention` in `text`, including `@all`. MIRROR of `mention_tokens()` in
 * send.rs (which handles `@all` via `mentions_all()`); they must agree or the
 * composer promises a ping the backend doesn't send.
 */
export function findMentions(text: string): MentionMatch[] {
  const out: MentionMatch[] = [];
  MENTION_RE.lastIndex = 0;
  let match: RegExpExecArray | null;
  while ((match = MENTION_RE.exec(text)) !== null) {
    const lead = match[1].length;
    const body = match[2].replace(MENTION_TRAIL, "");
    if (body.length === 0) {
      continue;
    }
    const start = match.index + lead;
    out.push({
      raw: body,
      name: body.toLowerCase(),
      start,
      end: start + 1 + body.length,
    });
  }
  return out;
}

/** Lowercased names in `text` minus `@all`: exactly `mention_tokens()`'s roster set. */
export function mentionTokens(text: string): string[] {
  return findMentions(text)
    .map((m) => m.name)
    .filter((n) => n !== "all");
}

export interface MentionQuery {
  start: number;
  end: number;
  /** Typed after '@', lowercased; empty right after the '@'. */
  query: string;
}

/**
 * The mention the caret is typing, or null: an unterminated run right before
 * the caret, so a space ends the suggestion (as in Slack/Discord).
 */
export function mentionQueryAt(text: string, caret: number): MentionQuery | null {
  let i = caret;
  while (i > 0 && MENTION_BODY.test(text[i - 1])) {
    i -= 1;
  }
  if (i === 0 || text[i - 1] !== "@") {
    return null;
  }
  const at = i - 1;
  if (at > 0 && !/\s/.test(text[at - 1])) {
    return null;
  }
  return { start: at, end: caret, query: text.slice(i, caret).toLowerCase() };
}

/** Replace the query at `caret` with `@username ` (trailing space), returning text and caret. */
export function applyMention(
  text: string,
  caret: number,
  username: string,
): { text: string; caret: number } {
  const q = mentionQueryAt(text, caret);
  if (!q) {
    return { text, caret };
  }
  const inserted = `@${username} `;
  const next = text.slice(0, q.start) + inserted + text.slice(q.end);
  return { text: next, caret: q.start + inserted.length };
}

export interface MentionCandidate {
  userId: string;
  username: string;
  displayName?: string;
  avatarUrl?: string;
}

export const MENTION_SUGGESTION_LIMIT = 8;

/**
 * Candidates matching `query`, best first: prefix over substring, then
 * alphabetical; empty lists everyone. Never widens the visible pool.
 */
export function rankMentionCandidates(
  candidates: MentionCandidate[],
  query: string,
  limit: number = MENTION_SUGGESTION_LIMIT,
): MentionCandidate[] {
  const q = query.toLowerCase();
  const scored: Array<{ c: MentionCandidate; rank: number }> = [];
  for (const c of candidates) {
    const uname = c.username.toLowerCase();
    const display = (c.displayName ?? "").toLowerCase();
    if (q.length === 0) {
      scored.push({ c, rank: 1 });
      continue;
    }
    if (uname.startsWith(q) || display.startsWith(q)) {
      scored.push({ c, rank: 0 });
      continue;
    }
    if (uname.includes(q) || display.includes(q)) {
      scored.push({ c, rank: 1 });
    }
  }
  scored.sort((a, b) =>
    a.rank !== b.rank
      ? a.rank - b.rank
      : a.c.username.localeCompare(b.c.username, undefined, { sensitivity: "base" }),
  );
  return scored.slice(0, limit).map((s) => s.c);
}
