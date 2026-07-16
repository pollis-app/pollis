// Extract a human-readable message from an unknown thrown value.
// Mirrors the ubiquitous `err instanceof Error ? err.message : <fallback>`
// ternary: returns the Error's message when possible, otherwise the caller's
// fallback string, or `String(err)` when no fallback is given.
export function errorMessage(err: unknown, fallback?: string): string {
  if (err instanceof Error) {
    return err.message;
  }
  if (fallback !== undefined) {
    return fallback;
  }
  return String(err);
}
