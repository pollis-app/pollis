// Dragging a side panel's resize edge never selects anything on the page (#2128), in BOTH looks.

import { test, expect, type Page } from "./lib/zero-loss";

const ALICE = { id: "u-alice", email: "alice@example.com", username: "alice" };
const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GRP";
const GENERAL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GEN";

const SKINS = ["terminal", "modern"] as const;
type Skin = (typeof SKINS)[number];

/** Messages give the drag text to sweep across, as the bug report's page had. */
function preloadState(skin: Skin) {
  const messages = Array.from({ length: 12 }, (_, i) => ({
    id: `01HQ7Z3K9M2P5R8T1V4W6Y${String(i).padStart(4, "0")}`,
    conversation_id: GENERAL_ID,
    sender_id: ALICE.id,
    content: `Selectable message body number ${i} with plenty of words to sweep across`,
    sent_at: new Date(Date.UTC(2026, 7, 1, 9, i)).toISOString(),
  }));
  return {
    session: ALICE,
    profile: { id: ALICE.id, username: ALICE.username },
    preferences: JSON.stringify({ skin, right_panel_open_by_default: true }),
    groups: [
      { id: GROUP_ID, name: "Acme", owner_id: ALICE.id, created_at: new Date().toISOString() },
    ],
    channels: {
      [GROUP_ID]: [{ id: GENERAL_ID, group_id: GROUP_ID, name: "general", channel_type: "text" }],
    },
    groupMembers: {
      [GROUP_ID]: [
        { user_id: ALICE.id, username: ALICE.username, role: "admin", joined_at: "2026-08-01T09:00:00.000Z" },
        { user_id: "u-bob", username: "bob", role: "member", joined_at: "2026-08-01T09:00:00.000Z" },
      ],
    },
    dmChannels: [],
    messages: { [GENERAL_ID]: messages },
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
      convertFileSrc: (p: string) => p,
      registerListener: () => {},
      unregisterListener: () => {},
      runCallback: () => {},
      invoke: () => Promise.resolve(null),
    };
  }, preloadState(skin));
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
  await page.getByTestId("sidebar").getByRole("button", { name: /general$/ }).click();
  await expect(page.getByTestId("right-panel")).toBeVisible();
}

const selectionText = (page: Page) =>
  page.evaluate(() => window.getSelection()?.toString() ?? "");

/** Press on the handle and sweep across the page in `steps`, ending at `toX`. */
async function dragHandle(page: Page, testId: string, toX: number, toY: number) {
  const box = await page.getByTestId(testId).boundingBox();
  if (!box) {
    throw new Error(`${testId} has no box`);
  }
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 3);
  await page.mouse.down();
  await page.mouse.move(toX, toY, { steps: 20 });
  return box;
}

for (const skin of SKINS) {
  test.describe(`resize drag selects nothing — ${skin} look`, () => {
    for (const edge of ["end", "start"] as const) {
      const which = edge === "end" ? "left sidebar" : "right panel";
      test(`dragging the ${which} edge leaves the selection empty`, async ({ page }) => {
        await boot(page, skin);
        const testId = `resize-handle-${edge}`;
        const viewport = page.viewportSize()!;
        // Sweep diagonally over the message log and back, the path that highlighted text.
        await dragHandle(page, testId, viewport.width / 2, viewport.height - 40);
        expect(await selectionText(page), "mid-drag").toBe("");
        await page.mouse.up();
        expect(await selectionText(page), "after release").toBe("");
        await expect(page.getByTestId(testId)).not.toHaveAttribute("data-dragging", "true");

        // Selection still works normally once the drag is over.
        const body = page.getByText("Selectable message body number 3", { exact: false }).first();
        await body.dblclick();
        expect(await selectionText(page), "a normal double-click still selects").not.toBe("");
      });

      test(`the ${which} drag restores selection when released outside the window`, async ({ page }) => {
        await boot(page, skin);
        const testId = `resize-handle-${edge}`;
        const viewport = page.viewportSize()!;
        await dragHandle(page, testId, viewport.width / 2, viewport.height / 2);
        // Leave the window through the bottom edge (sideways would collapse or max out the panel) and release there.
        await page.mouse.move(viewport.width / 2, viewport.height + 50, { steps: 5 });
        await page.mouse.up();
        expect(await selectionText(page), "after an outside release").toBe("");
        await expect(page.getByTestId(testId)).not.toHaveAttribute("data-dragging", "true");
        const body = page.getByText("Selectable message body number 3", { exact: false }).first();
        await body.dblclick();
        expect(await selectionText(page), "selection is back after an outside release").not.toBe("");
      });

      test(`a lost capture (window blur) ends the ${which} drag`, async ({ page }) => {
        await boot(page, skin);
        const testId = `resize-handle-${edge}`;
        const viewport = page.viewportSize()!;
        await dragHandle(page, testId, viewport.width / 2, viewport.height / 2);
        // The OS can take the pointer away mid-drag (alt-tab, a system dialog) with no pointerup.
        await page.evaluate(() => {
          window.dispatchEvent(new Event("blur"));
        });
        await expect(page.getByTestId(testId)).not.toHaveAttribute("data-dragging", "true");
        await page.mouse.up();
        const body = page.getByText("Selectable message body number 3", { exact: false }).first();
        await body.dblclick();
        expect(await selectionText(page), "selection works after an interrupted drag").not.toBe("");
      });
    }
  });
}
