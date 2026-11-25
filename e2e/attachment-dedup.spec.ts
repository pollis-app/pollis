// Shared attachment objects (#2109): a v2 reference (`v: 2`, keyed by its own `sha`) opens like
// any attachment; one the operator removed reads "File removed" and is never retried; one from a
// newer app version reads "Update Pollis to open this file" and is never fetched. A v1 reference
// (`kv`) still opens beside them.

import fs from "node:fs";
import path from "node:path";
import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_dedup";
const GENERAL = "c_general";

const SHARED = "media/v2-shared.enc";
const LEGACY = "media/legacy.enc";
const REMOVED = "media/v2-removed.enc";
const NEWER = "media/v3-newer.enc";
const FIXTURES = path.join(__dirname, "fixtures");
const MEDIA_ORIGIN = "https://media.test";

const att = (key: string, name: string, keyed: Record<string, unknown>, ct = "image/jpeg") => ({
  key,
  url: "",
  name,
  ct,
  size: 4096,
  hash: key.replace("media/", "").replace(".enc", ""),
  sha: `s-${key}`,
  ...keyed,
});

const message = (id: string, atts: ReturnType<typeof att>[], at: string) => ({
  id,
  conversation_id: GENERAL,
  sender_id: "u_dana",
  sender_username: "dana",
  ciphertext: "",
  content: JSON.stringify({ _att: atts, _txt: "" }),
  sent_at: at,
});

type Skin = "modern" | "terminal";

async function boot(page: Page, skin: Skin) {
  const fetched: string[] = [];
  await page.route(`${MEDIA_ORIGIN}/**`, (route) => {
    fetched.push(route.request().url());
    route.fulfill({
      body: fs.readFileSync(path.join(FIXTURES, "sensitive-photo.jpg")),
      contentType: "image/jpeg",
    });
  });
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, {
    session: ME,
    preferences: { skin },
    profile: { id: ME.id, username: ME.username },
    groups: [
      { id: GROUP_ID, name: "dedup", owner_id: ME.id, created_at: "2026-01-01T00:00:00Z", current_user_role: "admin" },
    ],
    channels: { [GROUP_ID]: [{ id: GENERAL, group_id: GROUP_ID, name: "general" }] },
    groupMembers: {
      [GROUP_ID]: [
        { user_id: ME.id, username: "mia", role: "admin", joined_at: "2026-01-01T00:00:00Z" },
        { user_id: "u_dana", username: "dana", role: "member", joined_at: "2026-01-01T00:00:00Z" },
      ],
    },
    messages: {
      [GENERAL]: [
        message("m_shared", [att(SHARED, "shared.jpg", { v: 2 })], "2026-11-01T09:00:00Z"),
        message("m_legacy", [att(LEGACY, "legacy.jpg", { kv: 1 })], "2026-11-01T09:01:00Z"),
        message("m_removed", [att(REMOVED, "gone.jpg", { v: 2 })], "2026-11-01T09:02:00Z"),
        message("m_newer", [att(NEWER, "future.jpg", { v: 3 })], "2026-11-01T09:04:00Z"),
      ],
    },
    mediaUrls: {
      [SHARED]: `${MEDIA_ORIGIN}/shared.jpg`,
      [LEGACY]: `${MEDIA_ORIGIN}/legacy.jpg`,
      [NEWER]: `${MEDIA_ORIGIN}/newer.jpg`,
    },
    removedMediaKeys: [REMOVED],
    dmChannels: [],
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP_ID}`).click();
  await page.getByTestId(`channel-option-${GENERAL}`).click();
  await expect(page.getByTestId("message-input")).toBeVisible();
  return fetched;
}

const card = (page: Page, key: string) => page.getByTestId(`attachment-${key}`);

for (const skin of ["modern", "terminal"] as const) {
  test.describe(`shared attachment objects (${skin})`, () => {
    test("a v2 reference and a v1 reference both open", async ({ page }) => {
      await boot(page, skin);
      await expect(card(page, SHARED)).toHaveAttribute("data-media-state", "ready");
      await expect(card(page, LEGACY)).toHaveAttribute("data-media-state", "ready");
    });

    test("a removed file reads File removed and is not retried", async ({ page }) => {
      await boot(page, skin);
      const removed = card(page, REMOVED);
      await expect(removed).toHaveAttribute("data-media-state", "removed");
      await expect(removed).toContainText("File removed");
      await expect(removed).toContainText("gone.jpg");
      // Still removed a moment later: no retry loop flips it back to loading.
      await page.waitForTimeout(500);
      await expect(removed).toHaveAttribute("data-media-state", "removed");
    });

    test("a reference from a newer app reads update to open and is never fetched", async ({ page }) => {
      const fetched = await boot(page, skin);
      await expect(card(page, NEWER)).toHaveAttribute("data-media-state", "update");
      await expect(card(page, NEWER)).toContainText("Update Pollis to open this file");
      await page.waitForTimeout(300);
      expect(fetched.some((u) => u.includes("newer"))).toBe(false);
    });
  });
}
