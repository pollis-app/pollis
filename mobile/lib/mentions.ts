// PORT of frontend/src/utils/mentions.ts; pollis-core/src/commands/messages/send.rs is the source of truth for both.
export function mentionsAll(content: string): boolean {
  return content.split(/\s+/).some((word) => {
    // The same predicate the Rust matcher uses.
    const trimmed = word.replace(/[^\p{L}\p{N}@]+$/u, "");
    return trimmed.toLowerCase() === "@all";
  });
}

// Mirrors `is_mention_body_char()` in send.rs.
const MENTION_BODY = /[\p{L}\p{N}_.-]/u;

// A mention opens only at the start or after whitespace ("a@b.com" is not one).
const MENTION_RE = /(^|\s)@([\p{L}\p{N}_.-]+)/gu;

const MENTION_TRAIL = /[.\-_]+$/;

export interface MentionMatch {
  raw: string;
  name: string;
  start: number;
  end: number;
}

/** Every `@mention` in `text`, including `@all`. MIRROR of `mention_tokens()` in send.rs. */
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

/** Lowercased usernames named in `text`, excluding `@all`. Mirrors `mention_tokens()`. */
export function mentionTokens(text: string): string[] {
  return findMentions(text)
    .map((m) => m.name)
    .filter((n) => n !== "all");
}

export interface MentionQuery {
  start: number;
  end: number;
  query: string;
}

/** The mention the caret is typing, or null; a space ends it. */
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

/** Replace the mention query at `caret` with `@username ` (trailing space). */
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

/** Prefix matches first, then alphabetical. Never widens the pool beyond visible members. */
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
