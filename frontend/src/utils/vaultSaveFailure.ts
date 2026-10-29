// Why a Vault save failed, by what the user can do (#1497): connection
// failures and DS / R2 5xx are "offline" (retry helps), over the upload cap is
// "tooLarge" (it doesn't), else generic. Same rule as mobile/lib/vault.ts.
// Import-free so frontend/tests runs it under plain node.

export type VaultSaveFailure = "offline" | "tooLarge" | "other";

export function vaultSaveFailure(err: unknown): VaultSaveFailure {
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
