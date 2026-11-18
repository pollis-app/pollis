// Sender-generated link previews (#1987). LINK_PREVIEW_SHOTS=<dir> also writes review screenshots.

import fs from "node:fs";
import path from "node:path";
import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_links";
const CHANNEL_ID = "c_general";

const FIXTURES = path.join(__dirname, "fixtures");
const MEDIA_ORIGIN = "https://media.test";
const THUMB_KEY = "media/lp-thumb.enc";
const PHOTO_BH = "LhLyBf;W{?KE,2XMSvV{[ibDJyJ#";

const GITHUB = "https://github.com/";
const DRAFT = {
  url: GITHUB,
  title: "GitHub · Build and ship software on a single, collaborative platform",
  description:
    "Join the world's most widely adopted, AI-powered developer platform where millions of developers, businesses, and the largest open source community build software that advances humanity.",
  image: null,
};

const RECEIVED = {
  id: "m_dana",
  conversation_id: CHANNEL_ID,
  sender_id: "u_dana",
  sender_username: "dana",
  ciphertext: "",
  content: `have you seen this ${GITHUB}`,
  sent_at: "2026-11-17T09:00:00Z",
  link_preview: {
    url: GITHUB,
    title: DRAFT.title,
    description: DRAFT.description,
    image: { key: THUMB_KEY, hash: "h-lp", kv: 1, sha: "s-lp", ct: "image/jpeg", size: 4096, w: 600, h: 450, bh: PHOTO_BH },
  },
};

// A preview naming a link the body does not show is never drawn.
const MISMATCHED = {
  ...RECEIVED,
  id: "m_spoof",
  content: "nothing to see here https://example.com/",
  sent_at: "2026-11-17T09:01:00Z",
  link_preview: { url: GITHUB, title: "Not this one", description: null, image: null },
};

const DANA_ALIAS = "Dana (Links)";

type Opts = {
  skin?: "modern" | "terminal";
  messages?: unknown[];
  delayMs?: number;
  background?: string;
  fontSize?: string;
};

async function boot(page: Page, opts: Opts = {}) {
  await page.route(`${MEDIA_ORIGIN}/**`, (route) =>
    route.fulfill({ body: fs.readFileSync(path.join(FIXTURES, "sensitive-photo.jpg")), contentType: "image/jpeg" }),
  );
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [
      { id: GROUP_ID, name: "links", owner_id: ME.id, created_at: "2026-01-01T00:00:00Z", current_user_role: "admin" },
    ],
    channels: { [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }] },
    groupMembers: {
      [GROUP_ID]: [
        { user_id: ME.id, username: "mia", role: "admin", joined_at: "2026-01-01T00:00:00Z" },
        { user_id: "u_dana", username: "dana", role: "member", joined_at: "2026-01-01T00:00:00Z" },
      ],
    },
    // Dana has an alias in this group (#2003): her rows carry the alias and the card together.
    groupAliases: [{ group_id: GROUP_ID, user_id: "u_dana", display_name: DANA_ALIAS, avatar_url: null }],
    messages: { [CHANNEL_ID]: opts.messages ?? [] },
    mediaUrls: { [THUMB_KEY]: `${MEDIA_ORIGIN}/thumb.jpg` },
    linkPreviews: { [GITHUB]: DRAFT },
    linkPreviewDelayMs: opts.delayMs ?? 0,
    dmChannels: [],
    preferences: {
      skin: opts.skin ?? "modern",
      ...(opts.background ? { background_color: opts.background } : {}),
      ...(opts.fontSize ? { font_size: opts.fontSize } : {}),
    },
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP_ID}`).click();
  await page.getByTestId(`channel-option-${CHANNEL_ID}`).click();
  await expect(page.getByTestId("message-input")).toBeVisible();
}

const fetches = (page: Page) =>
  page.evaluate(() => (window as unknown as { __linkPreviewFetches?: string[] }).__linkPreviewFetches ?? []);

/** Paste `text` into the composer, as a clipboard paste arrives. */
async function paste(page: Page, text: string) {
  await page.getByTestId("message-input").click();
  await page.evaluate((t) => {
    const data = new DataTransfer();
    data.setData("text/plain", t);
    const target = document.activeElement ?? document.querySelector('[data-testid="message-input"]')!;
    target.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true }));
  }, text);
}

async function shot(page: Page, name: string) {
  const dir = process.env.LINK_PREVIEW_SHOTS;
  await page.waitForFunction(() =>
    Array.from(document.images).every((img) => img.complete && img.naturalWidth > 0),
  );
  await page.screenshot({ path: dir ? path.join(dir, `${name}.png`) : test.info().outputPath(`${name}.png`) });
}

test.describe("link previews — Modern", () => {
  test("a pasted link shows the loading card at once, then the card; × keeps it off for the draft", async ({ page }) => {
    await boot(page, { delayMs: 800 });
    await paste(page, GITHUB);
    const card = page.getByTestId("composer-link-preview");
    await expect(card).toBeVisible();
    await expect(card.locator("[aria-busy=true]")).toBeVisible();
    await expect(card).toContainText("github.com");
    await expect(card).toContainText(DRAFT.title);
    await expect(card.locator("[aria-busy=true]")).toHaveCount(0);
    expect(await fetches(page)).toEqual([GITHUB]);
    await shot(page, "desktop-composer");

    await card.getByTestId("link-preview-remove").click();
    await expect(card).toHaveCount(0);
    // Still the same link in the draft: it stays unpreviewed, and nothing is fetched again.
    await page.keyboard.type(" and more");
    await page.waitForTimeout(900);
    await expect(page.getByTestId("composer-link-preview")).toHaveCount(0);
    expect(await fetches(page)).toEqual([GITHUB]);
  });

  test("a typed link shows its card after the pause, and the sent message keeps it", async ({ page }) => {
    await boot(page);
    await page.getByTestId("message-input").click();
    await page.keyboard.type(`look ${GITHUB}`);
    await expect(page.getByTestId("composer-link-preview")).toContainText(DRAFT.title);
    await page.keyboard.press("Enter");
    await expect(page.getByTestId("composer-link-preview")).toHaveCount(0);
    const sent = page.getByTestId("message-link-preview");
    await expect(sent).toHaveCount(1);
    await expect(sent).toContainText(DRAFT.title);
    const args = await page.evaluate(
      () => (window as unknown as { __tauriLastArgs: Record<string, Record<string, unknown>> }).__tauriLastArgs.send_message,
    );
    expect((args.linkPreview as { url: string }).url).toBe(GITHUB);
  });

  test("sending while the card is still loading sends at once, with no preview", async ({ page }) => {
    await boot(page, { delayMs: 3000 });
    await paste(page, GITHUB);
    await expect(page.getByTestId("composer-link-preview").locator("[aria-busy=true]")).toBeVisible();
    await page.keyboard.press("Enter");
    await expect(page.locator('[data-testid^="message-"]').filter({ hasText: GITHUB }).first()).toBeVisible();
    const args = await page.evaluate(
      () => (window as unknown as { __tauriLastArgs: Record<string, Record<string, unknown>> }).__tauriLastArgs.send_message,
    );
    expect(args.linkPreview ?? null).toBeNull();
    await expect(page.getByTestId("message-link-preview")).toHaveCount(0);
  });

  test("a received card renders and opens the body link; a preview of a link not in the body is not drawn", async ({ page }) => {
    await boot(page, { messages: [RECEIVED, MISMATCHED] });
    const card = page.getByTestId("message-dana").or(page.getByTestId("message-m_dana")).getByTestId("message-link-preview");
    await expect(page.getByTestId("message-link-preview")).toHaveCount(1);
    await expect(card).toContainText(DRAFT.title);
    await expect(card).toContainText("github.com");
    await expect(card.locator("img")).toBeVisible();
    // The sender's group alias (#2003) heads the row that carries the card.
    await expect(page.getByTestId("message-m_dana")).toContainText(DANA_ALIAS);
    // Fixed height: the card is the same box before and after its thumbnail.
    const box = await card.boundingBox();
    expect(box!.height).toBeGreaterThan(60);
    await shot(page, "desktop-received");

    await card.click();
    await expect
      .poll(() => page.evaluate(() => (window as unknown as { __pollisOpened?: string[] }).__pollisOpened ?? []))
      .toEqual([GITHUB]);
    // The renderer never loads anything from the previewed host.
    const requests: string[] = [];
    page.on("request", (r) => requests.push(r.url()));
    await page.waitForTimeout(300);
    expect(requests.filter((u) => u.includes("github.com"))).toEqual([]);
  });

  test("editing to another link shows its card in the edit bar; editing the link out drops it", async ({ page }) => {
    await boot(page, { messages: [{ ...RECEIVED, sender_id: ME.id, sender_username: ME.username }] });
    await expect(page.getByTestId("message-link-preview")).toHaveCount(1);
    await page.getByTestId("message-m_dana").hover();
    await page.getByTestId("message-m_dana").getByTestId("edit-button").click();
    const input = page.getByTestId("edit-message-bar-input");
    await expect(input).toBeFocused();
    await input.fill(`moved to ${GITHUB}`);
    await expect(page.getByTestId("composer-link-preview")).toContainText(DRAFT.title);
    await shot(page, "desktop-edit-bar");
    await input.fill("no link any more");
    await expect(page.getByTestId("composer-link-preview")).toHaveCount(0);
    await page.keyboard.press("Enter");
    await expect(page.getByTestId("message-m_dana")).toContainText("no link any more");
    await expect(page.getByTestId("message-link-preview")).toHaveCount(0);
  });

  test("delete for everyone takes the card with the text", async ({ page }) => {
    await boot(page, { messages: [{ ...RECEIVED, sender_id: ME.id, sender_username: ME.username }] });
    const row = page.getByTestId("message-m_dana");
    await expect(row.getByTestId("message-link-preview")).toHaveCount(1);
    await row.hover();
    await row.getByTestId("message-actions-more").click();
    await page.getByTestId("delete-button").click();
    await expect(page.getByTestId("delete-message-bar")).toBeVisible();
    await page.getByTestId("delete-message-confirm").click();
    await expect(page.getByTestId("message-link-preview")).toHaveCount(0);
    await expect(row).not.toContainText(GITHUB);
  });

  test("with Generate link previews off, the composer fetches nothing; received cards still render", async ({ page }) => {
    await boot(page, { messages: [RECEIVED] });
    await page.getByTestId("sidebar-row-preferences").click();
    const toggle = page.getByTestId("pref-link-previews-enabled");
    await expect(toggle).toBeVisible();
    await expect(toggle).toHaveAttribute("aria-checked", "true");
    await toggle.click();
    await expect(toggle).toHaveAttribute("aria-checked", "false");
    // Back to the channel the way a person would: the title bar's Back.
    while (!(await page.getByTestId("message-input").isVisible())) {
      await page.getByRole("button", { name: "Back" }).click();
    }
    await expect(page.getByTestId("message-link-preview")).toHaveCount(1);
    await paste(page, GITHUB);
    await page.waitForTimeout(900);
    await expect(page.getByTestId("composer-link-preview")).toHaveCount(0);
    expect(await fetches(page)).toEqual([]);
  });
});

test("strict network privacy shows the switch off and disabled, with the note", async ({ page }) => {
  await boot(page);
  await page.getByTestId("sidebar-row-preferences").click();
  const toggle = page.getByTestId("pref-link-previews-enabled");
  await expect(toggle).toBeEnabled();
  await expect(page.getByTestId("pref-link-previews-strict")).toHaveCount(0);
  await page.getByTestId("pref-overlay-mode-strict").click();
  await expect(toggle).toBeDisabled();
  await expect(toggle).toHaveAttribute("aria-checked", "false");
  await expect(page.getByTestId("pref-link-previews-strict")).toBeVisible();
  await page.getByTestId("pref-link-previews").scrollIntoViewIfNeeded();
  await shot(page, "desktop-preferences-strict");
});

test("a DM request shows only the bare link until it is accepted, then the card", async ({ page }) => {
  const dm = {
    id: "dm_req",
    created_by: "u_dana",
    created_at: "2026-11-17T08:00:00Z",
    members: [
      { user_id: ME.id, username: ME.username, added_by: "u_dana", added_at: "2026-11-17T08:00:00Z" },
      { user_id: "u_dana", username: "dana", added_by: "u_dana", added_at: "2026-11-17T08:00:00Z" },
    ],
  };
  await page.route(`${MEDIA_ORIGIN}/**`, (route) =>
    route.fulfill({ body: fs.readFileSync(path.join(FIXTURES, "sensitive-photo.jpg")), contentType: "image/jpeg" }),
  );
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [],
    dmChannels: [dm],
    dmRequests: [dm],
    messages: { dm_req: [{ ...RECEIVED, conversation_id: "dm_req" }] },
    mediaUrls: { [THUMB_KEY]: `${MEDIA_ORIGIN}/thumb.jpg` },
    preferences: { skin: "modern" },
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.getByTestId("sidebar-row-dms").click();
  await page.getByTestId("dm-option-dm_req").click();
  await expect(page.getByTestId("dm-request-bar")).toBeVisible();
  await expect(page.getByTestId("message-m_dana")).toContainText(GITHUB);
  await expect(page.getByTestId("message-link-preview")).toHaveCount(0);
  await page.getByTestId("dm-request-accept").click();
  await expect(page.getByTestId("dm-request-bar")).toHaveCount(0);
  await expect(page.getByTestId("message-link-preview")).toHaveCount(1);
});

test("an image link keeps its click-to-load unfurl and gets no card", async ({ page }) => {
  const IMAGE = `${MEDIA_ORIGIN}/photo.jpg`;
  await boot(page, {
    messages: [{ ...RECEIVED, id: "m_img", content: `pic ${IMAGE}`, link_preview: null }],
  });
  await expect(page.getByTestId("message-m_img").getByTestId("media-link-unfurl")).toBeVisible();
  await expect(page.getByTestId("message-link-preview")).toHaveCount(0);
});

test("Terminal look: no card on messages and no fetch in the composer", async ({ page }) => {
  await boot(page, { skin: "terminal", messages: [RECEIVED] });
  await expect(page.getByTestId("message-m_dana")).toContainText(GITHUB);
  await expect(page.getByTestId("message-m_dana")).toContainText(DANA_ALIAS);
  await expect(page.getByTestId("message-link-preview")).toHaveCount(0);
  await paste(page, GITHUB);
  await page.waitForTimeout(900);
  await expect(page.getByTestId("composer-link-preview")).toHaveCount(0);
  expect(await fetches(page)).toEqual([]);
  await shot(page, "desktop-terminal");
});

// Review screenshots: light ground and a large font size.
test("review shots: light theme and large text", async ({ page }) => {
  test.skip(!process.env.LINK_PREVIEW_SHOTS, "screenshots only");
  await boot(page, { messages: [RECEIVED], background: "#f6f1e7" });
  await expect(page.getByTestId("message-link-preview").locator("img")).toBeVisible();
  await paste(page, GITHUB);
  await expect(page.getByTestId("composer-link-preview")).toContainText(DRAFT.title);
  await shot(page, "desktop-light");
  await page.evaluate(() => localStorage.clear());
  await boot(page, { messages: [RECEIVED], fontSize: "20" });
  await expect(page.getByTestId("message-link-preview").locator("img")).toBeVisible();
  await shot(page, "desktop-large-text");
});
