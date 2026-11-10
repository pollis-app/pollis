// Composer paste fallback (#1794): a paste the webview can't see as a file reads the OS
// clipboard once, in the background, and the composer stays usable while it does.

import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_paste";
const CHANNEL_ID = "c_general";

// A 1x1 PNG.
const PNG = [
  0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
  0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
  0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0,
  0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
  0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

async function boot(page: Page) {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [
      { id: GROUP_ID, name: "paste", owner_id: ME.id, created_at: "2026-01-01T00:00:00Z", current_user_role: "admin" },
    ],
    channels: { [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }] },
    groupMembers: {
      [GROUP_ID]: [{ user_id: ME.id, username: "mia", role: "admin", joined_at: "2026-01-01T00:00:00Z" }],
    },
    messages: { [CHANNEL_ID]: [] },
    dmChannels: [],
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP_ID}`).click();
  await page.getByTestId(`channel-option-${CHANNEL_ID}`).click();
  await expect(page.getByTestId("message-input")).toBeVisible();
  await page.evaluate(() => (window as any).__resetTauriInvokeCounts());
}

/** Dispatch a paste carrying `files` (empty = what WebKitGTK hands over for a Steam copy on X11). */
async function paste(page: Page, files: { name: string; type: string; bytes: number[] }[] = []) {
  await page.getByTestId("message-input").click();
  await page.evaluate((list) => {
    const data = new DataTransfer();
    for (const f of list) {
      data.items.add(new File([new Uint8Array(f.bytes)], f.name, { type: f.type }));
    }
    const target = document.querySelector('[data-testid="message-input"]')!;
    target.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true }));
  }, files);
}

const invokes = (page: Page, cmd: string) =>
  page.evaluate((c) => (window as any).__tauriInvokeCounts[c] ?? 0, cmd);

test("a paste with no file on the event attaches the OS clipboard's image", async ({ page }) => {
  await boot(page);
  await page.evaluate((image) => (window as any).__setOsClipboard({ image }), PNG);
  await paste(page);
  await expect(page.getByTestId("composer-attachment")).toHaveCount(1);
  await expect(page.getByTestId("composer-attachment").locator("img")).toBeVisible();
});

test("a zero-byte file on the event is not attached; the OS read runs instead", async ({ page }) => {
  await boot(page);
  await page.evaluate((image) => (window as any).__setOsClipboard({ image }), PNG);
  await paste(page, [{ name: "broken.bmp", type: "image/bmp", bytes: [] }]);
  await expect(page.getByTestId("composer-attachment")).toHaveCount(1);
  expect(await invokes(page, "read_clipboard_image")).toBe(1);
  expect(await invokes(page, "stage_attachment")).toBe(1);
});

test("a file on the event never touches the OS clipboard", async ({ page }) => {
  await boot(page);
  await paste(page, [{ name: "shot.png", type: "image/png", bytes: PNG }]);
  await expect(page.getByTestId("composer-attachment")).toHaveCount(1);
  expect(await invokes(page, "read_clipboard_files")).toBe(0);
  expect(await invokes(page, "read_clipboard_image")).toBe(0);
});

test("while a slow clipboard owner is read, the composer types and repeat pastes don't pile up", async ({ page }) => {
  await boot(page);
  await page.evaluate((image) => (window as any).__setOsClipboard({ image }), PNG);
  await page.evaluate(() => (window as any).__tauriHold("read_clipboard_files"));
  await paste(page);
  await paste(page);
  await paste(page);
  expect(await invokes(page, "read_clipboard_files")).toBe(1);

  // The window is not frozen: typing lands while the read is outstanding.
  await page.keyboard.type("still here");
  await expect(page.getByTestId("message-input")).toContainText("still here");

  await page.evaluate(() => (window as any).__tauriRelease("read_clipboard_files"));
  await expect(page.getByTestId("composer-attachment")).toHaveCount(1);

  // Settled: the next paste reads again.
  await paste(page);
  await expect.poll(() => invokes(page, "read_clipboard_files")).toBe(2);
});

test("an empty clipboard pastes nothing and breaks nothing", async ({ page }) => {
  await boot(page);
  await paste(page);
  await expect.poll(() => invokes(page, "read_clipboard_image")).toBe(1);
  await expect(page.getByTestId("composer-attachment")).toHaveCount(0);
  await page.keyboard.type("hello");
  await expect(page.getByTestId("message-input")).toContainText("hello");
});

// X11 (#1794): WebKitGTK's own paste can hang on the clipboard owner, so the app takes
// Ctrl+V itself: a bounded text read, then a paste event the composer handles as usual.
async function bootAppPaste(page: Page) {
  await page.addInitScript(() => {
    (window as any).__pollisAppHandlesPaste = true;
  });
  await boot(page);
  await page.getByTestId("message-input").click();
}

test("X11: Ctrl+V reads the clipboard text through the app and the composer gets it", async ({ page }) => {
  await bootAppPaste(page);
  await page.evaluate(() => (window as any).__setOsClipboard({ text: "pasted by the app" }));
  await page.keyboard.press("Control+V");
  await expect(page.getByTestId("message-input")).toContainText("pasted by the app");
  expect(await invokes(page, "read_clipboard_text")).toBe(1);
  await expect(page.getByTestId("composer-attachment")).toHaveCount(0);
});

test("X11: Shift+Insert is taken over too", async ({ page }) => {
  await bootAppPaste(page);
  await page.evaluate(() => (window as any).__setOsClipboard({ text: "shift insert" }));
  await page.keyboard.press("Shift+Insert");
  await expect(page.getByTestId("message-input")).toContainText("shift insert");
});

test("X11: a Steam copy (a file list, no text) attaches the file on Ctrl+V", async ({ page }) => {
  await bootAppPaste(page);
  await page.evaluate(() => (window as any).__setOsClipboard({ files: ["/home/someone/.steam/screenshots/shot.png"] }));
  await page.keyboard.press("Control+V");
  await expect(page.getByTestId("composer-attachment")).toHaveCount(1);
  expect(await invokes(page, "read_clipboard_text")).toBe(1);
  expect(await invokes(page, "read_clipboard_files")).toBe(1);
});

test("X11: while a slow owner holds the text read, the composer types and Ctrl+V doesn't pile up", async ({ page }) => {
  await bootAppPaste(page);
  await page.evaluate(() => (window as any).__setOsClipboard({ text: "late" }));
  await page.evaluate(() => (window as any).__tauriHold("read_clipboard_text"));
  await page.keyboard.press("Control+V");
  await page.keyboard.press("Control+V");
  await page.keyboard.press("Control+V");
  expect(await invokes(page, "read_clipboard_text")).toBe(1);

  await page.keyboard.type("typing ");
  await expect(page.getByTestId("message-input")).toContainText("typing");

  await page.evaluate(() => (window as any).__tauriRelease("read_clipboard_text"));
  await expect(page.getByTestId("message-input")).toContainText("late");
});
