// Bots (#1182): the bridge's wire types and the pure rules the Bots pages share. Desktop's twin is
// `frontend/src/` (lane D); the shapes are the lane contract's, camelCase as the core serializes them.

/** What `list_group_bots` returns per bot, and what `create_bot` returns. */
export interface BotSummary {
  userId: string;
  groupId: string;
  displayName: string;
  username: string;
  createdBy: string;
  createdAt: string;
  hostCount: number;
}

/** One machine running a bot: a device under the bot's user. */
export interface BotHost {
  deviceId: string;
  name: string;
  lastSeenAt: string | null;
  createdAt: string;
}

/** `get_bot`: the summary, its hosts, and the group's bot cap. */
export interface BotDetail extends BotSummary {
  hosts: BotHost[];
  groupBotCap: number;
}

/** The command an admin runs on the machine that will host the bot; it prints the pairing code. */
export const BOT_PAIR_COMMAND = "pollis-bot pair";

/** The command an admin runs on a further machine for an existing bot; it prints that host's code. */
export function botAddHostCommand(botUserId: string): string {
  return `pollis-bot add-host --bot ${botUserId}`;
}

/** Error codes the core puts at the start of a refused pairing (lane contract). */
const BOT_ERROR_KEYS: [prefix: string, key: string][] = [
  ["pairing_code_mismatch", "mobile:bots.errors.codeMismatch"],
  ["bot_cap_reached", "mobile:bots.errors.capReached"],
  ["pairing_request_expired", "mobile:bots.errors.expired"],
];

/**
 * The catalogue key for a refused pairing, or null for any other failure (whose message the core
 * already wrote for people). Matched on the message's start, after a uniffi prefix if one is left.
 */
export function botErrorKey(message: string | null | undefined): string | null {
  if (!message) {
    return null;
  }
  const bare = message.replace(/^BridgeError\.\w+:\s*/, "").trim();
  for (const [prefix, key] of BOT_ERROR_KEYS) {
    if (bare.startsWith(prefix)) {
      return key;
    }
  }
  return null;
}

/** A user row as the roster, an author or a mention carries it; old cores send no flag. */
export interface MaybeBot {
  is_bot?: boolean | null;
  isBot?: boolean | null;
}

/** Whether a roster row is a bot. Absent means human: an old core never sends the flag. */
export function isBotUser(row: MaybeBot | null | undefined): boolean {
  return !!(row && (row.is_bot === true || row.isBot === true));
}

/** The ids of the bots in a roster, for tagging their messages and mentions. */
export function botIds<T extends MaybeBot & { user_id: string }>(rows: readonly T[]): ReadonlySet<string> {
  const out = new Set<string>();
  for (const row of rows) {
    if (isBotUser(row)) {
      out.add(row.user_id);
    }
  }
  return out;
}

/** The lowercased handles of the bots in a roster, as a message body's mentions name them. */
export function botHandles<T extends MaybeBot & { username?: string | null }>(
  rows: readonly T[],
): ReadonlySet<string> {
  const out = new Set<string>();
  for (const row of rows) {
    if (isBotUser(row) && row.username) {
      out.add(row.username.toLowerCase());
    }
  }
  return out;
}

/** True once the group holds as many bots as its cap allows; an unknown cap never blocks. */
export function botCapReached(count: number, cap: number | null | undefined): boolean {
  return typeof cap === "number" && cap > 0 && count >= cap;
}

/** Revoking this host leaves the bot with none, so it can never run again. */
export function isLastHost(hosts: readonly BotHost[], deviceId: string): boolean {
  return hosts.length === 1 && hosts[0].deviceId === deviceId;
}
