// Why a Vault save failed, as far as the user can act on it (#1497).
//
// The core's errors reach the renderer as text, so this reads the shapes
// `pollis-core` produces: a connection that failed or timed out, or a 5xx from
// the DS or R2, is "offline" (sending again is the fix); a file over the
// upload cap is "tooLarge" (sending again is not). Anything else stays
// generic. Mobile has the same rule in mobile/lib/vault.ts (`vaultSaveFailure`).
// Import-free on purpose, so frontend/tests/ runs it under plain node.

export type VaultSaveFailure = "offline" | "tooLarge" | "other";

export function vaultSaveFailure(err: unknown): VaultSaveFailure {
  // A Tauri rejection is the Rust error's text as a plain string.
  const text = err instanceof Error ? err.message : typeof err === "string" ? err : "";
  if (/the limit is \d+ bytes|content_length out of range|blob size out of range/i.test(text)) {
    return "tooLarge";
  }
  if (
    /error sending request|connection|timed out|dns error|network error/i.test(text) ||
    /\b5\d\d [A-Z][a-z]/.test(text)
  ) {
    return "offline";
  }
  return "other";
}
