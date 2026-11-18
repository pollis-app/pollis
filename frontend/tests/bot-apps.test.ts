/* Bot Apps (#2005): what the approval page says an App can do, what an approval grants (never
   wider than the manifest, never a widening the admin didn't turn on), and the input it signs. */

import test from "node:test";
import assert from "node:assert/strict";

import {
  NO_NARROWING,
  appApprovalErrorKind,
  appLines,
  appNotice,
  applyNarrowing,
  approveInput,
  changeKey,
  effectiveGrants,
  fullGrants,
  grantCeiling,
  narrowingFrom,
  orderChanges,
  requestsChannel,
  requiredWidenings,
  scheduleChangeParts,
} from "../src/utils/botApps.ts";
import type { AdminGrants, AppManifest, BotAppView, ManifestChange } from "../src/types/botApps.ts";

const CHANNELS = [
  { id: "c_alerts", name: "alerts" },
  { id: "c_ops", name: "ops" },
];

const V1: AppManifest = {
  name: "triage",
  version: "1.0.0",
  description: "Files alerts as issues.",
  triggers: ["messages", "webhook"],
  read: { channels: ["#alerts"], history: 30, events: ["created"] },
  actions: ["reply", "post"],
  state: { quota: 4096 },
  egress: ["api.linear.app"],
};

const V2: AppManifest = {
  ...V1,
  version: "2.0.0",
  triggers: ["messages", "webhook", "schedule"],
  read: { channels: ["#alerts", "#ops"], history: 90, events: ["created", "edited"] },
  egress: ["api.linear.app", "hooks.slack.com"],
};

const V1_GRANTS: AdminGrants = fullGrants(V1, CHANNELS);

const V2_CHANGES: ManifestChange[] = [
  { field: "trigger", kind: "added", value: "schedule", widening: true },
  { field: "channel", kind: "added", value: "#ops", widening: true },
  { field: "history", kind: "increased", value: "90", previous: "30", widening: true },
  { field: "event", kind: "added", value: "edited", widening: true },
  { field: "egress", kind: "added", value: "hooks.slack.com", widening: true },
  { field: "action", kind: "removed", value: "react", widening: false },
];

function view(over: Partial<BotAppView> = {}): BotAppView {
  return {
    botUserId: "u_bot",
    groupId: "g_work",
    approved: null,
    approvalRefusal: null,
    report: null,
    pending: null,
    channels: CHANNELS,
    latestSeq: 0,
    ...over,
  };
}

test("a manifest asks for a channel by name, case and '#' aside, or for any with '*'", () => {
  assert.equal(requestsChannel(V1, "alerts"), true);
  assert.equal(requestsChannel(V1, "#Alerts"), true);
  assert.equal(requestsChannel(V1, "ops"), false);
  assert.equal(requestsChannel({ ...V1, read: { ...V1.read, channels: ["*"] } }, "ops"), true);
});

test("everything requested: only the group's channels the manifest names", () => {
  assert.deepEqual(V1_GRANTS.channels, [{ id: "c_alerts", name: "alerts" }]);
  assert.equal(V1_GRANTS.history, 30);
  assert.deepEqual(V1_GRANTS.egress, ["api.linear.app"]);
});

test("the plain-language lines show the effective scope: manifest ∩ grants", () => {
  const narrowed: AdminGrants = { ...V1_GRANTS, history: 7, egress: [], actions: ["reply", "react" as never] };
  const lines = appLines(V1, narrowed);
  assert.deepEqual(lines, [
    { kind: "reads", channels: ["alerts"], history: 7, edited: false },
    { kind: "wakes", triggers: ["messages", "webhook"] },
    // "react" was never asked for, so it is not shown even though the grants name it.
    { kind: "can", actions: ["reply"] },
    { kind: "cannotSendOutside" },
    { kind: "keeps", bytes: 4096 },
  ]);
});

test("an App granted no channel reads none; one with egress names its hosts", () => {
  const lines = appLines(V2, { ...fullGrants(V2, CHANNELS), channels: [] });
  assert.deepEqual(lines[0], { kind: "readsNothing" });
  assert.ok(lines.some((l) => l.kind === "sendsTo" && l.hosts.join() === "api.linear.app,hooks.slack.com"));
  assert.ok(appLines(V2, fullGrants(V2, CHANNELS)).some((l) => l.kind === "reads" && l.edited));
});

test("effective grants never exceed the manifest, whatever the grants say", () => {
  const wide: AdminGrants = { ...fullGrants(V2, CHANNELS), history: 365 };
  const eff = effectiveGrants(V1, wide);
  assert.equal(eff.history, 30);
  assert.deepEqual(eff.channels.map((c) => c.id), ["c_alerts"]);
  assert.deepEqual(eff.egress, ["api.linear.app"]);
  assert.deepEqual(eff.events, ["created"]);
});

test("widenings are listed first and none is pre-accepted", () => {
  const ordered = orderChanges([V2_CHANGES[5], ...V2_CHANGES.slice(0, 5)]);
  assert.deepEqual(ordered.map((c) => c.widening), [true, true, true, true, true, false]);

  // Nothing turned on: the update grants exactly the carried-over grants.
  const ceiling = grantCeiling(V1_GRANTS, V2_CHANGES, new Set(), V2, CHANNELS);
  assert.deepEqual(ceiling, V1_GRANTS);
});

test("each widening the admin turns on is granted, and only that one", () => {
  const onlyOps = grantCeiling(V1_GRANTS, V2_CHANGES, new Set([changeKey(V2_CHANGES[1])]), V2, CHANNELS);
  assert.deepEqual(onlyOps.channels.map((c) => c.id), ["c_alerts", "c_ops"]);
  assert.equal(onlyOps.history, 30);
  assert.deepEqual(onlyOps.egress, ["api.linear.app"]);

  const all = grantCeiling(V1_GRANTS, V2_CHANGES, new Set(V2_CHANGES.map(changeKey)), V2, CHANNELS);
  assert.equal(all.history, 90);
  assert.deepEqual(all.events, ["created", "edited"]);
  assert.deepEqual(all.egress, ["api.linear.app", "hooks.slack.com"]);
});

test("an accepted '*' selector offers every channel the group has", () => {
  const any: AppManifest = { ...V1, read: { ...V1.read, channels: ["*"] } };
  const star: ManifestChange = { field: "channel", kind: "added", value: "*", widening: true };
  const ceiling = grantCeiling({ ...V1_GRANTS, channels: [] }, [star], new Set([changeKey(star)]), any, CHANNELS);
  assert.deepEqual(ceiling.channels.map((c) => c.id), ["c_alerts", "c_ops"]);
});

test("a new trigger or a larger quota can't be withheld by grants, so it must be allowed to approve", () => {
  const quota: ManifestChange = { field: "state_quota", kind: "increased", value: "8192", previous: "4096", widening: true };
  const shrink: ManifestChange = { field: "state_quota", kind: "decreased", value: "1024", previous: "4096", widening: false };
  assert.deepEqual(requiredWidenings([...V2_CHANGES, quota, shrink]).map(changeKey), [
    "trigger:added:schedule",
    "state_quota:increased:8192",
  ]);
});

test("narrowing only takes away: channels, history (clamped) and hosts", () => {
  const ceiling = fullGrants(V2, CHANNELS);
  const g = applyNarrowing(ceiling, { droppedChannelIds: ["c_ops"], droppedEgress: ["hooks.slack.com"], history: 400 });
  assert.deepEqual(g.channels.map((c) => c.id), ["c_alerts"]);
  assert.equal(g.history, 90);
  assert.deepEqual(g.egress, ["api.linear.app"]);
  assert.equal(applyNarrowing(ceiling, { ...NO_NARROWING, history: -3 }).history, 0);
  assert.deepEqual(applyNarrowing(ceiling, NO_NARROWING), ceiling);
});

test("an approved App's controls start from its current grants", () => {
  const ceiling = fullGrants(V2, CHANNELS);
  const current: AdminGrants = { ...ceiling, channels: [CHANNELS[0]], history: 14, egress: [] };
  const n = narrowingFrom(ceiling, current);
  assert.deepEqual(applyNarrowing(ceiling, n), current);
});

test("approving the waiting build pins its hash; development names the developer key instead", () => {
  const pending = {
    appHash: "a".repeat(64),
    manifestHash: "m2",
    manifest: V2,
    source: "https://example.com/triage.wasm",
    developerKey: "DEVKEYbase64AAAAAAAAAAAA",
    changes: [],
    widens: false,
    suggestedGrants: fullGrants(V2, CHANNELS),
    autoApprovable: false,
  };
  const grants = fullGrants(V2, CHANNELS);
  const prod = approveInput({ view: view({ pending }), of: "pending", development: false, autoUpdate: true, grants });
  assert.equal(prod.appHash, pending.appHash);
  assert.equal(prod.developerKey, null);
  assert.equal(prod.development, false);
  assert.equal(prod.autoUpdate, true);
  assert.equal(prod.manifestHash, "m2");

  const dev = approveInput({ view: view({ pending }), of: "pending", development: true, autoUpdate: true, grants });
  assert.equal(dev.development, true);
  assert.equal(dev.developerKey, pending.developerKey);
  assert.equal(dev.appHash, null);
  // Development installations don't update themselves.
  assert.equal(dev.autoUpdate, false);

  // No developer key offered: development can't be chosen.
  const noKey = approveInput({
    view: view({ pending: { ...pending, developerKey: null } }),
    of: "pending",
    development: true,
    autoUpdate: false,
    grants,
  });
  assert.equal(noKey.development, false);
  assert.equal(noKey.appHash, pending.appHash);
});

test("narrowing in place re-approves the approved build and manifest", () => {
  const approved = {
    seq: 3,
    development: false,
    appHash: "b".repeat(64),
    developerKey: null,
    manifestHash: "m1",
    manifest: V1,
    grants: V1_GRANTS,
    source: "./triage.wasm",
    autoUpdate: false,
    approverUserId: "u_me",
    issuedAt: 1_700_000_000,
    cannotSendOutside: false,
  };
  const input = approveInput({
    view: view({ approved, latestSeq: 3 }),
    of: "approved",
    development: false,
    autoUpdate: false,
    grants: { ...V1_GRANTS, egress: [] },
  });
  assert.equal(input.appHash, approved.appHash);
  assert.equal(input.manifestHash, "m1");
  assert.deepEqual(input.grants.egress, []);
});

test("the notice: a refused approval, a waiting build, nothing while running", () => {
  assert.equal(appNotice(view()), null);
  assert.equal(
    appNotice(view({ report: { state: "running", reason: "", detail: "", updatedAt: "" } })),
    null,
  );
  assert.deepEqual(
    appNotice(view({ report: { state: "awaiting_approval", reason: "hash_mismatch", detail: "d", updatedAt: "" } })),
    { code: "hash_mismatch", detail: "d", waiting: true },
  );
  assert.deepEqual(
    appNotice(
      view({
        approvalRefusal: { code: "bad_signature", detail: "x" },
        report: { state: "refused", reason: "rolled_back", detail: "y", updatedAt: "" },
      }),
    ),
    { code: "bad_signature", detail: "x", waiting: false },
  );
});

test("stale and changed refusals are told apart from other failures", () => {
  assert.equal(appApprovalErrorKind("app_approval_stale: another admin approved"), "stale");
  assert.equal(appApprovalErrorKind(new Error("app_approval_changed: look again")), "changed");
  assert.equal(appApprovalErrorKind("network down"), null);
});

test("new or switched commands and more frequent schedules can't be held back, so they must be allowed", () => {
  const changes: ManifestChange[] = [
    { field: "command", kind: "added", value: "close", widening: true },
    { field: "command", kind: "increased", value: "ticket", previous: "request", widening: true },
    { field: "command", kind: "removed", value: "old", widening: false },
    { field: "schedule", kind: "added", value: "nightly", widening: true },
    { field: "schedule", kind: "increased", value: "digest every 600s", previous: "every 3600s", widening: true },
    { field: "schedule", kind: "decreased", value: "sweep every 7200s", previous: "every 3600s", widening: false },
  ];
  assert.deepEqual(requiredWidenings(changes).map(changeKey), [
    "command:added:close",
    "command:increased:ticket",
    "schedule:added:nightly",
    "schedule:increased:digest every 600s",
  ]);
  // Nothing grantable among them: the ceiling stays the carried-over grants.
  assert.deepEqual(grantCeiling(V1_GRANTS, changes, new Set(changes.map(changeKey)), V1, CHANNELS), V1_GRANTS);
});

test("a changed schedule is read back into its name and periods", () => {
  assert.deepEqual(
    scheduleChangeParts({ field: "schedule", kind: "increased", value: "daily digest every 600s", previous: "every 3600s", widening: true }),
    { name: "daily digest", every: 600, previousEvery: 3600 },
  );
  assert.equal(scheduleChangeParts({ field: "schedule", kind: "added", value: "nightly", widening: true }), null);
});
