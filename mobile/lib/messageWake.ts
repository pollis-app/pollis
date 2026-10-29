// What a `new_message` wake-up (or a missed one) should do; mirrors desktop's
// `frontend/src/utils/messageWake.ts`. The wake-up names no sender, so whether
// a message is our OWN (from this account's other device) is known only after
// ingest; it is shown, never counted unread. A wake-up missed while backgrounded
// or reconnecting is gone, and our own message has no push to fall back on, so
// resume/reconnect re-ingests every listed conversation.

import type { ConversationKind } from "../hooks/queries/useMessages";

export interface WakeLatest {
  sender_id: string;
}

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
 * Never for the conversation on screen or our own message. When nothing could
 * be read back, it counts.
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

export interface CatchUpTarget {
  id: string;
  kind: ConversationKind;
}

/**
 * ONE ingest per group (a channel ingest catches up the whole MLS group) plus
 * one per DM.
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
 * A DM room is named by its conversation id, a group room by its group id
 * (caught up through any one channel). The inbox room carries no messages.
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
