// The Security events screen's quick filters (#1548), as the `kinds` each one
// sends to the DS. `all` sends none. The same table as desktop's
// `frontend/src/components/Security/securityEvents.ts`; `tests/security-events.test.ts`
// fails if the two drift.

export type SecurityEventFilter = "all" | "devices" | "identity" | "account";

export const SECURITY_EVENT_FILTERS: Record<SecurityEventFilter, string[] | null> = {
  all: null,
  devices: ["device_enrolled", "device_rejected", "device_revoked"],
  identity: [
    "identity_reset",
    "identity_rotated",
    "secret_key_rotated",
    "recovery_blob_fetched",
  ],
  account: ["email_changed"],
};

export const SECURITY_EVENT_FILTER_ORDER: SecurityEventFilter[] = [
  "all",
  "devices",
  "identity",
  "account",
];

// Mirrors `SecurityEventCursor` in pollis-api `account_reads.rs`.
export interface SecurityEventCursor {
  created_at: string;
  id: string;
}

// Mirrors `SecurityEvent` in pollis-core/src/commands/device_enrollment.rs.
export interface SecurityEvent {
  id: string;
  kind: string;
  device_id: string | null;
  created_at: string;
  metadata: string | null;
}

// Mirrors `SecurityEventsPage` in pollis-core/src/commands/device_enrollment.rs.
export interface SecurityEventsPage {
  events: SecurityEvent[];
  next_cursor: SecurityEventCursor | null;
}

// The `list_security_events` arguments for one page: `kinds` is omitted (null)
// for "all" so the DS body stays the pre-#1548 shape.
export function securityEventsArgs(
  userId: string,
  limit: number,
  filter: SecurityEventFilter,
  before: SecurityEventCursor | null,
): { userId: string; limit: number; before: SecurityEventCursor | null; kinds: string[] | null } {
  return { userId, limit, before, kinds: SECURITY_EVENT_FILTERS[filter] };
}
