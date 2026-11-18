/*
 * Per-group aliases (#2003): inside a group, a member may show a display name and
 * avatar other than their main profile's. Resolved at render, so old messages pick
 * up a new alias too. A missing alias, or a null field, falls back to whatever the
 * surface showed before. Mentions stay `@handle` in message text and on the wire;
 * only their rendering uses the alias. Mirrored in `mobile/lib/groupAlias.ts`.
 */

/** One alias, as `list_group_aliases` / `list_my_aliases` return it (Rust `GroupAlias`). */
export interface GroupAlias {
  group_id: string;
  user_id: string;
  display_name: string | null;
  avatar_url: string | null;
}

/** A group's aliases by user id. */
export type AliasMap = ReadonlyMap<string, GroupAlias>;

export const EMPTY_ALIASES: AliasMap = new Map();

export function toAliasMap(list: readonly GroupAlias[] | undefined): AliasMap {
  if (!list || list.length === 0) {
    return EMPTY_ALIASES;
  }
  return new Map(list.map((a) => [a.user_id, a]));
}

/** The name `userId` shows in this group: their alias name, else `fallback`. */
export function aliasName(aliases: AliasMap | undefined, userId: string, fallback: string): string;
export function aliasName(
  aliases: AliasMap | undefined,
  userId: string,
  fallback: string | null | undefined,
): string | null | undefined;
export function aliasName(
  aliases: AliasMap | undefined,
  userId: string,
  fallback: string | null | undefined,
): string | null | undefined {
  return aliases?.get(userId)?.display_name || fallback;
}

/** The avatar key `userId` shows in this group: their alias picture, else `fallback`. */
export function aliasAvatar(
  aliases: AliasMap | undefined,
  userId: string,
  fallback: string | null | undefined,
): string | null | undefined {
  return aliases?.get(userId)?.avatar_url || fallback;
}

/** True when `userId` has an alias in this group (either field). */
export function hasAlias(aliases: AliasMap | undefined, userId: string): boolean {
  const a = aliases?.get(userId);
  return !!a && (!!a.display_name || !!a.avatar_url);
}

/**
 * Alias names by lowercased handle, for rendering `@handle` mentions: `members`
 * supplies each user id's handle.
 */
export function mentionLabels(
  aliases: AliasMap | undefined,
  members: ReadonlyArray<{ user_id: string; username?: string | null }>,
): ReadonlyMap<string, string> {
  const out = new Map<string, string>();
  if (!aliases || aliases.size === 0) {
    return out;
  }
  for (const m of members) {
    const name = aliases.get(m.user_id)?.display_name;
    if (name && m.username) {
      out.set(m.username.toLowerCase(), name);
    }
  }
  return out;
}

/** Longest alias name, as the DS's `DISPLAY_NAME_MAX_LEN`. */
const ALIAS_NAME_MAX = 100;

/**
 * Whether `name` (already trimmed, non-empty) passes the DS's display-name rule:
 * at most 100 characters, no control characters, no bidi overrides, no `<` or `>`.
 */
export function isValidAliasName(name: string): boolean {
  if (name.length === 0 || [...name].length > ALIAS_NAME_MAX) {
    return false;
  }
  // eslint-disable-next-line no-control-regex
  return !/[\u0000-\u001f\u007f-\u009f‪-‮⁦-⁩‎‏<>]/.test(name);
}

/**
 * What a "save" writes for one field: `undefined` leaves it, `""` clears it (back
 * to the main profile), anything else sets it. `next` is the trimmed input;
 * `current` the alias as stored.
 */
export function fieldWrite(
  next: string | null,
  current: string | null | undefined,
): string | undefined {
  const want = next ?? "";
  const have = current ?? "";
  if (want === have) {
    return undefined;
  }
  return want;
}
