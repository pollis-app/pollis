// Typing indicator rules (#1635), desktop's `typingStore` / `useTypingPublisher` constants and throttle.

// Receiver-side TTL, for senders that drop offline mid-keystroke.
export const TYPING_TTL_MS = 6000;

// Sender re-emit cadence; below TYPING_TTL_MS.
export const TYPING_REFRESH_MS = 3000;

export type TypingRoomKey = `channel:${string}` | `dm:${string}`;

export function typingRoomKey(
  channelId: string | null | undefined,
  conversationId: string | null | undefined,
): TypingRoomKey | null {
  if (channelId) {
    return `channel:${channelId}`;
  }
  if (conversationId) {
    return `dm:${conversationId}`;
  }
  return null;
}

/** The identity-free wire body desktop's `typing_payload` builds (#836). */
export function typingPayload(
  channelId: string | null,
  conversationId: string | null,
  isTyping: boolean,
): Record<string, unknown> {
  return {
    type: "typing",
    channel_id: channelId,
    conversation_id: conversationId,
    is_typing: isTyping,
  };
}

type Timer = ReturnType<typeof setTimeout>;

/** A throttled `notify(draft)` per keystroke; one stop on empty, idle, send or `stop()`. */
export function createTypingThrottle(deps: {
  publish: (isTyping: boolean) => void;
  now?: () => number;
  schedule?: (fn: () => void, ms: number) => Timer;
  cancel?: (t: Timer) => void;
}) {
  const now = deps.now ?? Date.now;
  const schedule = deps.schedule ?? ((fn: () => void, ms: number): Timer => setTimeout(fn, ms));
  const cancel = deps.cancel ?? ((t: Timer) => clearTimeout(t));
  let lastSent = 0;
  let active = false;
  let stopTimer: Timer | null = null;

  function stop() {
    if (stopTimer) {
      cancel(stopTimer);
      stopTimer = null;
    }
    if (active) {
      active = false;
      lastSent = 0;
      deps.publish(false);
    }
  }

  function notify(value: string) {
    if (value.trim().length === 0) {
      stop();
      return;
    }
    const t = now();
    if (t - lastSent >= TYPING_REFRESH_MS) {
      lastSent = t;
      active = true;
      deps.publish(true);
    }
    if (stopTimer) {
      cancel(stopTimer);
    }
    stopTimer = schedule(stop, TYPING_REFRESH_MS);
  }

  return { notify, stop };
}

/** Which line the indicator draws; `null` = nobody typing. */
export type TypingLine =
  | { kind: "one"; name: string }
  | { kind: "two"; first: string; second: string }
  | { kind: "several"; count: number };

export function describeTyping(usernames: string[]): TypingLine | null {
  if (usernames.length === 0) {
    return null;
  }
  if (usernames.length === 1) {
    return { kind: "one", name: usernames[0] };
  }
  if (usernames.length === 2) {
    return { kind: "two", first: usernames[0], second: usernames[1] };
  }
  return { kind: "several", count: usernames.length };
}
