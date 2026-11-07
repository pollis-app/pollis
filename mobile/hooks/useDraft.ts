import { useCallback, useRef, useState } from "react";
import { getDraft, setDraft as storeDraft } from "../lib/drafts";

type Next = string | ((prev: string) => string);

/** Composer text that outlives the screen: kept per `key` (desktop's draft keys) until sent or the session ends. */
export function useDraft(key: string | null): [string, (next: Next) => void] {
  const [draft, setDraftState] = useState(() => getDraft(key));
  // Re-sync during render when the key changes (an iPad pane reused for another conversation), so no stale frame shows.
  const [prevKey, setPrevKey] = useState(key);
  if (key !== prevKey) {
    setPrevKey(key);
    setDraftState(getDraft(key));
  }
  const keyRef = useRef(key);
  keyRef.current = key;
  const setDraft = useCallback((next: Next) => {
    setDraftState((prev) => {
      const value = typeof next === "function" ? next(prev) : next;
      storeDraft(keyRef.current, value);
      return value;
    });
  }, []);
  return [draft, setDraft];
}

/** Desktop's key for a conversation's main composer: the room-kind prefix keeps channel and DM slots apart. */
export function conversationDraftKey(
  kind: "channel" | "dm" | null,
  conversationId: string | null,
): string | null {
  if (!conversationId || !kind) {
    return null;
  }
  return kind === "channel" ? `channel:${conversationId}` : `conv:${conversationId}`;
}
