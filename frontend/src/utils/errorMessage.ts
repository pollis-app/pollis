// Message from an unknown thrown value; Tauri rejects with a plain string (#1305). Else the fallback or `String(err)`.
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
