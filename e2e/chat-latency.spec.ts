// Chat action latency (#2141): send, reply, edit, delete, pin and react, each measured from the
// tap to the first commit that shows it, to the reply that confirms it, with the rows it
// re-rendered and the commands it called. Every reply is held back by an injected IPC latency
// (`__tauriSetLatency`), as a round trip to the DS would, so an action that waits on the network
// before it draws shows that wait here.

import { test, expect, type Page } from "./lib/zero-loss";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };
const BOB = { id: "u-bob", username: "bob" };

const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y2GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y2XCB";
/** A second channel, for what must not leak between conversations. */
const CHANNEL_B = "01HQ7Z3K9M2P5R8T1V4W6Y2XCC";

/** What every reply waits, in ms: well past a frame, so a wait on it cannot hide. */
const LATENCY_MS = 150;
/** Runs per action. */
const RUNS = 8;
/** A change is "in the same frame" when it commits this soon after the tap. */
const FRAME_MS = 16.7;

// Few enough rows, on a tall enough page, that the windowed log renders every one of them.
const ROWS = 8;
const ownId = (i: number) => `01HQ7Z3K9M2P5R8T1V4W6Y2O${String(i).padStart(2, "0")}`;
const bobId = (i: number) => `01HQ7Z3K9M2P5R8T1V4W6Y2B${String(i).padStart(2, "0")}`;

function messages() {
  const out = [];
  for (let i = 0; i < ROWS; i++) {
    out.push({
      id: bobId(i),
      conversation_id: CHANNEL_ID,
      sender_id: BOB.id,
      sender_username: BOB.username,
      content: `bob says ${i}`,
      sent_at: `2026-08-01T10:${String(i * 2).padStart(2, "0")}:00.000Z`,
    });
    out.push({
      id: ownId(i),
      conversation_id: CHANNEL_ID,
      sender_id: USER.id,
      sender_username: USER.username,
      content: `alice says ${i}`,
      sent_at: `2026-08-01T10:${String(i * 2 + 1).padStart(2, "0")}:00.000Z`,
    });
  }
  return out;
}

function preloadState() {
  const reactions: Record<string, { emoji: string; user_ids: string[]; count: number }[]> = {};
  for (let i = 0; i < ROWS; i++) {
    reactions[bobId(i)] = [{ emoji: "👍", user_ids: [BOB.id], count: 1 }];
  }
  return {
    session: USER,
    profile: { id: USER.id, username: USER.username },
    preferences: JSON.stringify({ skin: "modern" }),
    groups: [{ id: GROUP_ID, name: "Acme", owner_id: USER.id, created_at: new Date().toISOString() }],
    channels: {
      [GROUP_ID]: [
        { id: CHANNEL_ID, group_id: GROUP_ID, name: "general" },
        { id: CHANNEL_B, group_id: GROUP_ID, name: "random" },
      ],
    },
    groupMembers: {
      [GROUP_ID]: [
        { user_id: USER.id, username: USER.username, role: "admin" },
        { user_id: BOB.id, username: BOB.username, role: "member" },
      ],
    },
    dmChannels: [],
    messages: { [CHANNEL_ID]: messages(), [CHANNEL_B]: [] },
    reactions,
    bookmarks: [],
  };
}

async function boot(page: Page, extra: Record<string, unknown> = {}) {
  await page.addInitScript((preload) => {
    const w = window as unknown as Record<string, unknown>;
    w.__POLLIS_PRELOAD__ = preload;
    w.__TAURI_INTERNALS__ = {
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
    // The tap: capture phase, so it is stamped before the app's own handler runs.
    const perf = { tap: 0 };
    w.__perfTap = perf;
    document.addEventListener("pointerdown", () => (perf.tap = performance.now()), true);
    document.addEventListener(
      "keydown",
      (e) => {
        if (e.key === "Enter") {
          perf.tap = performance.now();
        }
      },
      true,
    );
  }, { ...preloadState(), ...extra });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP_ID}`).click();
  await page.getByTestId(`channel-option-${CHANNEL_ID}`).click();
  await expect(page.getByTestId(`message-${ownId(ROWS - 1)}`)).toBeVisible();
  await page.evaluate((ms) => {
    (window as unknown as { __tauriSetLatency: (m: Record<string, number>) => void }).__tauriSetLatency({ "*": ms });
  }, LATENCY_MS);
}

interface Sample {
  visibleMs: number;
  confirmedMs: number;
  rows: number;
  invokes: number;
}

/**
 * Arm a watch for `predicate` (a function body over `document`), run `act`, and time it.
 * `confirm` names the command whose reply confirms the action.
 */
async function measure(page: Page, predicate: string, confirm: string, act: () => Promise<void>): Promise<Sample> {
  await page.evaluate((body) => {
    const w = window as unknown as Record<string, any>;
    const check = new Function("document", body) as (d: Document) => boolean;
    const watch = { seenAt: 0, renders: { ...(w.__pollisRenders ?? {}) }, invokes: { ...(w.__tauriInvokeCounts ?? {}) }, log: w.__tauriInvokeLog.length };
    w.__perfWatch = watch;
    const observer = new MutationObserver(() => {
      if (!watch.seenAt && check(document)) {
        watch.seenAt = performance.now();
        observer.disconnect();
      }
    });
    observer.observe(document.body, { subtree: true, childList: true, characterData: true, attributes: true });
  }, predicate);
  await act();
  await expect.poll(() => page.evaluate(() => (window as any).__perfWatch.seenAt > 0), { timeout: 10_000 }).toBe(true);
  // Confirmed: the action's command replied.
  await expect
    .poll(
      () =>
        page.evaluate((cmd) => {
          const w = window as any;
          return w.__tauriInvokeLog.slice(w.__perfWatch.log).some((e: any) => e.command === cmd && e.settledAt !== null);
        }, confirm),
      { timeout: 10_000 },
    )
    .toBe(true);
  // Settled: whatever the reply set off has run too.
  await page.waitForTimeout(LATENCY_MS * 3);
  return await page.evaluate((cmd) => {
    const w = window as any;
    const watch = w.__perfWatch;
    const tap = w.__perfTap.tap;
    const settled = w.__tauriInvokeLog
      .slice(watch.log)
      .find((e: any) => e.command === cmd && e.settledAt !== null);
    const sum = (now: Record<string, number>, before: Record<string, number>, only?: string) =>
      Object.keys(now)
        .filter((k) => !only || k === only)
        .reduce((n, k) => n + (now[k] - (before[k] ?? 0)), 0);
    return {
      visibleMs: watch.seenAt - tap,
      confirmedMs: settled.settledAt - tap,
      rows: sum(w.__pollisRenders ?? {}, watch.renders, "MessageItem"),
      invokes: sum(w.__tauriInvokeCounts ?? {}, watch.invokes),
    };
  }, confirm);
}

function pct(values: number[], p: number): number {
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.ceil((p / 100) * sorted.length) - 1)];
}

function report(action: string, samples: Sample[]) {
  const v = samples.map((s) => s.visibleMs);
  const c = samples.map((s) => s.confirmedMs);
  const line = {
    action,
    runs: samples.length,
    visibleP50: Math.round(pct(v, 50) * 10) / 10,
    visibleP95: Math.round(pct(v, 95) * 10) / 10,
    confirmedP50: Math.round(pct(c, 50)),
    confirmedP95: Math.round(pct(c, 95)),
    rowsP50: pct(samples.map((s) => s.rows), 50),
    invokesP50: pct(samples.map((s) => s.invokes), 50),
  };
  console.log(`[perf] desktop ${JSON.stringify(line)}`);
  return line;
}

const contentIs = (rowId: string, text: string) =>
  `const row = document.querySelector('[data-testid="message-${rowId}"]');
   return !!row && (row.querySelector('[data-testid="message-content"]')?.textContent ?? '').includes(${JSON.stringify(text)});`;

/** Open a channel of the group by name, through the Cmd+K panel. */
async function switchChannel(page: Page, name: string) {
  await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
  await expect(page.getByTestId("search-panel")).toBeVisible();
  await page.getByTestId("search-panel-input").fill(name);
  await page.keyboard.press("Enter");
}

async function openMenu(page: Page, rowId: string) {
  const row = page.getByTestId(`message-${rowId}`);
  await row.scrollIntoViewIfNeeded();
  await row.hover();
  await row.getByTestId("message-actions-more").click();
}

test.use({ viewport: { width: 1280, height: 3200 } });

test.describe("chat action latency", () => {
  test("send, reply, edit, delete, pin and react", async ({ page }) => {
    test.setTimeout(240_000);
    await boot(page);
    const input = page.getByTestId("message-input");
    const results: Record<string, ReturnType<typeof report>> = {};

    // ── Send ────────────────────────────────────────────────────────────
    const send: Sample[] = [];
    for (let i = 0; i < RUNS; i++) {
      const text = `latency send ${i}`;
      await input.click();
      await page.keyboard.type(text);
      send.push(
        await measure(
          page,
          `return [...document.querySelectorAll('[data-testid="message-content"]')].some((n) => (n.textContent ?? '').includes(${JSON.stringify(text)}));`,
          "send_message",
          () => page.keyboard.press("Enter"),
        ),
      );
    }
    results.send = report("send", send);

    // ── Reply: the quote is drawn with the new row ─────────────────────────
    const reply: Sample[] = [];
    for (let i = 0; i < RUNS; i++) {
      const target = bobId(ROWS - 1 - i);
      const text = `latency reply ${i}`;
      const row = page.getByTestId(`message-${target}`);
      await row.scrollIntoViewIfNeeded();
      await row.hover();
      await row.getByTestId("reply-button").click();
      await page.keyboard.type(text);
      reply.push(
        await measure(
          page,
          `return !!document.querySelector('[data-testid="reply-preview-${target}"]') &&
             [...document.querySelectorAll('[data-testid="message-content"]')].some((n) => (n.textContent ?? '').includes(${JSON.stringify(text)}));`,
          "send_message",
          () => page.keyboard.press("Enter"),
        ),
      );
    }
    results.reply = report("reply", reply);

    // ── Edit: the new text shows and the bar is gone ──────────────────────
    const edit: Sample[] = [];
    for (let i = 0; i < RUNS; i++) {
      const target = ownId(ROWS - 1 - i);
      const text = `edited ${i}`;
      const row = page.getByTestId(`message-${target}`);
      await row.scrollIntoViewIfNeeded();
      await row.hover();
      await row.getByTestId("edit-button").click();
      const bar = page.getByTestId("edit-message-bar-input");
      await expect(bar).toBeVisible();
      await bar.fill(text);
      edit.push(
        await measure(
          page,
          `${contentIs(target, text).replace("return ", "const shown = ")}
           return shown && !document.querySelector('[data-testid="edit-message-bar"]');`,
          "edit_message",
          () => bar.press("Enter"),
        ),
      );
    }
    results.edit = report("edit", edit);

    // ── Delete: the row says deleted and the bar is gone ───────────────────
    const del: Sample[] = [];
    for (let i = 0; i < RUNS; i++) {
      const target = ownId(i);
      await openMenu(page, target);
      await page.getByTestId("delete-button").click();
      const confirm = page.getByTestId("delete-message-confirm");
      await expect(confirm).toBeVisible();
      del.push(
        await measure(
          page,
          `${contentIs(target, `alice says ${i}`).replace("return ", "const still = ")}
           return !still && !document.querySelector('[data-testid="delete-message-bar"]');`,
          "delete_message",
          () => confirm.click(),
        ),
      );
    }
    results.delete = report("delete", del);

    // ── Pin: the pinned list shows it ───────────────────────────────────
    await page.getByTestId("channel-pins-trigger").click();
    await expect(page.getByTestId("pinned-panel")).toBeVisible();
    const pin: Sample[] = [];
    for (let i = 0; i < RUNS; i++) {
      const target = bobId(i);
      await openMenu(page, target);
      pin.push(
        await measure(
          page,
          `return [...document.querySelectorAll('[data-testid="pinned-panel-text"]')].some((n) => (n.textContent ?? '').includes(${JSON.stringify(`bob says ${i}`)}));`,
          "pin_message",
          () => page.getByTestId("pin-button").click(),
        ),
      );
    }
    results.pin = report("pin", pin);

    // ── React: tap bob's 👍 and the count reads 2 ─────────────────────────
    const react: Sample[] = [];
    for (let i = 0; i < RUNS; i++) {
      const target = bobId(ROWS - 1 - i);
      const row = page.getByTestId(`message-${target}`);
      await row.scrollIntoViewIfNeeded();
      react.push(
        await measure(
          page,
          `const row = document.querySelector('[data-testid="message-${target}"]');
           return !!row && (row.querySelector('[data-testid="reaction-count"]')?.textContent ?? '').includes('2');`,
          "add_reaction",
          () => row.getByTestId("reaction-pill").first().click(),
        ),
      );
    }
    results.react = report("react", react);

    // Every action shows before its reply could have arrived (#2141): nothing waits on the
    // round trip to draw: a change that waited on it shows no sooner than the whole injected
    // latency. A dev build renders slowly and noisily, so the bound is on the p50; p95s are printed.
    for (const [action, r] of Object.entries(results)) {
      expect(r.visibleP50, `${action} draws before its reply`).toBeLessThan(LATENCY_MS);
    }
    // A send re-renders its own row and its neighbours' grouping (counts doubled by StrictMode),
    // never the whole log (50 before #2141).
    expect(results.send.rowsP50).toBeLessThanOrEqual(6);
    // No reaction re-renders a message row.
    expect(results.react.rowsP50).toBe(0);
    void FRAME_MS;
  });

  test("a message still being sent can be replied to, edited and deleted", async ({ page }) => {
    await boot(page);
    await page.evaluate(() => (window as any).__tauriHold("send_message"));
    const input = page.getByTestId("message-input");
    // A stub's row: `message-pending-…` until its send settles.
    const rowWith = (text: string) => page.locator('[data-testid^="message-pending-"]', { hasText: text }).last();

    await input.click();
    await page.keyboard.type("still going out");
    await page.keyboard.press("Enter");
    await expect(rowWith("still going out")).toBeVisible();
    // Pinned by its own id: the reply below quotes the same words.
    const pendingTestId = await rowWith("still going out").getAttribute("data-testid");
    const pending = page.getByTestId(pendingTestId!);
    // The stub carries the core's id from its first frame: no wait on new_message_id.
    const coreId = await page.evaluate(() => {
      const log = (window as any).__tauriInvokeLog as Array<{ command: string; args: { messageId?: string } }>;
      return log.filter((e) => e.command === "send_message").at(-1)?.args.messageId ?? null;
    });
    expect(coreId).toBeTruthy();
    expect(coreId).not.toMatch(/^pending-/);

    // Reply: the quote finds the pending original.
    await pending.hover();
    await expect(pending.getByTestId("reply-button")).toBeVisible();
    // Reactions wait until it is delivered.
    await expect(pending.getByTestId("reaction-add-btn")).toHaveCount(0);
    await pending.getByTestId("reply-button").click();
    await expect(page.getByTestId("reply-preview")).toBeVisible();
    await page.keyboard.type("a reply to it");
    await page.keyboard.press("Enter");
    const reply = rowWith("a reply to it");
    await expect(reply.getByTestId(`reply-preview-${coreId}`)).toBeVisible();
    const replyArgs = await page.evaluate(() => {
      const log = (window as any).__tauriInvokeLog as Array<{ command: string; args: { replyToId?: string } }>;
      return log.filter((e) => e.command === "send_message").at(-1)?.args ?? null;
    });
    expect(replyArgs?.replyToId).toBe(coreId);

    // Edit: the bar closes on submit and the row shows the new text.
    await pending.hover();
    await pending.getByTestId("edit-button").click();
    const bar = page.getByTestId("edit-message-bar-input");
    await bar.fill("still going out, fixed");
    await bar.press("Enter");
    await expect(page.getByTestId("edit-message-bar")).toHaveCount(0);
    await expect(rowWith("still going out, fixed")).toBeVisible();
    const editArgs = await page.evaluate(() => (window as any).__tauriLastArgs.edit_message);
    expect(editArgs.messageId).toBe(coreId);

    // Delete = discard: the pending reply goes away at once.
    await reply.hover();
    await reply.getByTestId("message-actions-more").click();
    await page.getByTestId("delete-button").click();
    await page.getByTestId("delete-message-confirm").click();
    await expect(page.getByTestId("delete-message-bar")).toHaveCount(0);
    await expect(page.getByTestId("message-content").filter({ hasText: "a reply to it" })).toHaveCount(0);
    expect(await page.evaluate(() => (window as any).__tauriInvokeCounts.discard_unsent_message)).toBe(1);

    await page.evaluate(() => (window as any).__tauriRelease("send_message"));
    await expect(page.getByTestId("message-content").filter({ hasText: "still going out, fixed" })).toBeVisible();
  });

  // Review fixes (#2141).

  test("a refused delete puts back its own row only, never another conversation's", async ({ page }) => {
    await boot(page);
    await page.evaluate(() => {
      const w = window as any;
      w.__tauriSetLatency({ delete_message: 4000 });
      w.__tauriRejectWith("delete_message", "network unreachable");
    });
    const target = ownId(ROWS - 1);
    await openMenu(page, target);
    await page.getByTestId("delete-button").click();
    await page.getByTestId("delete-message-confirm").click();
    await expect(page.getByTestId(`message-${target}`).getByTestId("message-content")).not.toContainText(`alice says ${ROWS - 1}`);

    // While the delete is still out, a send in another channel fails and shows Retry.
    await switchChannel(page, "random");
    await page.evaluate(() => {
      const w = window as any;
      w.__tauriFailSends(true);
      // Known offline: the send reads failed at once instead of retrying in place.
      w.__tauriSetOffline(true);
    });
    await page.getByTestId("message-input").click();
    await page.keyboard.type("sent while the delete was out");
    await page.keyboard.press("Enter");
    await expect(page.locator('[data-testid^="message-discard-"]')).toBeVisible();

    // The delete is refused: its row comes back in its own channel ...
    await expect(page.getByTestId("delete-message-error")).toBeVisible({ timeout: 10_000 });
    // ... and the failed send here keeps its row and its way out.
    await expect(page.locator('[data-testid^="message-discard-"]')).toBeVisible();
    await switchChannel(page, "general");
    await expect(page.getByTestId(`message-${target}`).getByTestId("message-content")).toContainText(`alice says ${ROWS - 1}`);
  });

  test("an edit and a delete in flight survive a refetch that reads the old row", async ({ page }) => {
    await boot(page);
    await page.evaluate(() => {
      const w = window as any;
      w.__tauriHold("edit_message");
      w.__tauriHold("delete_message");
    });
    const edited = ownId(ROWS - 1);
    const deleted = ownId(ROWS - 2);
    const row = page.getByTestId(`message-${edited}`);
    await row.hover();
    await row.getByTestId("edit-button").click();
    await page.getByTestId("edit-message-bar-input").fill("edited while a wake came in");
    await page.getByTestId("edit-message-bar-input").press("Enter");
    await openMenu(page, deleted);
    await page.getByTestId("delete-button").click();
    await page.getByTestId("delete-message-confirm").click();

    // The core has not stored either yet (#1555): put the store back, then a wake re-reads it.
    await page.evaluate(
      ({ channel, edited, deleted, editedText, deletedText }) => {
        const w = window as any;
        const rows = w.__tauriMock.messages[channel] as Array<Record<string, unknown>>;
        const e = rows.find((m) => m.id === edited)!;
        e.content = editedText;
        delete e.edited_at;
        const d = rows.find((m) => m.id === deleted)!;
        d.content = deletedText;
        delete d.deleted_at;
        w.__emitRealtimeEvent({ type: "new_message", channel_id: channel, conversation_id: null, sender_id: "u-bob" });
      },
      {
        channel: CHANNEL_ID,
        edited,
        deleted,
        editedText: `alice says ${ROWS - 1}`,
        deletedText: `alice says ${ROWS - 2}`,
      },
    );
    await expect
      .poll(() => page.evaluate(() => (window as any).__tauriInvokeCounts.read_channel_messages ?? 0))
      .toBeGreaterThan(1);
    await page.waitForTimeout(300);
    await expect(row.getByTestId("message-content")).toContainText("edited while a wake came in");
    await expect(page.getByTestId(`message-${deleted}`).getByTestId("message-content")).not.toContainText(`alice says ${ROWS - 2}`);
  });

  test("a stub without the core's id yet offers no actions", async ({ page }) => {
    // No id is ever ready: every stub waits on its own `new_message_id`.
    await boot(page, { holdCommands: ["new_message_id"] });
    // And the send stays out, so the row is still the stub when its id arrives.
    await page.evaluate(() => (window as any).__tauriHold("send_message"));
    await page.getByTestId("message-input").click();
    await page.keyboard.type("no id yet");
    await page.keyboard.press("Enter");
    const stub = page.locator('[data-testid^="message-pending-"]', { hasText: "no id yet" }).last();
    await expect(stub).toBeVisible();
    await stub.hover();
    await expect(stub.getByTestId("message-actions")).toHaveCount(0);

    // Once the core names it, reply, edit and delete are offered.
    await page.evaluate(() => (window as any).__tauriRelease("new_message_id"));
    const pendingTestId = await stub.getAttribute("data-testid");
    const named = page.getByTestId(pendingTestId!);
    await named.hover();
    await expect(named.getByTestId("reply-button")).toBeVisible();
  });
});
