// Message authors by display name (#2085), desktop: in a DM the other person's messages, a reply
// to them and the header name them by display name, falling back to the handle; a rename made
// elsewhere (display name and handle) reaches the open DM with no reload. Group aliases still win
// inside groups (group-aliases.spec.ts).

import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const DM = "dm_nick";

function msg(id: string, content: string, extra: Record<string, unknown> = {}) {
  return {
    id,
    conversation_id: DM,
    sender_id: "u_nick",
    sender_username: "nicholas.dinh31_wnkd",
    sender_display_name: "Nick",
    content,
    sent_at: "2026-01-01T00:00:01Z",
    ...extra,
  };
}

const PRELOAD = {
  session: ME,
  profile: { id: ME.id, username: ME.username },
  groups: [],
  channels: {},
  dmChannels: [
    {
      id: DM,
      created_by: "u_nick",
      created_at: "2026-01-01T00:00:00Z",
      members: [
        { user_id: ME.id, username: "mia", added_by: "u_nick", added_at: "" },
        { user_id: "u_nick", username: "nicholas.dinh31_wnkd", preferred_name: "Nick", added_by: "u_nick", added_at: "" },
      ],
    },
  ],
  messages: {
    [DM]: [msg("m_1", "hey mia")],
  },
};

async function openDm(page: Page) {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, PRELOAD);
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.getByTestId("menu-item-dms").click();
  await page.getByTestId(`dm-option-${DM}`).click();
}

function author(page: Page, messageId: string) {
  return page.getByTestId(`message-${messageId}`).getByTestId("message-author");
}

test("the other person's messages and the header show the display name", async ({ page }) => {
  await openDm(page);
  await expect(author(page, "m_1")).toHaveText("Nick");
  await expect(page.getByTestId("dm-header-username")).toHaveText("Nick");
});

test("a rename elsewhere reaches the open DM without a reload", async ({ page }) => {
  await openDm(page);
  await expect(author(page, "m_1")).toHaveText("Nick");
  await page.evaluate((dm) => {
    type Row = Record<string, unknown>;
    const mock = (window as unknown as {
      __tauriMock: { messages: Record<string, Row[]>; dmChannels: Row[] };
    }).__tauriMock;
    mock.messages[dm] = mock.messages[dm].map((r) => ({ ...r, sender_username: "nick", sender_display_name: "Nicholas" }));
    mock.dmChannels = mock.dmChannels.map((d) => ({
      ...d,
      members: (d.members as Row[]).map((m) =>
        m.user_id === "u_nick" ? { ...m, username: "nick", preferred_name: "Nicholas" } : m,
      ),
    }));
    (window as unknown as { __emitRealtimeEvent: (event: unknown) => number }).__emitRealtimeEvent({
      type: "profile_changed",
      user_id: "u_nick",
    });
  }, DM);
  await expect(author(page, "m_1")).toHaveText("Nicholas");
  await expect(page.getByTestId("dm-header-username")).toHaveText("Nicholas");
});

test("without a display name the handle stands", async ({ page }) => {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, {
    ...PRELOAD,
    messages: { [DM]: [msg("m_2", "plain", { sender_display_name: null })] },
    dmChannels: PRELOAD.dmChannels.map((d) => ({
      ...d,
      members: d.members.map((m) => ({ ...m, preferred_name: undefined })),
    })),
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.getByTestId("menu-item-dms").click();
  await page.getByTestId(`dm-option-${DM}`).click();
  await expect(author(page, "m_2")).toHaveText("nicholas.dinh31_wnkd");
  await expect(page.getByTestId("dm-header-username")).toHaveText("@nicholas.dinh31_wnkd");
});
