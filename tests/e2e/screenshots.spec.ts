// Visual smoke walk. Signs in once, then jumps to every static page via
// Cmd+K and saves a screenshot of each to `tests/e2e/screenshots/`.
// Filenames are deterministic, so a re-run overwrites the previous
// pass — the folder never grows.
//
// Not a regression test today: nothing compares against goldens. This
// is the "at-a-glance after a change" pass — open `tests/e2e/screenshots/`
// and look at the thumbnails. The directory is gitignored; flip the
// `.gitignore` line when you want to start tracking goldens.
//
// Why one big test instead of N: each Electron launch is a few seconds
// of cold-start; spinning up per page would 10× the run time. Order
// matters only to the extent that earlier failures abort later
// captures.

import { expect, test } from "@playwright/test";
import * as path from "node:path";
import { signUpAndUnlock } from "./helpers/auth";
import { dispose, launchPollis, uniqueSuffix, type LaunchedApp } from "./helpers/launch";
import { gotoPageViaCmdK } from "./helpers/navigate";
import { seedSoloContent } from "./helpers/seed";
import { wipeTestTurso } from "./helpers/turso";

const SCREENSHOT_DIR = path.resolve(__dirname, "screenshots");

/** One entry per page reachable from the keyboard. The `query` is the
 *  breadcrumb path from `PAGE_RESULTS` — Cmd+K matches via
 *  `.includes()` on name/breadcrumb/keywords, so the breadcrumb is the
 *  only field that's reliably unique across entries (names like
 *  "Settings" and "User" overlap several rows). The first hit wins on
 *  Enter, and breadcrumb ordering in PAGE_RESULTS matches our intent
 *  (Settings → Voice Settings → … ; Groups → Create Group → Find
 *  Groups). */
const PAGES: Array<{ file: string; query: string }> = [
  { file: "user-settings.png",   query: "/user" },
  { file: "settings-hub.png",    query: "/settings" },
  { file: "preferences.png",     query: "/preferences" },
  { file: "voice-settings.png",  query: "/settings/voice" },
  { file: "security.png",        query: "/security" },
  { file: "key-bindings.png",    query: "/shortcuts" },
  { file: "software-update.png", query: "/update" },
  { file: "invites.png",         query: "/invites" },
  { file: "join-requests.png",   query: "/join-requests" },
  { file: "dm-requests.png",     query: "/dms/requests" },
  { file: "dm-blocked.png",      query: "/dms/blocked" },
  { file: "dm-new.png",          query: "/dms/new" },
  { file: "groups.png",          query: "/groups" },
  { file: "groups-new.png",      query: "/groups/new" },
  { file: "groups-search.png",   query: "/groups/search" },
];

let alice: LaunchedApp | undefined;

test.beforeAll(async () => {
  await wipeTestTurso();
});

test.afterAll(async () => {
  if (alice !== undefined) {
    await dispose(alice);
  }
});

// Allow extra wall clock: ~15 pages × (nav + paint + screenshot) plus
// the cold-start signup. Wide envelope so the test doesn't flake on a
// slow Turso round-trip during one of the page mounts.
test("walks every static page and screenshots it", async () => {
  test.setTimeout(180_000);

  alice = await launchPollis({ devEmail: `screens-${uniqueSuffix()}@e2e.local` });
  const { page } = alice;
  await signUpAndUnlock(alice);

  // Pre-populate so the lists aren't empty. Solo content only (groups +
  // channels + messages on alice's side). Multi-user content (DMs,
  // inbound invites) requires a second Electron and is on the TODO.
  const seed = await seedSoloContent(page);

  // App shell on the root route — sidebar now shows the seeded groups +
  // unread channel hints; bottom bar shows the unread summary.
  await expect(page.getByTestId("app-ready")).toBeVisible();
  await page.screenshot({ path: path.join(SCREENSHOT_DIR, "00-app-ready.png") });

  // The Cmd+K search panel itself — open and centred on the page.
  // Captured before navigating anywhere so the result list is the
  // pristine PAGE_RESULTS + empty channel/voice/people set.
  const isMac = process.platform === "darwin";
  await page.keyboard.press(isMac ? "Meta+k" : "Control+k");
  await expect(page.getByTestId("search-panel-input")).toBeVisible();
  await page.screenshot({ path: path.join(SCREENSHOT_DIR, "01-search-panel.png") });
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("search-panel")).toBeHidden();

  for (const { file, query } of PAGES) {
    await gotoPageViaCmdK(page, query);
    // Short paint settle. Some routes mount React Query hooks that
    // fetch on mount; 300 ms covers the typical first paint without
    // making the suite drag. If a page consistently renders a loading
    // spinner in the screenshot, bump this per-entry rather than
    // globally.
    await page.waitForTimeout(300);
    await page.screenshot({ path: path.join(SCREENSHOT_DIR, file) });
  }

  // Seeded routes — dynamic IDs, so we navigate via the in-memory
  // router rather than Cmd+K. These are the "actual content" shots:
  // a group landing with its channel list, and a channel with the
  // messages we seeded into it.
  const firstGroup = seed.groups[0];
  const firstChannel = firstGroup?.channels[0];
  if (firstGroup !== undefined) {
    // The router uses memory history; window.location doesn't drive it.
    // The supported way to reach an arbitrary route from the renderer
    // is the sidebar, but seeded group links are deterministic — just
    // click the matching <a> by group name in the sidebar.
    await page
      .locator('[data-testid="sidebar"]')
      .getByText(firstGroup.name, { exact: false })
      .first()
      .click();
    await page.waitForTimeout(500);
    await page.screenshot({ path: path.join(SCREENSHOT_DIR, "group-landing.png") });

    if (firstChannel !== undefined) {
      await page
        .locator('[data-testid="sidebar"]')
        .getByText(firstChannel.name, { exact: false })
        .first()
        .click();
      // Channel load + render + scroll-to-bottom takes a tick longer
      // than the static settings pages.
      await page.waitForTimeout(800);
      await page.screenshot({ path: path.join(SCREENSHOT_DIR, "channel-with-messages.png") });
    }
  }
});
