// What a `new_message` wake-up announces after ingest (§5: it names no sender).
// #1628: only news. Rust's ingest reports, per conversation, the newest message it stored for
// the first time that is someone else's and past the read cursor. A receipt, an edit, a replay
// or a message already read on another device reports nothing, so it never alerts or counts.

/** One message the ingest reports as news; the fields the decision and the alert read. */
export interface WakeFresh {
  conversation_id: string;
  sender_id: string;
  sender_username?: string | null;
}

/** True when the message is the user's own (Rust never reports one; kept as a guard). */
export function isOwn(
  message: Pick<WakeFresh, 'sender_id'> | null | undefined,
  selfId: string | null | undefined,
): boolean {
  if (!message || !selfId) {
    return false;
  }
  return message.sender_id === selfId;
}

/** The reported news worth an alert: never our own, never the conversation on screen. */
export function wakeAlerts(args: {
  fresh: readonly WakeFresh[] | null | undefined;
  selfId: string | null | undefined;
  isSelected: (conversationId: string) => boolean;
}): WakeFresh[] {
  if (!args.fresh) {
    return [];
  }
  return args.fresh.filter(
    (m) => !isOwn(m, args.selfId) && !args.isSelected(m.conversation_id),
  );
}

/**
 * The `new_message` handler's decision, with its effects injected: ingest, then refetch the
 * unread counts Rust derives (never a local increment, which drifted, #1628), then announce
 * each reported message that is worth it. A failed ingest stored nothing, so it announces nothing.
 */
export async function runMessageWake(deps: {
  ingest: () => Promise<readonly WakeFresh[] | null | undefined>;
  refreshUnread: () => void;
  selfId: string | null | undefined;
  isSelected: (conversationId: string) => boolean;
  announce: (message: WakeFresh) => void;
}): Promise<WakeFresh[]> {
  let fresh: readonly WakeFresh[] | null | undefined = null;
  try {
    fresh = await deps.ingest();
  } catch (err) {
    console.warn('[realtime] wake ingest failed:', err);
  }
  deps.refreshUnread();
  const alerts = wakeAlerts({ fresh, selfId: deps.selfId, isSelected: deps.isSelected });
  for (const m of alerts) {
    deps.announce(m);
  }
  return alerts;
}
