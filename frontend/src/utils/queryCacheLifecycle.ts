/** App states that clear the query cache of decrypted bodies (#1256): lock and every session end. */
export const QUERY_CACHE_CLEARING_STATES: readonly string[] = ["pin-entry", "email-auth"];

export function clearsQueryCache(appState: string): boolean {
  return QUERY_CACHE_CLEARING_STATES.includes(appState);
}
