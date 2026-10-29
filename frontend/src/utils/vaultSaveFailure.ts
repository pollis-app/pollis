// Why a Vault save failed (#1497): offline (retry helps), tooLarge, or other. Mirrors mobile/lib/vault.ts; import-free.

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
