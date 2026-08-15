// Mirror of `mentions_all()` in pollis-core/src/commands/messages/send.rs.
// Keep the two in sync: the backend is the source of truth for whether a
// message actually pings everyone, and this drives the composer hint that
// tells the sender it will. A standalone `@all` token matches (whitespace-
// delimited, trailing punctuation ignored) so "@all" and "@all," match but
// "@allison" and "email@allcorp" do not. Case-insensitive.
export function mentionsAll(content: string): boolean {
  return content.split(/\s+/).some((word) => {
    // Trim trailing characters that are neither alphanumeric nor '@', the
    // same predicate the Rust matcher uses.
    const trimmed = word.replace(/[^\p{L}\p{N}@]+$/u, "");
    return trimmed.toLowerCase() === "@all";
  });
}

// Characters that may appear INSIDE a username after the leading '@'. Mirrors
// `is_mention_body_char()` in send.rs — deliberately wide (the `users.username`
// column has no charset constraint) but closed over sentence punctuation.
const MENTION_BODY = /[\p{L}\p{N}_.-]/u;

// A mention opens only at the start of the string or right after whitespace,
// which is what stops "email@allcorp" and "a@b.com" from reading as mentions.
// The body is captured greedily; trailing '.', '-' and '_' are trimmed by the
// callers below so "@dana." mentions `dana`.
const MENTION_RE = /(^|\s)@([\p{L}\p{N}_.-]+)/gu;

// Trailing punctuation the Rust side trims off a mention body.
const MENTION_TRAIL = /[.\-_]+$/;

/** One `@mention` found in a string, with the slice it occupies. */
export interface MentionMatch {
  /** Username as typed, without the leading '@'. */
  raw: string;
  /** Lowercased username — what matching compares on. */
  name: string;
  /** Index of the '@' in the source string. */
  start: number;
  /** Index one past the last character of the mention. */
  end: number;
}

/**
 * Every `@mention` in `text`, in order, including `@all`.
 *
 * MIRROR: `mention_tokens()` in pollis-core/src/commands/messages/send.rs,
 * except that this keeps `@all` (the renderer highlights it; the backend
 * handles it through `mentions_all()` instead). The two must agree on what
 * counts as a mention or the composer will promise a ping the backend does
 * not send.
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
      // +1 for the '@' itself.
      end: start + 1 + body.length,
    });
  }
  return out;
}

/**
 * Lowercased usernames named in `text`, excluding `@all`. Mirrors
 * `mention_tokens()` exactly — this is the set the backend resolves against
 * the channel roster.
 */
export function mentionTokens(text: string): string[] {
  return findMentions(text)
    .map((m) => m.name)
    .filter((n) => n !== "all");
}

/** An in-progress `@…` the caret is sitting inside. */
export interface MentionQuery {
  /** Index of the '@'. */
  start: number;
  /** Caret index — always the end of the query. */
  end: number;
  /** Text typed after the '@', lowercased. Empty right after typing '@'. */
  query: string;
}

/**
 * The mention the caret is currently typing, or null.
 *
 * Only an unterminated run of mention-body characters immediately before the
 * caret counts, so the suggestion disappears the moment the user types a
 * space — matching Slack/Discord, where a completed mention stops offering
 * alternatives.
 */
export function mentionQueryAt(text: string, caret: number): MentionQuery | null {
  let i = caret;
  while (i > 0 && MENTION_BODY.test(text[i - 1])) {
    i -= 1;
  }
  // The character before the run must be the '@' that opens the mention.
  if (i === 0 || text[i - 1] !== "@") {
    return null;
  }
  const at = i - 1;
  // …and that '@' must itself start the string or follow whitespace.
  if (at > 0 && !/\s/.test(text[at - 1])) {
    return null;
  }
  return { start: at, end: caret, query: text.slice(i, caret).toLowerCase() };
}

/**
 * Replace the mention query at `caret` with `@username `, returning the new
 * text and where the caret should land. A trailing space is appended so the
 * user can keep typing straight away (Slack/Discord do the same).
 */
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

/** A person who can be mentioned — normalized across group members and DMs. */
export interface MentionCandidate {
  userId: string;
  username: string;
  displayName?: string;
  avatarUrl?: string;
}

/** Max suggestions shown at once, matching Slack's composer list depth. */
export const MENTION_SUGGESTION_LIMIT = 8;

/**
 * Candidates matching `query`, best first.
 *
 * Prefix matches rank above substring matches (so "da" offers `dana` before
 * `adam`), ties break alphabetically, and an empty query lists everyone. The
 * pool is only ever the members the user can already see — this function never
 * widens it, which is what keeps mentions from becoming a user directory.
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
