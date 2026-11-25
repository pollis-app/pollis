// One conversation's reaction map, read in ONE `get_reactions_for_messages` call per batch (#2115). Pure.

import type { Reaction } from "./useReactions";

/** The native bridge's `invoke`, injected so these rules run under node --test. */
export type ReactionInvoke = <T>(command: string, args: Record<string, unknown>) => Promise<T>;

/** An entry per message: `[]` is "loaded, no reactions", absent is "not loaded yet". */
export type ReactionMap = Map<string, Reaction[]>;

/** Rust groups via a HashMap: sort for a stable pill order. */
export function sortPills(reactions: readonly Reaction[]): Reaction[] {
  return [...reactions].sort((a, b) => a.emoji.localeCompare(b.emoji));
}

/** The listed ids the map holds no entry for, deduplicated, in list order. */
export function missingIds(
  map: ReadonlyMap<string, Reaction[]> | undefined,
  messageIds: readonly string[],
): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  for (const id of messageIds) {
    if (!seen.has(id) && !map?.has(id)) {
      seen.add(id);
      out.push(id);
    }
  }
  return out;
}

/** `messageIds`' reactions in one call; every id gets an entry. */
export async function readReactions(
  messageIds: readonly string[],
  invoke: ReactionInvoke,
): Promise<ReactionMap> {
  const out: ReactionMap = new Map();
  if (messageIds.length === 0) {
    return out;
  }
  const byId = await invoke<Record<string, Reaction[]> | null>("get_reactions_for_messages", {
    messageIds: [...messageIds],
  });
  for (const id of messageIds) {
    out.set(id, sortPills(byId?.[id] ?? []));
  }
  return out;
}

/** How long a freshly opened conversation's list waits for its reactions before showing without them. */
export const REACTIONS_REVEAL_WAIT_MS = 150;

/**
 * Whether the list may show: nothing loaded yet (the empty or loading state shows instead), every
 * loaded message's reactions known, or the wait ran out. Once shown, a conversation stays shown.
 */
export function mayReveal(
  map: ReadonlyMap<string, Reaction[]> | undefined,
  messageIds: readonly string[],
  timedOut: boolean,
): boolean {
  return messageIds.length === 0 || timedOut || missingIds(map, messageIds).length === 0;
}

/** `previous` with `fresh`'s entries laid over it; entries `fresh` did not read are kept. */
export function mergeReactions(
  previous: ReadonlyMap<string, Reaction[]> | undefined,
  fresh: ReadonlyMap<string, Reaction[]>,
): ReactionMap {
  const next: ReactionMap = new Map(previous ?? []);
  for (const [id, reactions] of fresh) {
    next.set(id, reactions);
  }
  return next;
}
