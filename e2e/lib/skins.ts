import { test } from "@playwright/test";

export const AGNOSTIC_SKIN = "terminal";

/** `test` registered in one skin only: for behavioural tests with no skin branch. */
export function skinAgnosticTest(skin: string): typeof test {
  if (skin === AGNOSTIC_SKIN) {
    return test;
  }
  return (() => {}) as unknown as typeof test;
}
