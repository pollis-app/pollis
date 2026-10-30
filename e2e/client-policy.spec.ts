// A build below the published minimum (#1611) gets one full-window page: what happened, Update
// now (the Tauri updater) with the download page as the fallback, a package-manager install's
// command, and Check again. Blocked at launch, on window focus, and on a DS 426 mid-session.
// The policy is a test override (`clientPolicy` preload / `__tauriSetClientPolicy`), the updater
// a scripted mock (`__POLLIS_UPDATER__`).

import { test, expect, type Page } from "./lib/zero-loss";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };
const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0XCB";

const BLOCKED = {
  blocked: true,
  platform: "macos",
  version: "1.16.0",
  minimum: "1.17.0",
  message: null,
  source: "network",
};

type Updater = "none" | "succeed" | "fail";

async function boot(
  page: Page,
  opts: { clientPolicy?: Record<string, unknown>; updater?: Updater; managedInstall?: unknown } = {},
) {
  await page.addInitScript(
    ({ preload, updater }) => {
      const w = window as unknown as Record<string, unknown>;
      w.__POLLIS_PRELOAD__ = preload;
      w.__POLLIS_UPDATER__ = updater;
      w.__TAURI_INTERNALS__ = {
        metadata: {
          currentWindow: { label: "main" },
          currentWebview: { windowLabel: "main", label: "main" },
        },
        plugins: {},
        transformCallback: () => 0,
        convertFileSrc: (path: string) => path,
        registerListener: () => {},
        unregisterListener: () => {},
        runCallback: () => {},
        invoke: () => Promise.resolve(null),
      };
    },
    {
      preload: {
        session: USER,
        profile: { id: USER.id, username: USER.username },
        groups: [{ id: GROUP_ID, name: "Acme", owner_id: USER.id, created_at: new Date().toISOString() }],
        channels: { [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }] },
        clientPolicy: opts.clientPolicy,
        managedInstall: opts.managedInstall,
      },
      updater: opts.updater ?? "none",
    },
  );
  await page.goto("/");
}

async function expectOnlyTheBlockedPage(page: Page) {
  await expect(page.getByTestId("client-blocked-screen")).toBeVisible();
  // No way around it: no shell, no unlock, no sign-in, and no modal.
  await expect(page.getByTestId("sidebar")).toHaveCount(0);
  await expect(page.getByTestId("auth-screen")).toHaveCount(0);
  await expect(page.getByRole("dialog")).toHaveCount(0);
}

test("blocked at launch: the page says what happened, before the app starts", async ({ page }) => {
  await boot(page, { clientPolicy: BLOCKED });
  await expectOnlyTheBlockedPage(page);
  await expect(page.getByText("Update required")).toBeVisible();
  await expect(page.getByTestId("client-blocked-message")).toContainText("no longer supported");
  await expect(page.getByTestId("client-blocked-versions")).toHaveText("Installed 1.16.0 · Required 1.17.0 or later");
  const counts = await page.evaluate(
    () => (window as unknown as { __tauriInvokeCounts: Record<string, number> }).__tauriInvokeCounts,
  );
  // The check ran first; nothing after it did.
  expect(counts["check_client_policy"]).toBe(1);
  expect(counts["get_session"] ?? 0).toBe(0);
  expect(counts["get_unlock_state"] ?? 0).toBe(0);
});

test("the manifest's message replaces the built-in text", async ({ page }) => {
  await boot(page, { clientPolicy: { ...BLOCKED, message: "Version 1.16 can lose drafts. Please update." } });
  await expect(page.getByTestId("client-blocked-message")).toHaveText("Version 1.16 can lose drafts. Please update.");
});

test("Update now runs the updater and relaunches", async ({ page }) => {
  await boot(page, { clientPolicy: BLOCKED, updater: "succeed" });
  await expectOnlyTheBlockedPage(page);
  const update = page.getByTestId("client-blocked-update");
  await update.click();
  // Busy on the same frame, and it cannot be pressed twice.
  await expect(update).toBeDisabled();
  await expect(page.getByTestId("client-blocked-retry")).toBeDisabled();
  await expect
    .poll(() => page.evaluate(() => (window as unknown as { __pollisRelaunched?: number }).__pollisRelaunched ?? 0))
    .toBe(1);
  await expect(page.getByTestId("client-blocked-download")).toHaveCount(0);
});

for (const [updater, why] of [
  ["fail", "the download fails"],
  ["none", "the updater finds nothing"],
] as const) {
  test(`when ${why}, the download page is the fallback`, async ({ page }) => {
    await boot(page, { clientPolicy: BLOCKED, updater });
    await page.getByTestId("client-blocked-update").click();
    await expect(page.getByTestId("client-blocked-update-failed")).toContainText("didn't finish");
    await expect(page.getByTestId("client-blocked-update")).toHaveCount(0);
    await page.getByTestId("client-blocked-download").click();
    await expect
      .poll(() => page.evaluate(() => (window as unknown as { __pollisOpened?: string[] }).__pollisOpened ?? []))
      .toEqual(["https://pollis.com/#download"]);
  });
}

test("an AUR install gets its package-manager command, not Update now", async ({ page }) => {
  await boot(page, {
    clientPolicy: { ...BLOCKED, platform: "linux" },
    managedInstall: { kind: "aur", display_name: "the AUR (Arch User Repository)", update_command: "yay -S pollis" },
  });
  await expectOnlyTheBlockedPage(page);
  await expect(page.getByText("Pollis was installed with the AUR (Arch User Repository). Update it there:")).toBeVisible();
  await expect(page.getByTestId("client-blocked-command")).toContainText("yay -S pollis");
  await expect(page.getByTestId("client-blocked-update")).toHaveCount(0);
});

test("Check again: a fresh policy that allows this build carries on with the launch", async ({ page }) => {
  await boot(page, { clientPolicy: BLOCKED });
  await expectOnlyTheBlockedPage(page);
  await page.evaluate(() => {
    (window as unknown as { __tauriSetClientPolicy: (s: unknown) => void }).__tauriSetClientPolicy({ blocked: false });
  });
  await page.getByTestId("client-blocked-retry").click();
  await expect(page.getByTestId("sidebar")).toBeVisible();
  await expect(page.getByTestId("client-blocked-screen")).toHaveCount(0);
});

test("Check again while still blocked stays on the page", async ({ page }) => {
  await boot(page, { clientPolicy: BLOCKED });
  await page.evaluate(() => {
    (window as unknown as { __tauriSetClientPolicy: (s: unknown) => void }).__tauriSetClientPolicy({
      blocked: true,
      version: "1.16.0",
      minimum: "1.18.0",
      source: "network",
    });
  });
  await page.getByTestId("client-blocked-retry").click();
  await expect(page.getByTestId("client-blocked-versions")).toHaveText("Installed 1.16.0 · Required 1.18.0 or later");
  await expect(page.getByTestId("client-blocked-retry")).toBeEnabled();
});

test("blocked while running: regaining focus re-checks", async ({ page }) => {
  await boot(page);
  await expect(page.getByTestId("sidebar")).toBeVisible();
  await page.evaluate(() => {
    (window as unknown as { __tauriSetClientPolicy: (s: unknown) => void }).__tauriSetClientPolicy({
      blocked: true,
      version: "1.16.0",
      minimum: "1.17.0",
      source: "network",
    });
    window.dispatchEvent(new Event("focus"));
  });
  await expectOnlyTheBlockedPage(page);
});

test("blocked while running: a DS 426 on any command shows the page", async ({ page }) => {
  await boot(page);
  await expect(page.getByTestId("sidebar")).toBeVisible();
  await page.evaluate(() => {
    const w = window as unknown as {
      __tauriSetClientPolicy: (s: unknown) => void;
      __tauriRejectWith: (c: string, m: string) => void;
    };
    // What the core recorded from the 426 body.
    w.__tauriSetClientPolicy({ blocked: true, version: "1.16.0", minimum: "1.17.0", source: "ds" });
    w.__tauriRejectWith("read_channel_messages", "upgrade_required");
  });
  // Opening the channel reads its messages: that read is refused.
  await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
  await page.getByTestId("search-panel-input").fill("general");
  await page.keyboard.press("Enter");
  await expectOnlyTheBlockedPage(page);
  await expect(page.getByTestId("client-blocked-versions")).toHaveText("Installed 1.16.0 · Required 1.17.0 or later");
});

test("an allowed build never sees the page", async ({ page }) => {
  await boot(page, { clientPolicy: { blocked: false, version: "1.17.0", minimum: "1.17.0", source: "network" } });
  await expect(page.getByTestId("sidebar")).toBeVisible();
  await expect(page.getByTestId("client-blocked-screen")).toHaveCount(0);
});
