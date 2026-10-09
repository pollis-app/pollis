/**
 * When the renderer drops its React Query cache (#1256).
 *
 * The cache holds decrypted message bodies and is module-level, so it outlives
 * AppShell. It used to be cleared only on the way into "pin-entry" (a lock);
 * signing out, deleting the account or being signed out as a revoked device
 * left the previous session's plaintext in the heap. Every one of those lands
 * on "email-auth", so clearing on that transition covers them all at one
 * point, after AppShell has unmounted and no observer is left to refetch.
 */
export const QUERY_CACHE_CLEARING_STATES: readonly string[] = ["pin-entry", "email-auth"];

export function clearsQueryCache(appState: string): boolean {
  return QUERY_CACHE_CLEARING_STATES.includes(appState);
}
