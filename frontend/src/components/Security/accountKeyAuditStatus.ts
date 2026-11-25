/* The account-key line's display state (#2114): the audit's four verdicts, plus `checking`
   while the audit runs. Every outcome maps to a line, so the slot is never empty.
   Pure and import-free: mobile/lib/accountKeyAuditStatus.ts is this file byte for byte. */

/** Mirrors Rust `AuditStatus` (`pollis-core/src/commands/transparency.rs`). */
export type AuditVerdict = "ok" | "pending" | "alarm" | "unavailable";

/** `checking` is display-only and never part of the wire enum. */
export type AccountKeyDisplayStatus = AuditVerdict | "checking";

/** The parts of a React Query result the mapping reads. */
export interface AuditQueryState {
  isPending: boolean;
  isError: boolean;
  data?: { status: AuditVerdict } | null;
}

/** Each state's label, in the `settings` namespace; `tests/account-key-audit.test.ts` holds them to the catalogue. */
export const ACCOUNT_KEY_LABEL_KEYS: Record<AccountKeyDisplayStatus, string> = {
  checking: "security.accountKeyStatusChecking",
  ok: "security.accountKeyStatusOk",
  pending: "security.accountKeyStatusPending",
  alarm: "security.accountKeyStatusAlarm",
  unavailable: "security.accountKeyStatusUnavailable",
};

export function accountKeyDisplayStatus(query: AuditQueryState): AccountKeyDisplayStatus {
  if (query.data) {
    return query.data.status;
  }
  // A failed audit is "couldn't check", the same as the core's own `unavailable`.
  if (query.isError) {
    return "unavailable";
  }
  if (query.isPending) {
    return "checking";
  }
  return "unavailable";
}
