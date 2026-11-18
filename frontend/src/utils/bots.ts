/*
 * Bots (#1182): a bot is a user scoped to one group, managed by that group's admins.
 * Pure helpers shared by the Bots pages, the member list, messages and mentions.
 */

/**
 * A user record that may be a bot. The core serializes member rows in snake_case
 * (`is_bot`) and the bot commands in camelCase (`isBot`); an old core sends neither,
 * which reads as a person.
 */
export interface MaybeBot {
  is_bot?: boolean | null;
  isBot?: boolean | null;
}

/** Whether `user` is a bot account. The one place the field is read. */
export function isBotUser(user: MaybeBot | null | undefined): boolean {
  return !!(user?.is_bot ?? user?.isBot);
}

/** The ids of the bots among `members`. */
export function botIdSet(members: ReadonlyArray<MaybeBot & { user_id: string }>): ReadonlySet<string> {
  const out = new Set<string>();
  for (const m of members) {
    if (isBotUser(m)) {
      out.add(m.user_id);
    }
  }
  return out;
}

/** The lowercased handles of the bots among `members`, for `@mention` tokens. */
export function botHandleSet(
  members: ReadonlyArray<MaybeBot & { username?: string | null }>,
): ReadonlySet<string> {
  const out = new Set<string>();
  for (const m of members) {
    if (isBotUser(m) && m.username) {
      out.add(m.username.toLowerCase());
    }
  }
  return out;
}

/**
 * Bots one group may have: mirrors `MAX_BOTS_PER_GROUP` in `pollis-api/src/bots.rs`
 * (`frontend/tests/bots.test.ts` pins the two). The DS enforces it; this only lets
 * the Bots page say so before an admin pairs a host for nothing.
 */
export const GROUP_BOT_CAP = 25;

/** The command an admin runs on the machine that will host a new bot. */
export const BOT_PAIR_COMMAND = "pollis-bot pair";

/** The command an admin runs on a further machine for an existing bot. */
export function botAddHostCommand(botUserId: string): string {
  return `${BOT_PAIR_COMMAND} --bot ${botUserId}`;
}

/** The refusals the Bots pages explain in their own words; the core's error strings start with these. */
export type BotErrorKind = "codeMismatch" | "capReached" | "requestExpired";

const BOT_ERROR_PREFIXES: ReadonlyArray<[string, BotErrorKind]> = [
  ["pairing_code_mismatch", "codeMismatch"],
  ["bot_cap_reached", "capReached"],
  ["pairing_request_expired", "requestExpired"],
];

/** Which known refusal `err` is, or null for anything else (shown as the core reports it). */
export function botErrorKind(err: unknown): BotErrorKind | null {
  const message =
    err instanceof Error ? err.message : typeof err === "string" ? err : "";
  for (const [prefix, kind] of BOT_ERROR_PREFIXES) {
    if (message.startsWith(prefix)) {
      return kind;
    }
  }
  return null;
}
