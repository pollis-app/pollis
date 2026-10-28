/*
 * Zero message loss, desktop UI tier (#1524).
 *
 * Every spec imports `test` from here instead of `@playwright/test`. An
 * automatic fixture fails any test whose page still shows an undecryptable
 * placeholder (`[data-undecryptable="true"]`, `UndecryptableMessage.tsx`) when
 * it ends, unless the test set out to show one and said so with
 * `expectLosses()`.
 *
 * What it can and cannot catch: these specs run the browser build against the
 * IPC mock, so no real MLS runs here and a placeholder only appears when a spec
 * (or the mock) seeds one. The check stops a spec from rendering a loss it did
 * not mean to — the real decrypt path is proved loss-free by the Rust flows
 * suite (`src-tauri/tests/flows/zero_loss.rs`).
 */

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
