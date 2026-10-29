// What a `new_message` wake-up does after ingest (§5: it names no sender); our own messages never alert.

/** The fields of the newest local message that the decision reads. */
export interface WakeLatest {
  sender_id: string;
  sender_username?: string | null;
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

/** Alert unless on screen or our own; alert when nothing could be read back. */
export function shouldAlertForWake(args: {
  latest: WakeLatest | null | undefined;
  selfId: string | null | undefined;
  isSelected: boolean;
}): boolean {
  if (args.isSelected) {
    return false;
  }
  return !isOwnLatest(args.latest, args.selfId);
}
