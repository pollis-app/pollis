/** What a person row shows (#1971). */
export interface PersonName {
  // The display name when one is set, else the handle, else the fallback.
  name: string;
  // True when `name` is a handle: one line, truncated in the middle so both ends stay readable.
  isHandle: boolean;
}

function nonEmpty(value: string | null | undefined): string | null {
  const trimmed = value?.trim();
  return trimmed ? trimmed : null;
}

/** Display name first, then the handle (`@` prefixed when `at`), then `fallback`. */
export function personName(
  person: { username?: string | null; preferred_name?: string | null },
  fallback: string,
  { at = true }: { at?: boolean } = {},
): PersonName {
  const preferred = nonEmpty(person.preferred_name);
  if (preferred) {
    return { name: preferred, isHandle: false };
  }
  const username = nonEmpty(person.username);
  if (username) {
    return { name: at ? `@${username}` : username, isHandle: true };
  }
  return { name: fallback, isHandle: false };
}

/** Row actions sit under the name, not beside it, when the row is narrower than this (pt). */
export const STACK_ACTIONS_BELOW = 440;

/** Whether a row this wide puts its actions on their own line under the name. */
export function stackRowActions(rowWidth: number, fontScale = 1): boolean {
  return rowWidth * Math.min(1, 1 / Math.max(fontScale, 0.5)) < STACK_ACTIONS_BELOW;
}
