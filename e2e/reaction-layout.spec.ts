// Reactions land with their messages (#2115): one batched read per page, and no row moves after first paint.

import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_layout";
const CHANNEL_ID = "c_general";
const COUNT = 12;

const SKINS = ["modern", "terminal"] as const;
type Skin = (typeof SKINS)[number];

const ids = Array.from({ length: COUNT }, (_, i) => `m${i + 1}`);

// Every third message reacted; one with two pills and a custom emoji (its image resolves later).
const REACTIONS: Record<string, Array<{ emoji: string; user_ids: string[]; count: number }>> = {
  m1: [{ emoji: "👍", user_ids: ["u_dana"], count: 1 }],
  m4: [
    { emoji: "🎉", user_ids: ["u_dana", ME.id], count: 2 },
    { emoji: "<:party:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa>", user_ids: ["u_dana"], count: 1 },
  ],
  m7: [{ emoji: "❤️", user_ids: ["u_dana"], count: 1 }],
  m10: [{ emoji: "😂", user_ids: [ME.id], count: 1 }],
};

function preloadFor(skin: Skin) {
  return {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [
      {
        id: GROUP_ID,
        name: "layout",
        owner_id: ME.id,
        created_at: "2026-01-01T00:00:00Z",
        current_user_role: "admin",
      },
    ],
    channels: { [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }] },
    groupMembers: {
      [GROUP_ID]: [
        { user_id: ME.id, username: "mia", role: "admin", joined_at: "2026-01-01T00:00:00Z" },
        { user_id: "u_dana", username: "dana", role: "member", joined_at: "2026-01-01T00:00:00Z" },
      ],
    },
    messages: {
      [CHANNEL_ID]: ids.map((id, i) => ({
        id,
        conversation_id: CHANNEL_ID,
        sender_id: i % 2 === 0 ? "u_dana" : ME.id,
        sender_username: i % 2 === 0 ? "dana" : "mia",
        ciphertext: "",
        content: `message number ${i + 1}`,
        sent_at: `2026-08-01T09:${String(i).padStart(2, "0")}:00Z`,
      })),
    },
    dmChannels: [],
    preferences: { skin },
    reactions: REACTIONS,
  };
}

/** Records every message row's box the moment the first one is in the DOM. */
function recordFirstPaint() {
  const w = window as unknown as Record<string, unknown>;
  const snap = () => {
    const rows = document.querySelectorAll<HTMLElement>('[data-testid^="message-m"]');
    if (rows.length === 0) {
      return false;
    }
    // Offsets from the first row: the log scrolling to its end moves every row alike.
    const base = rows[0].getBoundingClientRect().top;
    const boxes: Record<string, { offset: number; height: number }> = {};
    rows.forEach((el) => {
      const r = el.getBoundingClientRect();
      boxes[el.dataset.testid as string] = { offset: Math.round(r.top - base), height: Math.round(r.height) };
    });
    w.__firstPaintBoxes = boxes;
    w.__firstPaintPills = document.querySelectorAll('[data-testid="reaction-pill"]').length;
    return true;
  };
  const observer = new MutationObserver(() => {
    if (snap()) {
      observer.disconnect();
    }
  });
  document.addEventListener("DOMContentLoaded", () => {
    observer.observe(document.body, { childList: true, subtree: true });
  });
}

async function boxesNow(page: Page) {
  return page.evaluate(() => {
    const rows = document.querySelectorAll<HTMLElement>('[data-testid^="message-m"]');
    const base = rows[0].getBoundingClientRect().top;
    const boxes: Record<string, { offset: number; height: number }> = {};
    rows.forEach((el) => {
      const r = el.getBoundingClientRect();
      boxes[el.dataset.testid as string] = { offset: Math.round(r.top - base), height: Math.round(r.height) };
    });
    return boxes;
  });
}

async function openChannel(page: Page, skin: Skin) {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, preloadFor(skin));
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP_ID}`).click();
  // Measure from the channel click on: the sidebar's own reads are not this ticket's.
  await page.evaluate(() => {
    (window as unknown as { __resetTauriInvokeCounts: () => void }).__resetTauriInvokeCounts();
  });
  await page.addScriptTag({ content: `(${recordFirstPaint.toString()})(); document.dispatchEvent(new Event("DOMContentLoaded"));` });
  await page.getByTestId(`channel-option-${CHANNEL_ID}`).click();
  await expect(page.getByTestId("message-content").first()).toBeVisible();
}

for (const skin of SKINS) {
  test.describe(`reactions land with their page — ${skin} skin`, () => {
    test("one batched reactions read for the page, no per-row reads", async ({ page }) => {
      await openChannel(page, skin);
      await expect(page.getByTestId("message-m4").getByTestId("reaction-pill")).toHaveCount(2);
      // Let the background ingest and its refetch run.
      await page.waitForTimeout(1_000);
      const counts = await page.evaluate(
        () => (window as unknown as { __tauriInvokeCounts: Record<string, number> }).__tauriInvokeCounts,
      );
      expect(counts.get_reactions_for_messages ?? 0).toBe(1);
      expect(counts.get_reactions ?? 0).toBe(0);
      const args = await page.evaluate(
        () =>
          (window as unknown as { __tauriLastArgs: Record<string, { messageIds: string[] }> })
            .__tauriLastArgs.get_reactions_for_messages.messageIds,
      );
      expect([...args].sort()).toEqual([...ids].sort());
    });

    test("pills are there on first paint and no row moves afterwards", async ({ page }) => {
      await openChannel(page, skin);
      const { first, pills } = await page.evaluate(() => {
        const w = window as unknown as {
          __firstPaintBoxes?: Record<string, { offset: number; height: number }>;
          __firstPaintPills?: number;
        };
        return { first: w.__firstPaintBoxes, pills: w.__firstPaintPills };
      });
      expect(first, "first paint recorded").toBeTruthy();
      const total = Object.values(REACTIONS).reduce((n, list) => n + list.length, 0);
      expect(pills, "every pill drawn with the first paint").toBe(total);
      await page.waitForTimeout(1_000);
      const later = await boxesNow(page);
      for (const [id, box] of Object.entries(first!)) {
        expect(later[id], `${id} still rendered`).toBeTruthy();
        expect(later[id], `${id} kept its place and size`).toEqual(box);
      }
      for (const id of Object.keys(REACTIONS)) {
        await expect(page.getByTestId(`message-${id}`).getByTestId("reaction-pill")).toHaveCount(
          REACTIONS[id].length,
        );
      }
    });
  });
}
