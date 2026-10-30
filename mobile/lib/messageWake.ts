// Which ingests a wake-up, a resume or a reconnect runs. Pure. A wake never counts unread itself
// (#1628): Rust's ingest stores what is new and the badges are re-read from Rust's counts.

import type { ConversationKind } from "../hooks/queries/useMessages";

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
