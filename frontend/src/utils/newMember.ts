// The "new" tag on a fresh member's messages for a week after the DS-stamped
// `group_member.joined_at` (#1506), so every device tags the same messages.
// `mobile/lib/newMember.ts` must match byte for byte from the window constant
// down (`new-member.test.ts`).

/** How long after joining a member's messages carry the tag. */
export const NEW_MEMBER_WINDOW_MS = 7 * 24 * 60 * 60 * 1000;

/**
 * Tolerance for a message stamped slightly before `joined_at` by a sender
 * whose clock runs behind. Messages genuinely older than the join (a member
 * who left and came back) are not tagged.
 */
const CLOCK_SKEW_MS = 5 * 60 * 1000;

// SQLite's `datetime('now')`: "YYYY-MM-DD HH:MM:SS", always UTC, no zone.
const SQLITE_UTC = /^(\d{4})-(\d{2})-(\d{2})[ T](\d{2}):(\d{2}):(\d{2})(?:\.(\d+))?$/;

/**
 * Epoch ms of a server timestamp, or null when it does not parse. A zone-less
 * value is UTC: that is what the DS writes, and `new Date()` would otherwise
 * read it in the viewer's zone and shift the window by their offset.
 */
export function parseServerTimestamp(value: string | null | undefined): number | null {
  if (!value) {
    return null;
  }
  const m = SQLITE_UTC.exec(value.trim());
  if (m) {
    const ms = m[7] ? Number(`0.${m[7]}`) * 1000 : 0;
    return Date.UTC(+m[1], +m[2] - 1, +m[3], +m[4], +m[5], +m[6], Math.round(ms));
  }
  const parsed = Date.parse(value);
  return Number.isNaN(parsed) ? null : parsed;
}

/**
 * user id → joined-at ms for every member who can carry the tag. The group's
 * creator never does: in a brand-new group they would otherwise wear it on
 * every message for its first week.
 */
export function newMemberJoins(
  members: ReadonlyArray<{ user_id: string; joined_at: string }>,
  creatorId: string | null | undefined,
): Map<string, number> {
  const out = new Map<string, number>();
  for (const m of members) {
    if (creatorId && m.user_id === creatorId) {
      continue;
    }
    const joined = parseServerTimestamp(m.joined_at);
    if (joined !== null) {
      out.set(m.user_id, joined);
    }
  }
  return out;
}

/** Whether a message sent at `sentAtMs` by `senderId` carries the tag. */
export function isNewMemberMessage(
  joins: ReadonlyMap<string, number>,
  senderId: string,
  sentAtMs: number,
): boolean {
  const joined = joins.get(senderId);
  if (joined === undefined) {
    return false;
  }
  return sentAtMs >= joined - CLOCK_SKEW_MS && sentAtMs < joined + NEW_MEMBER_WINDOW_MS;
}
