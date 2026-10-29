// What a `new_message` wake-up, or a missed one, should do (mirrors desktop's messageWake.ts). Own messages show but never count unread. Pure.

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

/** Never for the conversation on screen or our own message; counts when nothing could be read back. */
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

/** ONE ingest per group (it catches up every channel) plus one per DM. */
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

/** A DM room is named by conversation id, a group room by group id; the inbox has no messages. */
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
