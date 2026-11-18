/* Bot Apps (#2005): the pure rules behind the approval page — the plain-language manifest, the
 * grants an approval signs (never wider than the manifest, widened items only when switched on),
 * the bridge input, and the admin notice. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import {
  EMPTY_DRAFT,
  appApprovalErrorKey,
  appNotice,
  approvalBlocked,
  buildApproveInput,
  buildGrants,
  changeKey,
  changeText,
  effectiveGrants,
  formatInterval,
  formatQuota,
  grantOptions,
  isLinkSource,
  isStaleApprovalError,
  pendingKind,
  plainLines,
  sortChanges,
  widenGrants,
  type AdminGrants,
  type AppManifest,
  type BotAppView,
  type ManifestChange,
  type Translate,
} from "../lib/botApps.ts";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (rel: string) => readFileSync(join(ROOT, rel), "utf8");

// A catalogue stand-in: the key, then its options, so a line says which copy and values it used.
const t: Translate = (key, options) => {
  if (key === "mobile:bots.app.listSeparator") {
    return ", ";
  }
  return options ? `${key} ${JSON.stringify(options)}` : key;
};

function manifest(over: Partial<AppManifest> = {}): AppManifest {
  return {
    name: "triage",
    version: "1.0.0",
    description: "",
    triggers: ["messages", "mentions"],
    read: { channels: ["#alerts", "#ops"], history: 30, events: ["created"] },
    actions: ["post", "reply"],
    state: { quota: 4096 },
    egress: ["api.linear.app"],
    ...over,
  };
}

const CHANNELS = [
  { id: "c1", name: "alerts" },
  { id: "c2", name: "ops" },
  { id: "c3", name: "random" },
];

function grants(over: Partial<AdminGrants> = {}): AdminGrants {
  return {
    channels: [
      { id: "c1", name: "alerts" },
      { id: "c2", name: "ops" },
    ],
    history: 30,
    events: ["created"],
    actions: ["post", "reply"],
    egress: ["api.linear.app"],
    ...over,
  };
}

function view(over: Partial<BotAppView> = {}): BotAppView {
  return {
    botUserId: "bot1",
    groupId: "g1",
    approved: null,
    approvalRefusal: null,
    report: null,
    pending: null,
    channels: CHANNELS.slice(0, 2),
    latestSeq: 0,
    ...over,
  };
}

const approved = (m: AppManifest, g: AdminGrants, over = {}) => ({
  seq: 1,
  development: false,
  appHash: "a".repeat(64),
  developerKey: null,
  manifestHash: "m1",
  manifest: m,
  grants: g,
  source: "https://example.com/app.wasm",
  autoUpdate: false,
  approverUserId: "u1",
  issuedAt: 1_700_000_000,
  cannotSendOutside: g.egress.length === 0,
  ...over,
});

// --- Plain language ------------------------------------------------------------------------------

test("plain language describes the granted scope, one line per item", () => {
  const lines = plainLines(t, manifest(), grants());
  assert.deepEqual(
    lines.map((l) => l.id),
    ["reads", "events", "wakes", "can", "egress", "state"],
  );
  const by = Object.fromEntries(lines.map((l) => [l.id, l.text]));
  assert.match(by.reads, /plain\.reads .*"channels":"#alerts, #ops"/);
  assert.match(by.reads, /plain\.history .*count\W+30/);
  assert.equal(by.events, "mobile:bots.app.plain.eventsCreated");
  assert.match(by.wakes, /wakesOn .*triggers\.messages, mobile:bots\.app\.triggers\.mentions/);
  assert.match(by.can, /plain\.can .*actions\.post, mobile:bots\.app\.actions\.reply/);
  assert.match(by.egress, /plain\.sendsTo .*"hosts":"api\.linear\.app"/);
  assert.match(by.state, /plain\.keeps .*"size":"4 KB"/);
});

test("an approved App is described as manifest ∩ grants, never the raw request", () => {
  const narrowed = grants({ channels: [{ id: "c1", name: "alerts" }], history: 7, egress: [], actions: ["reply"] });
  const by = Object.fromEntries(plainLines(t, manifest(), narrowed).map((l) => [l.id, l.text]));
  assert.match(by.reads, /"channels":"#alerts"/);
  assert.doesNotMatch(by.reads, /#ops/);
  assert.match(by.reads, /count\W+7/);
  assert.match(by.can, /actions\.reply/);
  assert.doesNotMatch(by.can, /actions\.post/);
  // No host left: the "can't send data outside this group" mark is the line.
  assert.equal(by.egress, "mobile:bots.app.noEgressMark");
  // A grant the manifest does not request (a stale channel name) never shows.
  const stale = grants({ channels: [{ id: "c3", name: "random" }] });
  assert.equal(plainLines(t, manifest(), stale)[0].text, "mobile:bots.app.plain.readsNothing");
});

test("history, events and quota edge cases read plainly", () => {
  const by = Object.fromEntries(
    plainLines(
      t,
      manifest({ read: { channels: ["*"], history: 0, events: ["created", "edited"] }, state: { quota: 0 }, triggers: [] }),
      grants({ history: 0, events: ["created", "edited"] }),
    ).map((l) => [l.id, l.text]),
  );
  assert.match(by.reads, /historyNone/);
  assert.equal(by.events, "mobile:bots.app.plain.eventsBoth");
  assert.equal(by.wakes, "mobile:bots.app.plain.wakesNever");
  assert.equal(by.state, "mobile:bots.app.plain.keepsNothing");
  assert.equal(formatQuota(512), "512 B");
  assert.equal(formatQuota(1536), "1.5 KB");
  assert.equal(formatQuota(2 * 1024 * 1024), "2 MB");
});

test("effective grants mirror the core: a wildcard takes whatever the admin granted", () => {
  const eff = effectiveGrants(manifest({ read: { channels: ["*"], history: 30, events: ["created"] } }), grants());
  assert.equal(eff.channels.length, 2);
  assert.equal(effectiveGrants(manifest(), grants({ history: 90 })).history, 30);
});

// --- Diff and widenings --------------------------------------------------------------------------

const CHANGES: ManifestChange[] = [
  { field: "action", kind: "removed", value: "post", widening: false },
  { field: "channel", kind: "added", value: "#random", widening: true },
  { field: "egress", kind: "added", value: "evil.example", widening: true },
  { field: "history", kind: "increased", value: "60", previous: "30", widening: true },
  { field: "trigger", kind: "added", value: "webhook", widening: true },
];

test("widenings are listed first, in the core's order", () => {
  const sorted = sortChanges(CHANGES);
  assert.deepEqual(
    sorted.map((c) => c.field),
    ["channel", "egress", "history", "trigger", "action"],
  );
});

test("nothing widened is granted unless its own switch is on", () => {
  const next = manifest({
    read: { channels: ["#alerts", "#ops", "#random"], history: 60, events: ["created"] },
    actions: ["reply"],
    egress: ["api.linear.app", "evil.example"],
    triggers: ["messages", "mentions", "webhook"],
  });
  const suggested = grants({ actions: ["reply"] });
  const channels = CHANNELS;

  const none = widenGrants(suggested, CHANGES, new Set(), channels, next);
  assert.deepEqual(none.channels.map((c) => c.id), ["c1", "c2"]);
  assert.deepEqual(none.egress, ["api.linear.app"]);
  assert.equal(none.history, 30);

  const some = widenGrants(
    suggested,
    CHANGES,
    new Set([changeKey(CHANGES[1]), changeKey(CHANGES[3])]),
    channels,
    next,
  );
  assert.deepEqual(some.channels.map((c) => c.id), ["c1", "c2", "c3"]);
  // The egress widening stayed off.
  assert.deepEqual(some.egress, ["api.linear.app"]);
  assert.equal(some.history, 60);
});

test("a widening only approval itself grants (a trigger) blocks Approve until switched on", () => {
  assert.equal(approvalBlocked(CHANGES, new Set()), true);
  assert.equal(approvalBlocked(CHANGES, new Set([changeKey(CHANGES[4])])), false);
  // Grant-scoped widenings never block: left off they are simply not granted.
  assert.equal(approvalBlocked(CHANGES.slice(0, 4), new Set()), false);
});

test("slash commands and schedules read plainly and, widened, must be switched on to approve", () => {
  const cmd: ManifestChange = { field: "command", kind: "added", value: "close", widening: true };
  const toPosted: ManifestChange = { field: "command", kind: "increased", value: "ticket", previous: "request", widening: true };
  const toRequest: ManifestChange = { field: "command", kind: "increased", value: "ticket", previous: "command", widening: true };
  const faster: ManifestChange = {
    field: "schedule",
    kind: "increased",
    value: "digest every 600s",
    previous: "every 3600s",
    widening: true,
  };
  const slower: ManifestChange = { ...faster, kind: "decreased", value: "digest every 7200s", widening: false };
  assert.match(changeText(t, cmd), /commandAdded .*"value":"\/close"/);
  assert.match(changeText(t, toPosted), /commandToPosted/);
  assert.match(changeText(t, toRequest), /commandToRequest/);
  assert.match(changeText(t, faster), /scheduleChanged .*"value":"digest","every":"10 min","was":"1 h"/);
  assert.match(changeText(t, slower), /"every":"2 h"/);
  assert.equal(formatInterval(90), "90 s");
  assert.equal(formatInterval(86400), "1 d");
  for (const c of [cmd, toPosted, faster]) {
    assert.equal(approvalBlocked([c], new Set()), true, c.field);
    assert.equal(approvalBlocked([c], new Set([changeKey(c)])), false, c.field);
  }
  assert.equal(approvalBlocked([slower], new Set()), false);
});

// --- The approval input --------------------------------------------------------------------------

test("an update offers only carried-over access plus accepted widenings; controls only narrow", () => {
  const old = manifest();
  const next = manifest({ read: { channels: ["#alerts", "#ops"], history: 60, events: ["created"] } });
  const v = view({
    approved: approved(old, grants({ history: 14 })),
    pending: {
      appHash: "b".repeat(64),
      manifestHash: "m2",
      manifest: next,
      source: "https://example.com/app.wasm",
      developerKey: null,
      changes: [{ field: "history", kind: "increased", value: "60", previous: "30", widening: true }],
      widens: true,
      suggestedGrants: grants({ history: 14 }),
      autoApprovable: false,
    },
  });
  assert.equal(pendingKind(v), "update");
  const opts = grantOptions(v, "pending", new Set());
  assert.ok(opts);
  // Not accepted: the stepper cannot reach past the carried-over 14 days.
  assert.equal(opts.maxHistory, 14);
  const typedTooMuch = buildGrants(opts, { ...EMPTY_DRAFT, history: 60 });
  assert.equal(typedTooMuch.history, 14);
  // Deselect a channel and drop the host.
  const narrowed = buildGrants(opts, { channels: { c2: false }, egress: { "api.linear.app": false }, history: 7 });
  assert.deepEqual(narrowed.channels, [{ id: "c1", name: "alerts" }]);
  assert.deepEqual(narrowed.egress, []);
  assert.equal(narrowed.history, 7);
  // A switch for something not offered changes nothing.
  const sneaky = buildGrants(opts, { channels: { c3: true }, egress: { "evil.example": true }, history: null });
  assert.deepEqual(sneaky.channels.map((c) => c.id), ["c1", "c2"]);
  assert.deepEqual(sneaky.egress, ["api.linear.app"]);

  const input = buildApproveInput(v, "pending", { development: false, grants: narrowed, autoUpdate: true });
  assert.deepEqual(input, {
    groupId: "g1",
    botUserId: "bot1",
    grants: { channels: [{ id: "c1", name: "alerts" }], history: 7, events: ["created"], actions: ["post", "reply"], egress: [] },
    development: false,
    appHash: "b".repeat(64),
    developerKey: null,
    manifestHash: "m2",
    autoUpdate: true,
  });
});

test("a first approval offers everything the manifest asks for", () => {
  const m = manifest();
  const v = view({
    pending: {
      appHash: "c".repeat(64),
      manifestHash: "m1",
      manifest: m,
      source: "./app.wasm",
      developerKey: "devkeybase64AAAAAAAAAAAAAAAA",
      changes: [],
      widens: false,
      suggestedGrants: grants(),
      autoApprovable: false,
    },
  });
  assert.equal(pendingKind(v), "first");
  const opts = grantOptions(v, "pending", new Set());
  assert.ok(opts);
  assert.equal(opts.maxHistory, 30);
  assert.deepEqual(opts.egress, ["api.linear.app"]);
  const g = buildGrants(opts, EMPTY_DRAFT);
  assert.deepEqual(g, grants());
  // Development: the developer key, no App hash, never auto-update.
  const dev = buildApproveInput(v, "pending", { development: true, grants: g, autoUpdate: true });
  assert.equal(dev?.development, true);
  assert.equal(dev?.developerKey, "devkeybase64AAAAAAAAAAAAAAAA");
  assert.equal(dev?.appHash, null);
  assert.equal(dev?.autoUpdate, false);
  // Production of the same build: its App hash.
  const prod = buildApproveInput(v, "pending", { development: false, grants: g, autoUpdate: false });
  assert.equal(prod?.appHash, "c".repeat(64));
  assert.equal(prod?.developerKey, null);
  // Not a link unless https.
  assert.equal(isLinkSource("./app.wasm"), false);
  assert.equal(isLinkSource("https://example.com/app.wasm"), true);
  assert.equal(isLinkSource("http://example.com/app.wasm"), false);
});

test("narrowing in place re-signs the approved App hash and manifest; it can re-widen only up to the manifest", () => {
  const a = approved(manifest(), grants({ channels: [{ id: "c1", name: "alerts" }], history: 7 }));
  const v = view({ approved: a });
  const opts = grantOptions(v, "approved", new Set());
  assert.ok(opts);
  assert.deepEqual(opts.channels.map((c) => c.id), ["c1", "c2"]);
  assert.equal(opts.maxHistory, 30);
  const g = buildGrants(opts, { channels: { c2: true }, egress: {}, history: 99 });
  assert.equal(g.history, 30);
  const input = buildApproveInput(v, "approved", { development: false, grants: g, autoUpdate: true });
  assert.equal(input?.appHash, a.appHash);
  assert.equal(input?.manifestHash, "m1");
  assert.equal(input?.autoUpdate, true);
  const devApproved = view({ approved: approved(manifest(), grants(), { development: true, appHash: null, developerKey: "k" }) });
  const devInput = buildApproveInput(devApproved, "approved", { development: true, grants: grants(), autoUpdate: true });
  assert.equal(devInput?.developerKey, "k");
  assert.equal(devInput?.appHash, null);
  assert.equal(devInput?.autoUpdate, false);
});

// --- Notice and errors ---------------------------------------------------------------------------

test("the notice: waiting codes read as waiting, refusals name their reason, unknown codes keep the detail", () => {
  assert.equal(appNotice(view()), null);
  assert.equal(
    appNotice(view({ report: { state: "running", reason: "", detail: "", updatedAt: "" } })),
    null,
  );
  for (const code of ["not_approved", "hash_mismatch", "developer_key_mismatch", "build_widens"]) {
    const n = appNotice(view({ report: { state: "refused", reason: code, detail: "d", updatedAt: "" } }));
    assert.equal(n?.waiting, true, code);
  }
  const refused = appNotice(view({ report: { state: "refused", reason: "bad_build_signature", detail: "d", updatedAt: "" } }));
  assert.deepEqual(refused, { waiting: false, reasonKey: "mobile:bots.app.reasons.bad_build_signature", detail: "d" });
  const unknown = appNotice(view({ report: { state: "refused", reason: "brand_new", detail: "core words", updatedAt: "" } }));
  assert.deepEqual(unknown, { waiting: false, reasonKey: null, detail: "core words" });
  // A stored approval that does not verify here outranks a running report.
  const bad = appNotice(
    view({
      approvalRefusal: { code: "rolled_back", detail: "x" },
      report: { state: "running", reason: "", detail: "", updatedAt: "" },
    }),
  );
  assert.equal(bad?.reasonKey, "mobile:bots.app.reasons.rolled_back");
});

test("stale and changed approvals get their own copy and a refetch", () => {
  assert.equal(appApprovalErrorKey("app_approval_stale: another admin"), "mobile:bots.app.errors.stale");
  assert.equal(appApprovalErrorKey("BridgeError.Bridge: app_approval_changed: look again"), "mobile:bots.app.errors.changed");
  assert.equal(appApprovalErrorKey("network down"), "mobile:bots.app.errors.failed");
  assert.equal(isStaleApprovalError("app_approval_stale"), true);
  assert.equal(isStaleApprovalError(undefined), false);
});

// --- What the screens must keep ------------------------------------------------------------------

test("every reason code and every key the rules use has English copy", () => {
  const en = JSON.parse(readFileSync(join(ROOT, "../frontend/src/i18n/locales/en/mobile.json"), "utf8"));
  const app = en.bots.app;
  for (const code of [
    "malformed", "wrong_kind", "wrong_installation", "approver_not_in_group", "uncertified_approver",
    "bad_signature", "transparency_alarm", "not_admin", "manifest_mismatch", "widened_grants",
    "rolled_back", "bad_build_signature", "artifact_unavailable",
  ]) {
    assert.equal(typeof app.reasons[code], "string", code);
  }
  const src = read("lib/botApps.ts");
  for (const m of src.matchAll(/"mobile:bots\.app\.([\w.]+)"/g)) {
    const path = m[1].split(".");
    let node = app;
    for (const p of path) {
      node = node?.[p] ?? node?.[`${p}_other`];
    }
    assert.ok(node !== undefined, `mobile:bots.app.${m[1]}`);
  }
});

test("the approval UI draws no modal and the bot page and bots list run the auto-approval", () => {
  for (const f of readdirSync(join(ROOT, "components/bots"))) {
    const src = read(`components/bots/${f}`);
    assert.doesNotMatch(src, /\bModal\b|Alert\.alert/, f);
  }
  assert.match(read("app/group/bot.tsx"), /useAutoApproveBotApps\(/);
  assert.match(read("app/group/bot.tsx"), /<BotAppSection/);
  assert.match(read("app/group/bots.tsx"), /useAutoApproveBotApps\(/);
  // No timer: the auto-approval runs on open, never on an interval.
  assert.doesNotMatch(read("hooks/queries/useBotApps.ts"), /setInterval|refetchInterval/);
});
