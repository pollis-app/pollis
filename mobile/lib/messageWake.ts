// What a `new_message` wake-up — or a missed one — should do.
//
// Mirrors desktop's `frontend/src/utils/messageWake.ts`. The wake-up names no
// sender (§5 signalling minimization), so whether a message is our OWN — sent
// from this account's other device — is only known after ingest, from the
// MLS-attributed `sender_id` of the newest local row. Our own message is shown
// exactly like anyone else's, but never counts as unread.
//
// The second half is the catch-up a missed wake-up needs. A LiveKit wake-up
// that fires while the app is backgrounded (or while the room is reconnecting)
// is simply gone, and a message from this user's other device is the one case
// that has no push to fall back on — the Delivery Service never pushes a
// sender's own message to their own account. So on resume / reconnect the app
// re-ingests the conversations it shows. Pure, so `tests/message-wake.test.ts`
// pins both rules.

import type { ConversationKind } from "../hooks/queries/useMessages";

/** The fields of the newest local message that the decision reads. */
export interface WakeLatest {
  sender_id: string;
}

/** True when the newest message in the conversation is the user's own. */
export function isOwnLatest(
  latest: WakeLatest | null | undefined,
  selfId: string | null | undefined,
): boolean {
  if (!latest || !selfId) {
    return false;
  }
  return latest.sender_id === selfId;
}

/**
 * Whether a wake-up should bump the unread badge. Never for the conversation
 * on screen, never for our own message. When nothing could be read back it
 * counts, as every wake-up did before the sender could be told apart.
 */
export function shouldCountUnread(args: {
  latest: WakeLatest | null | undefined;
  selfId: string | null | undefined;
  isOpen: boolean;
}): boolean {
  if (args.isOpen) {
    return false;
  }
  return !isOwnLatest(args.latest, args.selfId);
}

/** One ingest the catch-up runs. */
export interface CatchUpTarget {
  id: string;
  kind: ConversationKind;
}

/**
 * The ingests that bring every listed conversation up to date. ONE per group —
 * channel ingest catches up the whole MLS group, all sibling channels with it —
 * plus one per DM. A group with no text channel has nothing to show.
 */
export function catchUpTargets(
  groups: ReadonlyArray<{ channels: ReadonlyArray<{ id: string }> }>,
  dms: ReadonlyArray<{ id: string }>,
): CatchUpTarget[] {
  const out: CatchUpTarget[] = [];
  for (const g of groups) {
    const first = g.channels[0];
    if (first) {
      out.push({ id: first.id, kind: "channel" });
    }
  }
  for (const d of dms) {
    out.push({ id: d.id, kind: "dm" });
  }
  return out;
}

/**
 * Which ingest a reconnected room needs: a DM room is named by its
 * conversation id; a group room by its group id, caught up through any one of
 * its channels. The inbox room carries no messages.
 */
export function roomCatchUpTarget(
  roomName: string,
  groups: ReadonlyArray<{ id: string; channels: ReadonlyArray<{ id: string }> }>,
  dms: ReadonlyArray<{ id: string }>,
): CatchUpTarget | null {
  if (roomName.startsWith("inbox-")) {
    return null;
  }
  const group = groups.find((g) => g.id === roomName);
  if (group) {
    const first = group.channels[0];
    return first ? { id: first.id, kind: "channel" } : null;
  }
  if (dms.some((d) => d.id === roomName)) {
    return { id: roomName, kind: "dm" };
  }
  return null;
}
