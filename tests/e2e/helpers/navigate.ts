// Cmd/Ctrl+K navigation — the same surface a keyboard-driven user uses
// to jump between pages. Reaching every static page through one
// affordance keeps tests independent of sidebar tree changes and
// mirrors the contract documented in `CLAUDE.md` ("New static pages
// must be registered in three places ... PAGE_RESULTS").

import { expect, type Page } from "@playwright/test";

/** Open the search panel, type a query, press Enter to select the top
 *  hit. Resolves once the panel closes. Use the `name` from
 *  `PAGE_RESULTS` (or a unique prefix) for predictable matches. */
export async function gotoPageViaCmdK(page: Page, query: string): Promise<void> {
  // The shortcut is `mod+k` (Cmd on macOS, Ctrl elsewhere). Playwright
  // already maps `Meta` to Cmd on darwin and Ctrl on win/linux when the
  // page key combos are written that way.
  const isMac = process.platform === "darwin";
  await page.keyboard.press(isMac ? "Meta+k" : "Control+k");
  const input = page.getByTestId("search-panel-input");
  await expect(input).toBeVisible({ timeout: 5_000 });
  await input.fill(query);
  // Wait for the panel to have at least one result before pressing
  // Enter; otherwise we race the filter and Enter falls on an empty
  // list (no-op).
  await expect(page.getByTestId("search-panel-result-item").first()).toBeVisible({ timeout: 5_000 });
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("search-panel")).toBeHidden({ timeout: 5_000 });
}
