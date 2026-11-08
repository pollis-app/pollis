/** Copy message history between the account's own devices (#1735), desktop half; the protocol is Rust's. */

import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };

const MAC = {
  device_id: "dev_mac",
  device_name: "MacBook",
  online: true,
  last_active_at: "2026-11-08T09:00:00Z",
  supports_transfer: true,
};
const IPAD = {
  device_id: "dev_ipad",
  device_name: "iPad",
  online: false,
  last_active_at: "2026-11-01T09:00:00Z",
  supports_transfer: true,
};
const OLD_PHONE = {
  device_id: "dev_old",
  device_name: "Old phone",
  online: true,
  last_active_at: "2026-11-08T08:00:00Z",
  supports_transfer: false,
};

const SIBLING_REQUEST = {
  transfer_id: "ht_req",
  requester_device_id: "dev_pixel",
  requester_device_name: "Pixel 9",
  requester_added_at: "2026-11-02T12:00:00Z",
  via: "sibling",
  pin_required: true,
  created_at: "2026-11-08T10:00:00Z",
};

function signedIn(extra: Record<string, unknown> = {}) {
  return {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [],
    channels: {},
    messages: {},
    dmChannels: [],
    preferences: { skin: "terminal" },
    historySources: [IPAD, OLD_PHONE, MAC],
    ...extra,
  };
}

async function boot(page: Page, data: Record<string, unknown>) {
  await page.addInitScript((d) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = d;
  }, data);
  await page.goto("/");
}

type HistoryMock = {
  historyRuns: string[];
  historyRequests: Array<{ sourceDeviceId: string; via: string }>;
  historyApprovals: Array<{ transferId: string; pin: string | null }>;
  historyDeclines: string[];
  historyCancels: string[];
  historyPending: unknown[];
};

async function mock(page: Page): Promise<HistoryMock> {
  return page.evaluate(() => {
    const m = (window as unknown as { __tauriMock: HistoryMock }).__tauriMock;
    return JSON.parse(
      JSON.stringify({
        historyRuns: m.historyRuns,
        historyRequests: m.historyRequests,
        historyApprovals: m.historyApprovals,
        historyDeclines: m.historyDeclines,
        historyCancels: m.historyCancels,
        historyPending: m.historyPending,
      }),
    ) as HistoryMock;
  });
}

/** What the other device would report next, streamed into the open run. */
async function step(page: Page, transferId: string, frame: Record<string, unknown>) {
  await page.evaluate(
    ({ transferId, frame }) =>
      (window as unknown as { __historyTransferStep: (id: string, f: unknown) => void }).__historyTransferStep(
        transferId,
        frame,
      ),
    { transferId, frame },
  );
}

async function nudge(page: Page, transferId: string, event: string) {
  await page.evaluate(
    ({ transferId, event }) =>
      (window as unknown as { __emitRealtimeEvent: (e: unknown) => number }).__emitRealtimeEvent({
        type: "history_transfer",
        transfer_id: transferId,
        event,
      }),
    { transferId, event },
  );
}

async function openPage(page: Page, query: string, path: string, testId: string) {
  await expect(page.getByTestId("sidebar")).toBeVisible();
  await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
  await expect(page.getByTestId("search-panel")).toBeVisible();
  await page.getByTestId("search-panel-input").fill(query);
  await page.getByTestId("search-panel-result-item").filter({ hasText: path }).first().click();
  await expect(page.getByTestId(testId)).toBeVisible();
}

async function openCopyHistory(page: Page) {
  await openPage(page, "Copy Message History", "/security/copy-history", "copy-history-page");
}

/** Fill an InputOtp one cell at a time (per-key typing races its focus move). */
async function fillOtp(page: Page, scope: string, digits: string) {
  const cells = page.getByTestId(scope).locator("input:not([type=hidden])");
  for (let i = 0; i < digits.length; i++) {
    await cells.nth(i).fill(digits[i]);
    if (i < digits.length - 1) {
      await expect(cells.nth(i)).not.toHaveValue("");
    }
  }
}

/** On the copy page: ask the MacBook; returns once the request is out and waiting. */
async function requestFromMac(page: Page) {
  await openCopyHistory(page);
  await page.getByTestId("history-request-start").click();
  const status = page.getByTestId("history-transfer-status");
  await expect(status).toHaveAttribute("data-phase", "waiting");
  await expect(page.getByTestId("history-transfer-title")).toHaveText("Approve on MacBook");
  await expect(page.getByTestId("history-transfer-body")).toHaveText("Open Pollis there.");
  return status;
}

test("link time: the new device offers the approver's history, then shows it arriving", async ({ page }) => {
  await boot(page, {
    session: null,
    profile: null,
    groups: [],
    channels: {},
    messages: {},
    dmChannels: [],
    preferences: { skin: "terminal" },
    enrollmentWait: "approved",
    historySources: [IPAD, { ...MAC, approved_this_device: true }],
  });
  await page.getByTestId("sign-in-with-device-button").click();
  await page.getByTestId("link-code-input").fill("pollis-link:v1:link_mock_1:BQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQU");
  await page.getByTestId("link-sign-in-button").click();

  await expect(page.getByTestId("pin-create-screen")).toBeVisible();
  await fillOtp(page, "pin-create-screen", "2468");
  await expect(page.getByTestId("pin-create-submit")).toHaveText("Save PIN");
  await fillOtp(page, "pin-create-screen", "2468");

  // Before the app opens: named after the device that approved the link, nothing asked yet.
  await expect(page.getByTestId("history-offer-screen")).toBeVisible();
  await expect(page.getByTestId("history-offer-title")).toHaveText("Copy your message history from MacBook?");
  await expect(page.getByTestId("app-ready")).toHaveCount(0);
  expect((await mock(page)).historyRequests).toEqual([]);

  await page.getByTestId("history-offer-copy").click();
  await expect(page.getByTestId("history-transfer-title")).toHaveText("Approve on MacBook");
  expect((await mock(page)).historyRequests).toEqual([{ sourceDeviceId: "dev_mac", via: "link" }]);
  await expect.poll(async () => (await mock(page)).historyRuns).toEqual(["ht_1"]);

  await step(page, "ht_1", { state: "transferring", rows_done: 120, chunks_done: 1 });
  await expect(page.getByTestId("history-transfer-title")).toHaveText("Copying message history from MacBook");
  await expect(page.getByTestId("history-transfer-counts")).toHaveText("120 messages copied · 1 batch");
  await step(page, "ht_1", { rows_done: 340, chunks_done: 2, estimated_total: 680 });
  await expect(page.getByRole("progressbar")).toHaveAttribute("aria-valuenow", "50");
  await step(page, "ht_1", { state: "completed", rows_done: 512, chunks_done: 3 });
  await expect(page.getByTestId("history-transfer-title")).toHaveText("Copied 512 messages");

  await page.getByTestId("history-offer-continue").click();
  await expect(page.getByTestId("app-ready")).toBeAttached();
});

test("link time: Not now goes straight into the app and asks nothing", async ({ page }) => {
  await boot(page, {
    session: null,
    profile: null,
    groups: [],
    channels: {},
    messages: {},
    dmChannels: [],
    preferences: { skin: "terminal" },
    enrollmentWait: "approved",
    historySources: [MAC, IPAD],
  });
  await page.getByTestId("sign-in-with-device-button").click();
  await page.getByTestId("link-code-input").fill("pollis-link:v1:link_mock_1:BQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQU");
  await page.getByTestId("link-sign-in-button").click();
  await expect(page.getByTestId("pin-create-screen")).toBeVisible();
  await fillOtp(page, "pin-create-screen", "2468");
  await expect(page.getByTestId("pin-create-submit")).toHaveText("Save PIN");
  await fillOtp(page, "pin-create-screen", "2468");

  // No approver named: the user picks, from a vertical list.
  await expect(page.getByTestId("history-offer-title")).toHaveText("Copy your message history from another device?");
  await expect(page.getByRole("radiogroup")).toBeVisible();
  await page.getByTestId("history-offer-not-now").click();
  await expect(page.getByTestId("app-ready")).toBeAttached();
  expect((await mock(page)).historyRequests).toEqual([]);
});

test("the device list puts online first and disables a device that can't send", async ({ page }) => {
  await boot(page, signedIn());
  await openCopyHistory(page);
  const rows = page.getByTestId("history-source-list").getByRole("radio");
  await expect(rows).toHaveCount(3);
  await expect(rows.nth(0)).toContainText("MacBook");
  await expect(rows.nth(0)).toContainText("Online");
  await expect(rows.nth(0)).toHaveAttribute("aria-checked", "true");
  await expect(rows.nth(1)).toContainText("iPad");
  await expect(rows.nth(1)).toContainText("Last active");
  await expect(rows.nth(2)).toContainText("Old phone");
  await expect(rows.nth(2)).toContainText("Update Pollis on this device");
  await expect(rows.nth(2)).toBeDisabled();

  await rows.nth(1).click();
  await expect(rows.nth(1)).toHaveAttribute("aria-checked", "true");
  await page.getByTestId("history-request-start").click();
  await expect(page.getByTestId("history-transfer-title")).toHaveText("Approve on iPad");
  expect((await mock(page)).historyRequests).toEqual([{ sourceDeviceId: "dev_ipad", via: "sibling" }]);
});

test("a declined request says so and offers to ask again", async ({ page }) => {
  await boot(page, signedIn());
  await requestFromMac(page);
  await step(page, "ht_1", { state: "declined" });
  await expect(page.getByTestId("history-transfer-title")).toHaveText("Declined on MacBook");
  await expect(page.getByTestId("history-transfer-body")).toHaveText("Nothing was copied.");
  await expect(page.getByTestId("history-transfer-cancel")).toHaveCount(0);
  await expect(page.getByTestId("history-transfer-retry")).toBeVisible();

  // Done returns to the picker.
  await page.getByTestId("history-transfer-done").click();
  await expect(page.getByTestId("history-source-list")).toBeVisible();
});

test("paused waits for the other device, and resumes on its nudge or on Resume", async ({ page }) => {
  await boot(page, signedIn());
  await requestFromMac(page);
  await step(page, "ht_1", { state: "transferring", rows_done: 40, chunks_done: 1 });
  await step(page, "ht_1", { state: "paused" });
  await expect(page.getByTestId("history-transfer-title")).toHaveText("Waiting for MacBook");
  await expect(page.getByTestId("history-transfer-body")).toHaveText("Keep Pollis open on both devices.");
  await expect(page.getByTestId("history-transfer-counts")).toHaveText("40 messages copied · 1 batch");
  await expect(page.getByTestId("history-transfer-retry")).toHaveText("Resume");

  // The MacBook came back and sent a chunk: the run picks up without a click.
  await nudge(page, "ht_1", "chunk");
  await expect.poll(async () => (await mock(page)).historyRuns).toEqual(["ht_1", "ht_1"]);
  await step(page, "ht_1", { state: "transferring", rows_done: 80, chunks_done: 2 });
  await expect(page.getByTestId("history-transfer-title")).toHaveText("Copying message history from MacBook");

  // Paused again; this time the user resumes.
  await step(page, "ht_1", { state: "paused" });
  await page.getByTestId("history-transfer-retry").click();
  await expect.poll(async () => (await mock(page)).historyRuns.length).toBe(3);
  await step(page, "ht_1", { state: "completed", rows_done: 90, chunks_done: 3 });
  await expect(page.getByTestId("history-transfer-title")).toHaveText("Copied 90 messages");
});

test("a failed copy shows why, and Try again asks the same device", async ({ page }) => {
  await boot(page, signedIn());
  await requestFromMac(page);
  await step(page, "ht_1", { state: "transferring", rows_done: 10, chunks_done: 1 });
  await step(page, "ht_1", { state: "failed", error: "The other device stopped answering." });
  await expect(page.getByTestId("history-transfer-title")).toHaveText("Couldn't copy message history");
  await expect(page.getByTestId("history-transfer-body")).toHaveText("The other device stopped answering.");

  await page.getByTestId("history-transfer-retry").click();
  await expect(page.getByTestId("history-transfer-title")).toHaveText("Approve on MacBook");
  expect((await mock(page)).historyRequests).toHaveLength(2);
  await step(page, "ht_2", { state: "completed", rows_done: 75, chunks_done: 1 });
  await expect(page.getByTestId("history-transfer-title")).toHaveText("Copied 75 messages");
});

test("conversations that weren't ready yet are named, with a retry", async ({ page }) => {
  await boot(page, signedIn());
  await requestFromMac(page);
  await step(page, "ht_1", { state: "completed", rows_done: 300, chunks_done: 2, deferred_conversations: 2 });
  await expect(page.getByTestId("history-transfer-title")).toHaveText("Copied 300 messages");
  await expect(page.getByTestId("history-transfer-body")).toHaveText(
    "2 conversations weren't ready yet. Try again later.",
  );
  await expect(page.getByTestId("history-transfer-retry")).toHaveText("Try again");
});

test("cancel stops the copy", async ({ page }) => {
  await boot(page, signedIn());
  await requestFromMac(page);
  await step(page, "ht_1", { state: "transferring", rows_done: 5, chunks_done: 1 });
  await page.getByTestId("history-transfer-cancel").click();
  await expect(page.getByTestId("history-transfer-title")).toHaveText("Copy cancelled");
  expect((await mock(page)).historyCancels).toEqual(["ht_1"]);
  await expect(page.getByTestId("history-transfer-cancel")).toHaveCount(0);
});

test("a pending request opens the approval page; a wrong PIN approves nothing", async ({ page }) => {
  await boot(page, signedIn({ historyPending: [SIBLING_REQUEST] }));
  await expect(page.getByTestId("app-ready")).toBeAttached();

  // Landing finds the open request and puts it in front of the user — a page, not an overlay.
  await expect(page.getByTestId("history-approval-page")).toBeVisible();
  await expect(page.getByTestId("history-approval-title")).toHaveText("Pixel 9 wants to copy your message history");
  await expect(page.getByTestId("history-approval-added")).toContainText("Added");
  await expect(page.getByText("It will be able to read all your past messages.")).toBeVisible();
  await expect(page.getByTestId("history-approval-approve")).toBeDisabled();

  await fillOtp(page, "history-approval-pin", "9999");
  await page.getByTestId("history-approval-approve").click();
  await expect(page.getByTestId("history-approval-error")).toHaveText("That PIN isn't right.");
  expect((await mock(page)).historyPending).toHaveLength(1);

  await fillOtp(page, "history-approval-pin", "1234");
  await page.getByTestId("history-approval-approve").click();
  await expect(page.getByTestId("history-transfer-title")).toHaveText("Sending message history to Pixel 9");
  const calls = await mock(page);
  expect(calls.historyApprovals).toEqual([
    { transferId: "ht_req", pin: "9999" },
    { transferId: "ht_req", pin: "1234" },
  ]);
  await expect.poll(async () => (await mock(page)).historyRuns).toEqual(["ht_req"]);
  await step(page, "ht_req", { rows_done: 64, chunks_done: 1 });
  await expect(page.getByTestId("history-transfer-counts")).toHaveText("64 messages sent · 1 batch");
  await step(page, "ht_req", { state: "completed", rows_done: 64, chunks_done: 1 });
  await expect(page.getByTestId("history-transfer-title")).toHaveText("Sent 64 messages to Pixel 9");
});

test("a request nudged in later opens the approval page; Decline sends nothing", async ({ page }) => {
  await boot(page, signedIn());
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.evaluate((request) => {
    (window as unknown as { __tauriMock: { historyPending: unknown[] } }).__tauriMock.historyPending.push(request);
  }, { ...SIBLING_REQUEST, pin_required: false });
  await nudge(page, "ht_req", "requested");

  await expect(page.getByTestId("history-approval-page")).toBeVisible();
  await expect(page.getByTestId("history-approval-pin")).toHaveCount(0);
  await page.getByTestId("history-approval-decline").click();
  await expect(page.getByTestId("history-approval-declined")).toBeVisible();
  const calls = await mock(page);
  expect(calls.historyDeclines).toEqual(["ht_req"]);
  expect(calls.historyApprovals).toEqual([]);
  expect(calls.historyRuns).toEqual([]);
});

test("the Linked page answers the new device's request inline", async ({ page }) => {
  await boot(
    page,
    signedIn({
      deviceLinkPin: "1234",
      deviceLinkScript: [
        { state: "ready_to_approve", device_name: "Pixel 9", new_device_id: "dev_pixel", request_id: "req_1" },
      ],
    }),
  );
  await openPage(page, "Security", "/security", "security-page");
  await page.getByTestId("link-device-button").click();
  await fillOtp(page, "link-device-pin-input", "1234");
  await page.getByTestId("link-device-approve").click();
  await expect(page.getByTestId("link-device-done")).toContainText("Pixel 9 is signed in");

  await page.evaluate((request) => {
    (window as unknown as { __tauriMock: { historyPending: unknown[] } }).__tauriMock.historyPending.push(request);
  }, { ...SIBLING_REQUEST, via: "link", pin_required: false });
  await nudge(page, "ht_req", "requested");

  // Answered here, single tap: no PIN (it was just entered for the link), no jump to another page.
  const inline = page.getByTestId("linked-history-request");
  await expect(inline).toBeVisible();
  await expect(page.getByTestId("history-approval-pin")).toHaveCount(0);
  await expect(page.getByTestId("link-device-page")).toBeVisible();
  await inline.getByTestId("history-approval-approve").click();
  await expect(page.getByTestId("history-transfer-title")).toHaveText("Sending message history to Pixel 9");
  expect((await mock(page)).historyApprovals).toEqual([{ transferId: "ht_req", pin: null }]);
});

test("unfinished transfers resume on their own after unlock", async ({ page }) => {
  await boot(
    page,
    signedIn({
      historyTransfers: [
        {
          transfer_id: "ht_open",
          role: "receiver",
          peer_device_id: "dev_mac",
          peer_device_name: "MacBook",
          via: "sibling",
          state: "paused",
          chunks_done: 2,
          rows_done: 200,
          rows_skipped: 0,
          deferred_conversations: 0,
          updated_at: "2026-11-08T09:00:00Z",
        },
        {
          transfer_id: "ht_old",
          role: "receiver",
          peer_device_id: "dev_ipad",
          peer_device_name: "iPad",
          via: "sibling",
          state: "completed",
          chunks_done: 1,
          rows_done: 10,
          rows_skipped: 0,
          deferred_conversations: 0,
          updated_at: "2026-11-01T09:00:00Z",
        },
      ],
    }),
  );
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await expect.poll(async () => (await mock(page)).historyRuns).toEqual(["ht_open"]);

  // Security shows it under way; the copy page follows it.
  await openPage(page, "Security", "/security", "security-page");
  await expect(page.getByTestId("history-active-ht_open")).toContainText("Copying from MacBook");
  await page.getByTestId("copy-history-button").click();
  await expect(page.getByTestId("copy-history-page")).toBeVisible();
  await step(page, "ht_open", { state: "transferring", rows_done: 260, chunks_done: 3 });
  await expect(page.getByTestId("history-transfer-counts")).toHaveText("260 messages copied · 3 batches");
});

test("history-transfer security events read as device events", async ({ page }) => {
  await boot(
    page,
    signedIn({
      securityEvents: [
        {
          id: "ev1",
          kind: "history_transfer_approved",
          device_id: "dev_pixel_0123456789",
          created_at: "2026-11-08T10:00:00Z",
          metadata: "via=sibling,to=dev_pixel_0123456789",
        },
      ],
    }),
  );
  await openPage(page, "Security", "/security", "security-page");
  await expect(page.getByTestId("security-events-list")).toContainText("Message history copy approved");
});
