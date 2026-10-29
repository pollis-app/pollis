// What a `new_message` wake-up does after ingest. It names no sender (§5), so
// "our own message from another device" is known only from the newest local
// row's MLS-attributed `sender_id`; ours shows but never alerts or badges.

export interface WakeLatest {
  sender_id: string;
  sender_username?: string | null;
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
 * Whether a wake-up alerts (notification + badge): never for the open
 * conversation or our own message. If nothing could be read back it alerts,
 * since a missed alert is the worse failure.
 */
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
