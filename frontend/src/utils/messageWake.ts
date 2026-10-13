// What a `new_message` wake-up should do once its envelopes are ingested.
//
// The wake-up names no sender (§5 signalling minimization), so whether the
// message is our OWN — sent from this account's other device — is only known
// after ingest, from the MLS-attributed `sender_id` of the newest local row.
// A message of our own is shown exactly like anyone else's, but never raises
// an alert or an unread badge. Kept pure so the rule is testable without a
// renderer (`frontend/tests/message-wake.test.ts`).

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

/**
 * Whether a wake-up should alert (notification + unread badge).
 *
 * Never for the conversation on screen, never for our own message. When
 * nothing could be read back (ingest failed, or the message is not decryptable
 * yet) it alerts, as every wake-up did before the sender could be told apart:
 * a missed alert for someone else's message is the worse failure.
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
