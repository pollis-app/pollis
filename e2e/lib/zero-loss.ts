// Zero message loss (#1524): fails a test ending on an undecryptable placeholder unless it called expectLosses().

import { test as base, expect } from "@playwright/test";

export * from "@playwright/test";

const EXPECTS_LOSS = "expects-loss";

/** Opt the running test out of the zero-loss check. */
export function expectLosses(): void {
  base.info().annotations.push({ type: EXPECTS_LOSS });
}

export const test = base.extend<{ zeroLoss: void }>({
  zeroLoss: [
    async ({ page }, use, testInfo) => {
      await use();
      if (testInfo.annotations.some((a) => a.type === EXPECTS_LOSS)) {
        return;
      }
      // A test that already failed has said what went wrong.
      if (testInfo.status !== testInfo.expectedStatus || page.isClosed()) {
        return;
      }
      const shown = await page.locator('[data-undecryptable="true"]').count();
      expect(
        shown,
        "zero-loss invariant (#1524): the page shows an undecryptable placeholder this test did " +
          "not set out to create — call expectLosses() in a test that does",
      ).toBe(0);
    },
    { auto: true },
  ],
});
