// PORT: copy of frontend/src/utils/mentions.ts; keep the two in sync. The Rust
// matcher in pollis-core/src/commands/messages/send.rs is the source of truth.
//
// Mirrors `mentions_all()`: a standalone, case-insensitive `@all` ("@all," yes;
// "@allison", "email@allcorp" no). Drives the composer's ping-everyone hint.
export function mentionsAll(content: string): boolean {
  return content.split(/\s+/).some((word) => {
    // Same trailing-trim predicate as the Rust matcher.
    const trimmed = word.replace(/[^\p{L}\p{N}@]+$/u, "");
    return trimmed.toLowerCase() === "@all";
  });
}

// Mirrors `is_mention_body_char()`: deliberately wide (`users.username` has no
// charset constraint) but closed over sentence punctuation.
const MENTION_BODY = /[\p{L}\p{N}_.-]/u;

// A mention opens only at the start or after whitespace (so "a@b.com" is not
// one); trailing '.', '-', '_' are trimmed so "@dana." mentions `dana`.
const MENTION_RE = /(^|\s)@([\p{L}\p{N}_.-]+)/gu;

const MENTION_TRAIL = /[.\-_]+$/;

export interface MentionMatch {
  /** Username as typed, without the leading '@'. */
  raw: string;
  /** Lowercased; what matching compares on. */
  name: string;
  /** Index of the '@' in the source string. */
  start: number;
  /** Index one past the last character of the mention. */
  end: number;
}

/**
 * Every `@mention` in `text`, in order. Mirrors `mention_tokens()` in send.rs
 * except that it keeps `@all`; the two must agree, or the composer promises a
 * ping the backend does not send.
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

/** Lowercased usernames in `text` minus `@all`: the set the backend resolves. */
export function mentionTokens(text: string): string[] {
  return findMentions(text)
    .map((m) => m.name)
    .filter((n) => n !== "all");
}

export interface MentionQuery {
  /** Index of the '@'. */
  start: number;
  /** Caret index — always the end of the query. */
  end: number;
  /** Text typed after the '@', lowercased. Empty right after typing '@'. */
  query: string;
}

/**
 * The mention the caret is typing, or null. Only an unterminated run right
 * before the caret counts, so a typed space ends the suggestion.
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

/** Replace the query at `caret` with `@username ` (trailing space included). */
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
 * Candidates matching `query`: prefix matches before substring ones, ties
 * alphabetical, empty query lists all. Never widens the pool beyond members the
 * user can already see, so mentions never become a user directory.
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
