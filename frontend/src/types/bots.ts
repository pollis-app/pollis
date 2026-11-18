// Bots (#1182): the bridge types of `list_group_bots`, `get_bot`, `create_bot`,
// `add_bot_host`. Twin: `mobile/types/bots.ts`; Rust: `pollis-core/src/commands/bots/admin.rs`.

export type BotSummary = {
  userId: string;
  groupId: string;
  displayName: string;
  username: string;
  /** Empty once the creating admin's account is gone. */
  createdBy: string;
  createdAt: string;
  /** Hosts not revoked. */
  hostCount: number;
};

export type BotHost = {
  deviceId: string;
  name: string;
  lastSeenAt: string | null;
  createdAt: string;
};

export type BotDetail = BotSummary & { hosts: BotHost[]; groupBotCap: number };

/** Error-string prefixes the Bots pages tell apart. */
export const BOT_ERROR_PREFIX = {
  codeMismatch: "pairing_code_mismatch",
  capReached: "bot_cap_reached",
  requestExpired: "pairing_request_expired",
} as const;
