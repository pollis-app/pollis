/*
 * On-device export (#856) — the renderer's half, in both skins.
 *
 * The archive itself is Rust's (`pollis-core/src/commands/export.rs`, unit-
 * tested there, including "never touches the network"). What this tier pins
 * is the contract between the two halves:
 *
 *   1. the "Your data" section renders on Security and one click reaches
 *      `export_archive` with the path the OS picker returned and no scope,
 *   2. cancelling the picker writes nothing and invokes nothing,
 *   3. the summary reports files honestly and the network step is OFFERED,
 *      not taken — `fetch_export_attachments` runs only after its own button,
 *   4. the two conversation entry points (DM settings, channel header) pass
 *      exactly their conversation id.
 *
 *   pnpm --filter @pollis/e2e e2e:ui -- export-archive.spec.ts
 */

import { test, expect, type Page } from "@playwright/test";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };

const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0XCB";
const DM_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0DMX";
const HASH = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
const SAVE_PATH = "/Users/alice/Downloads/pollis-archive-2026-09-11.json";

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

function preloadState(skin: Skin, exportSavePath: string | null = SAVE_PATH) {
  const now = "2026-09-01T10:00:00.000Z";
  return {
    session: USER,
    profile: { id: USER.id, username: USER.username },
    preferences: JSON.stringify({ skin }),
    exportSavePath,
    groups: [{ id: GROUP_ID, name: "Acme", owner_id: USER.id, created_at: now }],
    channels: {
      [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }],
    },
    dmChannels: [
      {
        id: DM_ID,
        created_by: USER.id,
        created_at: now,
        members: [
          { user_id: USER.id, username: "alice", added_by: USER.id, added_at: now },
          { user_id: "u-bob", username: "bob", added_by: USER.id, added_at: now },
        ],
      },
    ],
    messages: {
      [CHANNEL_ID]: [
        {
          id: "01HQ7Z3K9M2P5R8T1V4W6Y0MS1",
          conversation_id: CHANNEL_ID,
          sender_id: "u-bob",
          content: "plain text",
          sent_at: "2026-08-01T10:05:00.000Z",
        },
        {
          id: "01HQ7Z3K9M2P5R8T1V4W6Y0MS2",
          conversation_id: CHANNEL_ID,
          sender_id: USER.id,
          content: JSON.stringify({
            _att: [{ key: "r2/cat", name: "cat.png", ct: "image/png", size: 3, hash: HASH }],
            _txt: "look",
          }),
          sent_at: "2026-08-01T10:06:00.000Z",
        },
      ],
      [DM_ID]: [
        {
          id: "01HQ7Z3K9M2P5R8T1V4W6Y0DMM",
          conversation_id: DM_ID,
          sender_id: "u-bob",
          content: "hi",
          sent_at: "2026-08-01T11:00:00.000Z",
        },
      ],
    },
    vaultMessages: [
      { id: "v1", user_id: USER.id, content: "a note", created_at: now, updated_at: now, pinned: false },
    ],
  };
}

async function boot(page: Page, preload: ReturnType<typeof preloadState>) {
  await page.addInitScript((state) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = state;
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
  }, preload);
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
}

async function openCommandPalette(page: Page) {
  await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
  await expect(page.getByTestId("search-panel")).toBeVisible();
}

async function gotoSecurity(page: Page) {
  await openCommandPalette(page);
  await page.getByTestId("search-panel-input").fill("Security");
  await page
    .getByTestId("search-panel-result-item")
    .filter({ hasText: "/security" })
    .first()
    .click();
  await expect(page.getByTestId("security-page")).toBeVisible();
}

async function gotoChannel(page: Page) {
  await openCommandPalette(page);
  await page.getByTestId("search-panel-input").fill("general");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("channel-export-trigger")).toBeVisible();
}

async function invokeCount(page: Page, cmd: string): Promise<number> {
  return page.evaluate(
    (c) => (window as unknown as { __tauriInvokeCounts: Record<string, number> }).__tauriInvokeCounts[c] ?? 0,
    cmd,
  );
}

async function lastArgs<T>(page: Page, cmd: string): Promise<T> {
  return page.evaluate(
    (c) => (window as unknown as { __tauriLastArgs: Record<string, unknown> }).__tauriLastArgs[c] as T,
    cmd,
  );
}

for (const skin of SKINS) {
  test.describe(`export archive — ${skin} skin`, () => {
    test("one click on Security writes the account archive to the picked path", async ({ page }) => {
      await boot(page, preloadState(skin));
      await gotoSecurity(page);

      const section = page.getByTestId("export-section");
      await expect(section).toBeVisible();
      // The exit sits directly above the danger zone, never below it.
      const exportBox = await section.boundingBox();
      const dangerBox = await page.getByTestId("settings-danger-zone").boundingBox();
      expect(exportBox!.y).toBeLessThan(dangerBox!.y);

      await page.getByTestId("export-archive-button").click();

      const done = page.getByTestId("export-archive-button-done");
      await expect(done).toBeVisible();
      await expect(done).toContainText("3 messages");
      await expect(done).toContainText("2 conversations");
      await expect(done).toContainText(SAVE_PATH);
      // Files are counted honestly: nothing cached in the browser, one missing.
      await expect(done).toContainText("0 attachments copied alongside");
      await expect(done).toContainText("1 not on this device");

      expect(await lastArgs<{ path: string; conversationId: string | null }>(page, "export_archive")).toEqual({
        path: SAVE_PATH,
        conversationId: null,
      });
    });

    test("cancelling the picker writes nothing and invokes nothing", async ({ page }) => {
      await boot(page, preloadState(skin, null));
      await gotoSecurity(page);
      await page.getByTestId("export-archive-button").click();

      // Give a wrongly-eager implementation the chance to fire.
      await expect(page.getByTestId("export-archive-button")).toBeEnabled();
      expect(await invokeCount(page, "pick_save_path")).toBe(1);
      expect(await invokeCount(page, "export_archive")).toBe(0);
      await expect(page.getByTestId("export-archive-button-done")).toHaveCount(0);
      await expect(page.getByTestId("export-archive-button-error")).toHaveCount(0);
    });

    test("the network step is offered after the export and taken only on its own button", async ({ page }) => {
      await boot(page, preloadState(skin));
      await gotoSecurity(page);
      await page.getByTestId("export-archive-button").click();
      await expect(page.getByTestId("export-archive-button-done")).toBeVisible();

      const fetch = page.getByTestId("export-archive-button-fetch");
      await expect(fetch).toBeVisible();
      await expect(fetch).toContainText("missing attachment");
      expect(await invokeCount(page, "fetch_export_attachments")).toBe(0);

      await fetch.click();
      const fetched = page.getByTestId("export-archive-button-fetched");
      await expect(fetched).toBeVisible();
      await expect(fetched).toContainText("1 attachment downloaded");
      expect(await invokeCount(page, "fetch_export_attachments")).toBe(1);
      const args = await lastArgs<{ filesDir: string; attachments: { content_hash: string; file: string }[] }>(
        page,
        "fetch_export_attachments",
      );
      expect(args.filesDir).toBe(SAVE_PATH.replace(/\.json$/, "-files"));
      expect(args.attachments).toHaveLength(1);
      expect(args.attachments[0].content_hash).toBe(HASH);
      // The offer is a one-shot: once taken, it is gone.
      await expect(fetch).toHaveCount(0);
    });

    test("DM settings exports exactly that conversation", async ({ page }) => {
      await boot(page, preloadState(skin));
      await page.getByTestId("menu-item-dms").click();
      await page.getByTestId(`dm-option-${DM_ID}-secondary`).click();
      await page.getByTestId("dm-settings-export-button").click();

      const done = page.getByTestId("dm-settings-export-button-done");
      await expect(done).toBeVisible();
      await expect(done).toContainText("1 message across 1 conversation");
      expect(await lastArgs<{ conversationId: string | null }>(page, "export_archive")).toMatchObject({
        conversationId: DM_ID,
      });
      // A text-only conversation offers no download.
      await expect(page.getByTestId("dm-settings-export-button-fetch")).toHaveCount(0);
    });

    test("the channel header exports exactly that channel", async ({ page }) => {
      await boot(page, preloadState(skin));
      await gotoChannel(page);
      await page.getByTestId("channel-export-trigger").click();

      await expect(page.getByTestId("channel-export-trigger-done")).toBeVisible();
      expect(await lastArgs<{ conversationId: string | null }>(page, "export_archive")).toMatchObject({
        conversationId: CHANNEL_ID,
      });
    });
  });
}
