// Extract a human-readable message from an unknown thrown value.
//
// A Tauri command rejects with the Rust error's text as a plain STRING, not an
// `Error` (pollis-core's `Error` serializes to its message), so a string is a
// message in its own right. Before #1305 it fell through to the caller's
// fallback, which is how every sign-in failure — a network error, a 5xx, a
// keystore error after the code was accepted — rendered as "Invalid code".
//
// Returns the Error's message or the rejection string when there is one,
// otherwise the caller's fallback, or `String(err)` when no fallback is given.
export function errorMessage(err: unknown, fallback?: string): string {
  if (err instanceof Error && err.message) {
    return err.message;
  }
  if (typeof err === "string" && err.trim()) {
    return err;
  }
  if (fallback !== undefined) {
    return fallback;
  }
  return String(err);
}
