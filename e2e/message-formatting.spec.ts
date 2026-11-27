// Message formatting (#2142) in BOTH looks: bold, italic, underline, strikethrough and code
// blocks as elements (never HTML strings); escapes; mentions and links inside bold; nothing
// parsed inside code; markers out of the conversation preview; the pinned list drawing the same.
// FORMATTING_SHOTS=<dir> also writes review screenshots.

import path from "node:path";
import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_fmt";
const CHANNEL_ID = "c_general";

const msg = (id: string, content: string, minute: number) => ({
  id,
  conversation_id: CHANNEL_ID,
  sender_id: "u_dana",
  sender_username: "dana",
  ciphertext: "",
  content,
  sent_at: `2026-11-20T09:${String(minute).padStart(2, "0")}:00Z`,
});

const STYLES = msg(
  "m_styles",
  "**bold** *italic* _also_ __under__ ~~gone~~ ***both*** and 2*3*4 file_name_here",
  0,
);
const ESCAPES = msg("m_escapes", "\\*not italic\\* and \\_\\_not under\\_\\_", 1);
const CODE = msg(
  "m_code",
  "look:\n```js\nconst x = **y**; // @mia https://example.com/z\n```\ndone",
  2,
);
const RICH = msg("m_rich", "hey **@mia** see **https://example.com/a_b**", 3);

async function boot(page: Page, skin: "modern" | "terminal") {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [
      { id: GROUP_ID, name: "fmt", owner_id: ME.id, created_at: "2026-01-01T00:00:00Z", current_user_role: "admin" },
    ],
    channels: { [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }] },
    groupMembers: {
      [GROUP_ID]: [
        { user_id: ME.id, username: "mia", role: "admin", joined_at: "2026-01-01T00:00:00Z" },
        { user_id: "u_dana", username: "dana", role: "member", joined_at: "2026-01-01T00:00:00Z" },
      ],
    },
    messages: { [CHANNEL_ID]: [STYLES, ESCAPES, CODE, RICH] },
    pins: [],
    dmChannels: [],
    dmRequests: [],
    preferences: { skin },
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
}

async function shot(page: Page, name: string) {
  const dir = process.env.FORMATTING_SHOTS;
  await page.screenshot({ path: dir ? path.join(dir, `${name}.png`) : test.info().outputPath(`${name}.png`) });
}

for (const skin of ["modern", "terminal"] as const) {
  test.describe(`message formatting — ${skin}`, () => {
    test("styles, escapes, code, and mentions and links inside bold", async ({ page }) => {
      await boot(page, skin);
      await page.getByTestId("menu-item-groups").click();
      await page.getByTestId(`group-option-${GROUP_ID}`).click();

      // The conversation preview keeps the words and drops the markers.
      const option = page.getByTestId(`channel-option-${CHANNEL_ID}`);
      await expect(option).toContainText("hey @mia see https://example.com/a_b");
      await expect(option).not.toContainText("**");

      await option.click();
      await expect(page.getByTestId("message-input")).toBeVisible();

      // Every style is its own element, with the markers gone.
      const styles = page.getByTestId(`message-${STYLES.id}`).getByTestId("message-content");
      await expect(styles).toContainText("bold italic also under gone both and 2*3*4 file_name_here");
      await expect(styles.locator("strong").first()).toHaveText("bold");
      await expect(styles.locator("em").first()).toHaveText("italic");
      await expect(styles.locator("em").nth(1)).toHaveText("also");
      await expect(styles.locator("u")).toHaveText("under");
      await expect(styles.locator("s")).toHaveText("gone");
      await expect(styles.locator("strong em")).toHaveText("both");
      // Arithmetic and snake_case are left alone.
      await expect(styles.locator("em")).toHaveCount(3);
      const weight = await styles.locator("strong").first().evaluate((el) => getComputedStyle(el).fontWeight);
      const style = await styles.locator("em").first().evaluate((el) => getComputedStyle(el).fontStyle);
      const line = await styles.locator("s").evaluate((el) => getComputedStyle(el).textDecorationLine);
      expect(style).toBe("italic");
      expect(line).toContain("line-through");
      if (skin === "modern") {
        expect(Number(weight)).toBeGreaterThanOrEqual(600);
      }
      // Rendered as elements, never markup from the text.
      expect(await styles.innerHTML()).not.toContain("**");

      // Escaped markers stay literal.
      const escapes = page.getByTestId(`message-${ESCAPES.id}`).getByTestId("message-content");
      await expect(escapes).toContainText("*not italic* and __not under__");
      await expect(escapes.locator("em, u, strong")).toHaveCount(0);

      // A code block: monospace, its own block, exactly as typed, nothing inside parsed.
      const code = page.getByTestId(`message-${CODE.id}`);
      const block = code.getByTestId("message-code-block");
      await expect(block).toHaveText("const x = **y**; // @mia https://example.com/z");
      await expect(block.locator("a, strong, [data-testid^=mention-]")).toHaveCount(0);
      expect(await block.evaluate((el) => getComputedStyle(el).display)).toBe("block");
      expect(await block.evaluate((el) => getComputedStyle(el).fontFamily)).toContain("DM Mono");
      await expect(code.getByTestId("message-content")).toContainText("look:");
      await expect(code.getByTestId("message-content")).toContainText("done");
      // A link only inside code gets no media tile or preview.
      await expect(code.getByTestId("message-link-preview")).toHaveCount(0);

      // A mention and a link inside bold keep working.
      const rich = page.getByTestId(`message-${RICH.id}`);
      await expect(rich.locator("strong [data-testid=mention-self]")).toHaveText("@mia");
      const link = rich.locator("strong a.message-link");
      await expect(link).toHaveAttribute("href", "https://example.com/a_b");
      await expect(link).toHaveText("https://example.com/a_b");

      await shot(page, `formatting-${skin}`);

      // The pinned list draws the same body.
      await rich.hover();
      await rich.getByTestId("message-actions-more").click();
      await page.getByTestId("pin-button").click();
      const styled = page.getByTestId(`message-${STYLES.id}`);
      await styled.hover();
      await styled.getByTestId("message-actions-more").click();
      await page.getByTestId("pin-button").click();
      await page.getByTestId("channel-pins-trigger").click();
      const rows = page.getByTestId("pinned-panel-row");
      await expect(rows).toHaveCount(2);
      const panel = page.getByTestId("pinned-panel-row").filter({ hasText: "file_name_here" });
      await expect(panel.locator("strong").first()).toHaveText("bold");
      await expect(panel.locator("u")).toHaveText("under");
      const pinnedRich = page.getByTestId("pinned-panel-row").filter({ hasText: "hey" });
      await expect(pinnedRich.locator("strong [data-testid=mention-self]")).toHaveText("@mia");
      await expect(pinnedRich.locator("strong a.message-link")).toHaveAttribute("href", "https://example.com/a_b");
      await shot(page, `formatting-pins-${skin}`);
    });
  });
}

test("copying a whole formatted message copies what was typed", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await boot(page, "modern");
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP_ID}`).click();
  await page.getByTestId(`channel-option-${CHANNEL_ID}`).click();
  const body = page.getByTestId(`message-${STYLES.id}`).getByTestId("message-formatted");
  const copied = await body.evaluate((el) => {
    const range = document.createRange();
    range.selectNodeContents(el);
    const sel = window.getSelection()!;
    sel.removeAllRanges();
    sel.addRange(range);
    const data = new DataTransfer();
    el.dispatchEvent(new ClipboardEvent("copy", { clipboardData: data, bubbles: true, cancelable: true }));
    return data.getData("text/plain");
  });
  expect(copied).toBe(STYLES.content);
});
