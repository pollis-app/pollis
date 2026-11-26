// The account-key line on a peer's profile (#2114) holds its slot from the first frame:
// "Checking key…" while the audit runs, then the verdict on the same single line, so the
// menu below never moves, whatever the audit says or if it fails.

import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u-alice", email: "alice@example.com", username: "alice" };
const DM_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0DMX";
const DM_MSG = "01HQ7Z3K9M2P5R8T1V4W6Y0DM1";
const AUDIT = "audit_peer_account_key";
const LONG_REASON =
  "the published chain disagrees with the key this device pinned for this person at the same version, which is far longer than one line";

interface Audit {
  status?: string;
  detail?: string;
  fail?: boolean;
}

function member(userId: string, username: string) {
  return { user_id: userId, username, added_by: ME.id, added_at: "" };
}

async function boot(page: Page, accountKeyAudit: Audit, hold = AUDIT) {
  await page.addInitScript((d) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = d;
  }, {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    preferences: JSON.stringify({ skin: "modern", right_panel_open_by_default: true }),
    groups: [],
    channels: {},
    dmChannels: [
      {
        id: DM_ID,
        created_by: ME.id,
        created_at: new Date().toISOString(),
        members: [member(ME.id, "alice"), member("u-bob", "bob")],
      },
    ],
    messages: {
      [DM_ID]: [
        { id: DM_MSG, conversation_id: DM_ID, sender_id: "u-bob", content: "hi", sent_at: "2026-08-01T10:00:00.000Z" },
      ],
    },
    accountKeyAudit,
    // The audit's reply waits for the spec, as a slow transparency log would.
    holdCommands: [hold],
  });
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
}

// Memory-history router: the DM is reached by permalink, the profile from its member list.
async function openBobsProfile(page: Page) {
  await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
  await expect(page.getByTestId("search-panel")).toBeVisible();
  await page.getByTestId("search-panel-input").fill(`pollis://m/${DM_ID}/${DM_MSG}`);
  await expect(page.getByTestId(`message-${DM_MSG}`)).toBeVisible();
  await page.getByTestId("right-panel-member-u-bob").click();
  await expect(page.getByTestId("user-profile-menu")).toBeVisible();
}

async function box(page: Page, testId: string) {
  const b = await page.getByTestId(testId).boundingBox();
  expect(b).not.toBeNull();
  return b!;
}

const CASES: Array<{ name: string; audit: Audit; status: string; label: RegExp }> = [
  { name: "ok", audit: { status: "ok" }, status: "ok", label: /^Key publicly verified$/ },
  { name: "unavailable", audit: { status: "unavailable", detail: "log unreachable" }, status: "unavailable", label: /^Verification unavailable$/ },
  { name: "a failed audit", audit: { fail: true }, status: "unavailable", label: /^Verification unavailable$/ },
  { name: "alarm with a long reason", audit: { status: "alarm", detail: LONG_REASON }, status: "alarm", label: /^Key verification failed/ },
];

for (const c of CASES) {
  test(`the menu does not move when the audit lands: ${c.name}`, async ({ page }) => {
    await boot(page, c.audit);
    await openBobsProfile(page);

    const line = page.getByTestId("peer-account-key-audit");
    await expect(line).toHaveAttribute("data-status", "checking");
    await expect(line).toHaveText("Checking key…");
    const menuBefore = await box(page, "user-profile-menu");
    const lineBefore = await box(page, "peer-account-key-audit");

    await page.evaluate((cmd) => {
      (window as unknown as { __tauriRelease: (c: string) => void }).__tauriRelease(cmd);
    }, AUDIT);

    // A failed audit is retried once by React Query before it settles.
    await expect(line).toHaveAttribute("data-status", c.status, { timeout: 10_000 });
    await expect(line).toHaveText(c.label);
    const menuAfter = await box(page, "user-profile-menu");
    const lineAfter = await box(page, "peer-account-key-audit");
    expect(menuAfter.y).toBe(menuBefore.y);
    expect(lineAfter.height).toBe(lineBefore.height);
  });
}

test("an alarm's reason stays on the line and in full on hover", async ({ page }) => {
  await boot(page, { status: "alarm", detail: LONG_REASON });
  await openBobsProfile(page);
  await page.evaluate((cmd) => {
    (window as unknown as { __tauriRelease: (c: string) => void }).__tauriRelease(cmd);
  }, AUDIT);
  const line = page.getByTestId("peer-account-key-audit");
  await expect(line).toHaveAttribute("data-status", "alarm");
  await expect(page.getByTestId("peer-account-key-audit-reason")).toContainText("published chain disagrees");
  await expect(line.locator("[title]")).toHaveAttribute("title", `Key verification failed: ${LONG_REASON}`);
});

test("the Security page's own key line holds its slot the same way", async ({ page }) => {
  const SELF_AUDIT = "self_audit_account_key";
  await boot(page, { status: "ok" }, SELF_AUDIT);
  await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
  await page.getByTestId("search-panel-input").fill("Security");
  await page.locator('[data-testid="search-panel-result-item"][data-item-id="page-security"]').click();
  await expect(page.getByTestId("security-page")).toBeVisible();

  const line = page.getByTestId("self-account-key-audit");
  await expect(line).toHaveAttribute("data-status", "checking");
  await expect(line).toHaveText("Checking key…");
  const nextBefore = await box(page, "this-build-section");

  await page.evaluate((cmd) => {
    (window as unknown as { __tauriRelease: (c: string) => void }).__tauriRelease(cmd);
  }, SELF_AUDIT);
  await expect(line).toHaveAttribute("data-status", "ok");
  await expect(line).toHaveText("Key publicly verified");
  expect((await box(page, "this-build-section")).y).toBe(nextBefore.y);
});
