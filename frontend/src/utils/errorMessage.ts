// A message from an unknown thrown value. Tauri rejects with the Rust error's
// text as a STRING (#1305), so a string is a message; else the Error's
// message, the fallback, or `String(err)`.
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
