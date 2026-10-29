/**
 * When the renderer drops its React Query cache (#1256): it holds decrypted
 * message bodies and outlives AppShell. Lock lands on "pin-entry" and every
 * session end on "email-auth", after AppShell has unmounted.
 */
export const QUERY_CACHE_CLEARING_STATES: readonly string[] = ["pin-entry", "email-auth"];

export function clearsQueryCache(appState: string): boolean {
  return QUERY_CACHE_CLEARING_STATES.includes(appState);
}
