// A device revoked while offline signs itself out (#1338), leaving no decrypted message on screen.

import { test, expect, type Page } from "./lib/zero-loss";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };
const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0XCB";
const MESSAGE_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0MSG";
const SECRET = "history that must not stay on a removed device";

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

function preloadState(skin: Skin) {
  return {
    session: USER,
    profile: { id: USER.id, username: USER.username },
    preferences: JSON.stringify({ skin }),
    groups: [{ id: GROUP_ID, name: "Acme", owner_id: USER.id, created_at: new Date().toISOString() }],
    channels: { [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }] },
    messages: {
      [CHANNEL_ID]: [
        {
          id: MESSAGE_ID,
          conversation_id: CHANNEL_ID,
          sender_id: "u-bob",
          content: SECRET,
          sent_at: "2026-08-01T10:05:00.000Z",
        },
      ],
    },
  };
}

async function boot(page: Page, skin: Skin) {
  await page.addInitScript((preload) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = preload;
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {
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
  }, preloadState(skin));
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
}

async function openCommandPalette(page: Page) {
  await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
  await expect(page.getByTestId("search-panel")).toBeVisible();
}

for (const skin of SKINS) {
  test(`${skin}: a revoked device lands on sign-in and says why`, async ({ page }) => {
    await boot(page, skin);

    await openCommandPalette(page);
    await page.getByTestId("search-panel-input").fill("general");
    await page.keyboard.press("Enter");
    await expect(page.getByTestId(`message-${MESSAGE_ID}`)).toBeVisible();

    // Revoked from another device while this one was not listening: no nudge,
    // just every command refused from now on.
    await page.evaluate(() => {
      (window as unknown as { __tauriRevokeDevice: () => void }).__tauriRevokeDevice();
    });

    // Whichever command runs next finds out; keys only, so this cannot race the sign-out.
    await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
    await page
      .getByTestId("search-panel-input")
      .pressSequentially("Security", { timeout: 5000 })
      .catch(() => {
        // Already signed out: the palette went with the shell.
      });
    await page.keyboard.press("Enter");

    await expect(page.getByTestId("auth-screen")).toBeVisible();
    await expect(page.getByTestId("auth-session-notice")).toContainText("removed from your account");
    await expect(page.getByText(SECRET)).toHaveCount(0);
    // The core decides the wipe from its own record of which device was
    // revoked; the app only signs out, and never asks for a wipe itself.
    const asked = await page.evaluate(
      () => (window as unknown as { __tauriMock: { lastLogoutDeleteData: boolean | null } }).__tauriMock.lastLogoutDeleteData,
    );
    expect(asked, "the app signed out").toBe(false);
  });
}

for (const skin of SKINS) {
  test(`${skin}: a stray device_revoked rejection never signs a healthy device out`, async ({ page }) => {
    await boot(page, skin);

    // A rejection that says device_revoked while the core says this device is
    // NOT revoked — e.g. one that landed after the session it belonged to.
    await page.evaluate(() => {
      (window as unknown as { __tauriRejectWith: (c: string, m: string) => void }).__tauriRejectWith(
        "read_channel_messages",
        "device_revoked",
      );
    });
    // Opening the channel reads its messages: that read is refused.
    await openCommandPalette(page);
    await page.getByTestId("search-panel-input").fill("general");
    await page.keyboard.press("Enter");
    await page.waitForTimeout(500);

    const searches = await page.evaluate(
      () => (window as unknown as { __tauriInvokeCounts: Record<string, number> }).__tauriInvokeCounts["read_channel_messages"] ?? 0,
    );
    expect(searches, "control: the rejected command really ran").toBeGreaterThan(0);
    await expect(page.getByTestId("sidebar")).toBeVisible();
    await expect(page.getByTestId("auth-screen")).toHaveCount(0);
    const loggedOut = await page.evaluate(
      () => (window as unknown as { __tauriMock: { lastLogoutDeleteData: boolean | null } }).__tauriMock.lastLogoutDeleteData,
    );
    expect(loggedOut, "no sign-out happened").toBeNull();
  });
}
