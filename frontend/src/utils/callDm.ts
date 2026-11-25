import type { DMConversation } from "../types";

/** The call id on `/call/<id>`, else null: only 1:1 calls use that route, never a group voice channel. */
export function callIdFromPath(pathname: string): string | null {
  const parts = pathname.split("/").filter(Boolean);
  if (parts[0] !== "call" || !parts[1]) {
    return null;
  }
  return decodeURIComponent(parts[1]);
}

/**
 * The 1:1 DM a call with `peerId` belongs to (#2116): the conversation whose members are exactly
 * the viewer and that peer. A group DM that merely contains the peer is never chosen, and with no
 * such DM there is none: a call never creates one. Calls are per user, so the list order (latest
 * activity first) breaks a tie.
 */
export function resolveCallDm(
  conversations: readonly DMConversation[],
  peerId: string | null,
): string | null {
  if (!peerId) {
    return null;
  }
  const match = conversations.find((c) => {
    const peers = c.peers ?? [];
    if (peers.length !== 1 || peers[0].user_id !== peerId) {
      return false;
    }
    return c.member_count === undefined || c.member_count === 2;
  });
  return match?.id ?? null;
}
