// The Security events quick filters (#1548); the same table as desktop's, pinned by tests/security-events.test.ts.

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

export interface SecurityEventCursor {
  created_at: string;
  id: string;
}

export interface SecurityEvent {
  id: string;
  kind: string;
  device_id: string | null;
  created_at: string;
  metadata: string | null;
}

export interface SecurityEventsPage {
  events: SecurityEvent[];
  next_cursor: SecurityEventCursor | null;
}

// `kinds` is null for "all", keeping the pre-#1548 body shape.
export function securityEventsArgs(
  userId: string,
  limit: number,
  filter: SecurityEventFilter,
  before: SecurityEventCursor | null,
): { userId: string; limit: number; before: SecurityEventCursor | null; kinds: string[] | null } {
  return { userId, limit, before, kinds: SECURITY_EVENT_FILTERS[filter] };
}
