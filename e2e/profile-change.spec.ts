// A profile change elsewhere (#2084), desktop: the DS's `profile_changed` nudge makes every
// surface that names the person redraw, with no reload: sender labels on old messages, the
// member list and the DM list.

import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP = "g_team";
const GENERAL = "c_general";
const DM = "dm_bob";
const SHOTS = "artifacts/profile-change";

function msg(id: string, conversation: string, content: string) {
  return {
    id,
    conversation_id: conversation,
    sender_id: "u_bob",
    sender_username: "bob",
    content,
    sent_at: "2026-01-01T00:00:01Z",
  };
}

const PRELOAD = {
  session: ME,
  profile: { id: ME.id, username: ME.username },
  groups: [{ id: GROUP, name: "Team", owner_id: ME.id, created_at: "2026-01-01T00:00:00Z", current_user_role: "admin" }],
  channels: { [GROUP]: [{ id: GENERAL, group_id: GROUP, name: "general" }] },
  groupMembers: {
    [GROUP]: [
      { user_id: ME.id, username: "mia", role: "admin", joined_at: "2026-01-01T00:00:00Z" },
      { user_id: "u_bob", username: "bob", role: "member", joined_at: "2026-01-01T00:00:00Z" },
    ],
  },
  dmChannels: [
    {
      id: DM,
      created_by: "u_bob",
      created_at: "2026-01-01T00:00:00Z",
      members: [
        { user_id: ME.id, username: "mia", added_by: "u_bob", added_at: "" },
        { user_id: "u_bob", username: "bob", added_by: "u_bob", added_at: "" },
      ],
    },
  ],
  messages: {
    [GENERAL]: [msg("m_group", GENERAL, "hello team")],
    [DM]: [msg("m_dm", DM, "hello mia")],
  },
};

async function boot(page: Page) {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, PRELOAD);
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
}

/** What the DS now answers after Bob renames himself on another device. */
async function renameBobRemotely(page: Page) {
  await page.evaluate(
    ([group, dm]) => {
      type Row = Record<string, unknown>;
      const mock = (window as unknown as {
        __tauriMock: { messages: Record<string, Row[]>; groupMembers: Record<string, Row[]>; dmChannels: Row[] };
      }).__tauriMock;
      // New objects, as IPC would hand back: the mock otherwise returns the cached rows themselves.
      const rename = (row: Row): Row =>
        row.user_id === "u_bob" || row.sender_id === "u_bob"
          ? { ...row, ...(row.sender_id === "u_bob" ? { sender_username: "bobby" } : { username: "bobby" }) }
          : row;
      for (const key of Object.keys(mock.messages)) {
        mock.messages[key] = mock.messages[key].map(rename);
      }
      mock.groupMembers[group] = mock.groupMembers[group].map(rename);
      mock.dmChannels = mock.dmChannels.map((d) =>
        d.id === dm ? { ...d, members: (d.members as Row[]).map(rename) } : d,
      );
    },
    [GROUP, DM],
  );
}

async function emitProfileChanged(page: Page) {
  await page.evaluate(() => {
    (window as unknown as { __emitRealtimeEvent: (event: unknown) => number }).__emitRealtimeEvent({
      type: "profile_changed",
      user_id: "u_bob",
    });
  });
}

function author(page: Page, messageId: string) {
  return page.getByTestId(`message-${messageId}`).getByTestId("message-author");
}

test("an old message's sender label follows a rename made on another device, without a reload", async ({ page }) => {
  await boot(page);
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP}`).click();
  await page.getByTestId(`channel-option-${GENERAL}`).click();
  await expect(author(page, "m_group")).toHaveText("bob");
  await page.screenshot({ path: `${SHOTS}/before.png` });

  await renameBobRemotely(page);
  await emitProfileChanged(page);
  await expect(author(page, "m_group")).toHaveText("bobby");
  await page.screenshot({ path: `${SHOTS}/after.png` });
});

test("the member list follows the rename", async ({ page }) => {
  await boot(page);
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP}`).click();
  await page.getByTestId("menu-item-members").click();
  await expect(page.getByTestId("member-name-u_bob")).toHaveText("bob");

  await renameBobRemotely(page);
  await emitProfileChanged(page);
  await expect(page.getByTestId("member-name-u_bob")).toHaveText("bobby");
});

test("the DM list title and the DM's messages follow the rename", async ({ page }) => {
  await boot(page);
  await page.getByTestId("menu-item-dms").click();
  await expect(page.getByTestId(`dm-option-${DM}`)).toContainText("bob");

  await renameBobRemotely(page);
  await emitProfileChanged(page);
  await expect(page.getByTestId(`dm-option-${DM}`)).toContainText("bobby");
  await page.getByTestId(`dm-option-${DM}`).click();
  await expect(author(page, "m_dm")).toHaveText("bobby");
});
