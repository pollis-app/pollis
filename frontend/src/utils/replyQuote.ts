// What a reply's quote of its original says when the original's text is not on this device.
// Pure. mobile/lib/replyQuote.ts is a byte copy (mobile/tests/reply-quote.test.ts fails on drift).
//
// Every message is encrypted, so "encrypted" says nothing. The two real cases:
// - the original is here but this device could not decrypt it → "Couldn't decrypt";
// - the original is not here because it was sent before this user joined the group (MLS never
//   delivers those to a later member) → "Sent before you joined". Message ids are ULIDs, so the
//   original's send time is in its id even when the message itself never arrived.

const CROCKFORD = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/** The millisecond timestamp in a ULID's first 10 characters, or null if `id` is not a ULID. */
export function ulidTimeMs(id: string): number | null {
  if (typeof id !== "string" || id.length !== 26) {
    return null;
  }
  let ms = 0;
  for (const ch of id.slice(0, 10).toUpperCase()) {
    const v = CROCKFORD.indexOf(ch);
    if (v < 0) {
      return null;
    }
    ms = ms * 32 + v;
  }
  return ms;
}

/** Was the message `replyToId` sent before this user joined (`joinedAtMs`)? Unknown → false. */
export function sentBeforeJoin(replyToId: string, joinedAtMs: number | null | undefined): boolean {
  if (joinedAtMs == null || !Number.isFinite(joinedAtMs)) {
    return false;
  }
  const sent = ulidTimeMs(replyToId);
  return sent !== null && sent < joinedAtMs;
}
