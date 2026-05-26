// Two-user DM round-trip: alice opens a DM to bob by username, bob
// accepts the resulting request, bob sends a reply, alice sees the
// reply. Both instances share the disposable test Turso; each has its
// own `POLLIS_DATA_DIR` so SQLite/keystore stay isolated.
//
// LiveKit realtime is not wired up under E2E (no LIVEKIT_URL in the
// env), so the usual realtime-push → poll_mls_welcomes path doesn't
// fire. We trigger explicit syncs via the `app.sync` keyboard command
// (Cmd/Ctrl+R) at each handover point. The sync handler in `AppShell`
// runs `poll_mls_welcomes` + `process_pending_commits` for every group
// + a queryClient.invalidateQueries() — exactly what the realtime
// listener would do on receive.

import { expect, test } from "@playwright/test";
import { signUpAndUnlock } from "./helpers/auth";
import { dispose, launchPollis, uniqueSuffix, type LaunchedApp } from "./helpers/launch";
import { wipeTestTurso } from "./helpers/turso";

let alice: LaunchedApp | undefined;
let bob: LaunchedApp | undefined;

test.beforeAll(async () => {
  await wipeTestTurso();
});

test.afterAll(async () => {
  if (alice !== undefined) {
    await dispose(alice);
  }
  if (bob !== undefined) {
    await dispose(bob);
  }
});

/** Read the auto-generated username from the User Settings page. We
 *  navigate via window.location since the router uses memory history
 *  (no URL bar). */
async function readUsername(handle: LaunchedApp): Promise<string> {
  const { page } = handle;
  await page.evaluate(() => window.location.assign("#/user"));
  await page.evaluate(() => window.dispatchEvent(new HashChangeEvent("hashchange")));
  // Memory history doesn't react to hash changes — use the router's
  // navigate via the global window reference our app exposes for
  // debugging (`__POLLIS_DEBUG__`) when present, else fall back to a
  // raw click on whatever Settings link is reachable. For now, use the
  // existing Cmd+K search path to reach /user reliably.
  await page.keyboard.press("Meta+k");
  await page.locator('input[placeholder*="Search"]').first().fill("User");
  await page.getByRole("option", { name: /User/i }).first().click();
  await expect(page.getByTestId("settings-page")).toBeVisible({ timeout: 15_000 });
  const username = await page.getByTestId("settings-username-input").inputValue();
  return username;
}

/** Trigger the app.sync keyboard command (Cmd+R / Ctrl+R) so the app
 *  re-polls MLS welcomes + pending commits and invalidates all
 *  TanStack Query caches. The default combo is the same on every
 *  platform; Playwright translates Meta on macOS, Control elsewhere. */
async function syncApp(handle: LaunchedApp): Promise<void> {
  const isMac = process.platform === "darwin";
  await handle.page.keyboard.press(isMac ? "Meta+r" : "Control+r");
}

test.fixme(
  "alice opens a DM to bob by username, bob accepts, replies, alice sees reply",
  async () => {
    // FIXME: this test still needs:
    //   1. A reliable way to navigate Alice's renderer to /dms/new (the
    //      Cmd+K Settings hop in `readUsername` is a stand-in; the real
    //      path is through the sidebar's "Start DM" affordance).
    //   2. Polling cadence tuning. The Rust integration harness drives
    //      `poll_mls_welcomes` + `process_pending_commits` between
    //      every handover; the `syncApp` helper below approximates that
    //      via Cmd+R, but the actual frame timing (welcome arrives →
    //      accept commit lands → first message envelope appears) needs
    //      a `toPass({ intervals: [...] })` loop, not a single
    //      `waitFor`.
    //   3. A `data-testid` on the StartDM page's username TextInput
    //      proper. Today we have `#dm-identifier` and the hidden
    //      `dm-identifier-input` mirror; both work but neither is the
    //      ergonomic Playwright selector.
    //
    // Scaffold below is the design sketch. Unfixme once the above are
    // tightened up — the framework underneath (launch helpers, Turso
    // wipe, two-instance pattern) is already in place.

    const aliceEmail = `alice-${uniqueSuffix()}@e2e.local`;
    const bobEmail = `bob-${uniqueSuffix()}@e2e.local`;

    // Spin both clients up in parallel so initialize_identity / KeyPackage
    // publish runs concurrently.
    [alice, bob] = await Promise.all([
      launchPollis({ devEmail: aliceEmail, dataDirLabel: `pollis-e2e-alice-${uniqueSuffix()}` }),
      launchPollis({ devEmail: bobEmail, dataDirLabel: `pollis-e2e-bob-${uniqueSuffix()}` }),
    ]);
    await Promise.all([signUpAndUnlock(alice), signUpAndUnlock(bob)]);

    const bobUsername = await readUsername(bob);
    expect(bobUsername.length).toBeGreaterThan(0);

    // Alice → Start DM → enter bob's username → submit.
    await alice.page.evaluate(() => window.location.assign("#/dms/new"));
    await expect(alice.page.getByTestId("start-dm-page")).toBeVisible({ timeout: 15_000 });
    await alice.page.locator("#dm-identifier").fill(bobUsername);
    await alice.page.getByTestId("start-dm-submit-button").click();

    // Bob: sync to catch the welcome, navigate to requests, accept.
    await syncApp(bob);
    await bob.page.evaluate(() => window.location.assign("#/dms/requests"));
    await expect(bob.page.getByTestId("requests-page")).toBeVisible({ timeout: 15_000 });
    const acceptButton = bob.page.locator('[data-testid^="accept-request-"]').first();
    await expect(acceptButton).toBeVisible({ timeout: 15_000 });
    await acceptButton.click();

    // Bob: open the conversation and reply.
    await syncApp(bob);
    const dmHeader = bob.page.getByTestId("dm-header-username");
    await expect(dmHeader).toBeVisible({ timeout: 15_000 });
    await bob.page.getByTestId("message-input").fill("hello from bob");
    await bob.page.keyboard.press("Enter");

    // Alice: sync, scroll into the conversation, see bob's reply.
    await syncApp(alice);
    const aliceDmHeader = alice.page.getByTestId("dm-header-username");
    await expect(aliceDmHeader).toBeVisible({ timeout: 15_000 });
    await expect(alice.page.getByTestId("message-list")).toContainText("hello from bob", {
      timeout: 15_000,
    });
  },
);
