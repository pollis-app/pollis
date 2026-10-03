import { test } from "@playwright/test";

/**
 * The skin a skin-agnostic test registers in. Terminal is the default skin.
 */
export const AGNOSTIC_SKIN = "terminal";

/**
 * `test`, registered in ONE skin of a per-skin loop and skipped (not
 * registered at all) in the other.
 *
 * Only for a test whose code path has no skin branch AND whose assertions are
 * behavioural — invoke arguments and counts, mock state, routes, text — so the
 * second skin's run repeats identical work. Anything that measures paint or
 * geometry, saves a per-skin screenshot, reaches its target through markup
 * that differs per skin, or exercises a component the skins mount differently
 * stays a plain `test` and keeps running in both.
 */
export function skinAgnosticTest(skin: string): typeof test {
  if (skin === AGNOSTIC_SKIN) {
    return test;
  }
  return (() => {}) as unknown as typeof test;
}
