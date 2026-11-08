// #1735: copy message history between own devices — the state machine, the packet, the prompt gating, the runner.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { decodeRealtimeEvent, isTrustedOrigin } from "../lib/realtime/events.ts";
import {
  canCancel,
  currentTransfer,
  isTerminal,
  mergeTransfer,
  needsRun,
  preferredSource,
  progressPercent,
  retryKind,
  sortSources,
  transferPhase,
  type HistoryTransfer,
  type PendingTransferRequest,
  type TransferSource,
  type TransferState,
} from "../lib/historyTransfer.ts";
import {
  canPresentTransferApproval,
  claimInline,
  nextTransferApproval,
  noticeHistoryTransfer,
  nudgeWantsPendingCheck,
  nudgeWantsRun,
  onHistoryTransferNudge,
  type HistoryTransferNudge,
} from "../lib/historyTransferPrompt.ts";
import { createTransferRunner } from "../lib/historyTransferRunner.ts";

const read = (p: string) => readFileSync(new URL(p, import.meta.url), "utf8");

function transfer(over: Partial<HistoryTransfer> = {}): HistoryTransfer {
  return {
    transfer_id: "T1",
    role: "receiver",
    peer_device_id: "D-src",
    peer_device_name: "MacBook",
    via: "sibling",
    state: "transferring",
    chunks_done: 0,
    rows_done: 0,
    rows_skipped: 0,
    deferred_conversations: 0,
    updated_at: "2026-11-08 10:00:00",
    ...over,
  };
}

function source(over: Partial<TransferSource> = {}): TransferSource {
  return { device_id: "D1", device_name: "Phone", online: false, ...over };
}

function pending(over: Partial<PendingTransferRequest> = {}): PendingTransferRequest {
  return {
    transfer_id: "T1",
    requester_device_id: "D-new",
    requester_device_name: "iPad",
    requester_added_at: "2026-11-08 09:00:00",
    via: "sibling",
    pin_required: true,
    created_at: "2026-11-08 10:00:00",
    ...over,
  };
}

// ── packet ──

test("the DS's history_transfer packet decodes, either id spelling, and only from the DS", () => {
  for (const wire of [
    { type: "history_transfer", transfer_id: "T9", event: "requested" },
    { type: "history_transfer", transferId: "T9", event: "requested" },
  ]) {
    const event = decodeRealtimeEvent(new TextEncoder().encode(JSON.stringify(wire)));
    assert.deepEqual(event, { type: "history_transfer", transfer_id: "T9", event: "requested" });
    assert.ok(isTrustedOrigin(event!, false), "DS-sent (no participant) is trusted");
    assert.ok(!isTrustedOrigin(event!, true), "a participant cannot spoof it");
  }
});

test("a malformed history_transfer packet is dropped, not passed on half-read", () => {
  const bad = [
    { type: "history_transfer", event: "requested" },
    { type: "history_transfer", transfer_id: "", event: "requested" },
    { type: "history_transfer", transfer_id: 7, event: "requested" },
    { type: "history_transfer", transfer_id: "T1", event: "exploded" },
    { type: "history_transfer", transfer_id: "T1" },
  ];
  for (const wire of bad) {
    assert.equal(decodeRealtimeEvent(JSON.stringify(wire)), null, JSON.stringify(wire));
  }
  for (const kind of ["requested", "accepted", "chunk", "acked", "done", "cancelled"]) {
    assert.ok(decodeRealtimeEvent(JSON.stringify({ type: "history_transfer", transfer_id: "T", event: kind })));
  }
});

test("the packet carries nothing but the id and the event (content-free hint)", () => {
  const event = decodeRealtimeEvent(
    JSON.stringify({ type: "history_transfer", transfer_id: "T", event: "chunk", rows: ["secret"] }),
  );
  assert.deepEqual(Object.keys(event!).sort(), ["event", "transfer_id", "type"]);
});

// ── state machine ──

test("terminal states never run again; everything else does and can be cancelled", () => {
  const terminal: TransferState[] = ["completed", "declined", "cancelled", "expired", "failed"];
  const live: TransferState[] = ["awaiting_approval", "transferring", "paused"];
  for (const state of terminal) {
    assert.ok(isTerminal(state));
    assert.ok(!needsRun(transfer({ state })));
    assert.ok(!canCancel(transfer({ state })));
  }
  for (const state of live) {
    assert.ok(!isTerminal(state));
    assert.ok(needsRun(transfer({ state })));
    assert.ok(canCancel(transfer({ state })));
  }
});

test("phases: waiting, progress, paused, done vs deferred, and the end states", () => {
  assert.equal(transferPhase(transfer({ state: "awaiting_approval" })), "waiting");
  assert.equal(transferPhase(transfer({ state: "transferring" })), "transferring");
  assert.equal(transferPhase(transfer({ state: "paused" })), "paused");
  assert.equal(transferPhase(transfer({ state: "completed" })), "done");
  assert.equal(transferPhase(transfer({ state: "completed", deferred_conversations: 2 })), "deferred");
  assert.equal(transferPhase(transfer({ state: "failed" })), "failed");
  assert.equal(transferPhase(transfer({ state: "declined" })), "declined");
});

test("retry: paused resumes on either side; only the receiver re-asks after an end", () => {
  assert.equal(retryKind(transfer({ state: "paused" })), "resume");
  assert.equal(retryKind(transfer({ state: "paused", role: "source" })), "resume");
  assert.equal(retryKind(transfer({ state: "failed" })), "request");
  assert.equal(retryKind(transfer({ state: "expired" })), "request");
  assert.equal(retryKind(transfer({ state: "completed", deferred_conversations: 1 })), "request");
  assert.equal(retryKind(transfer({ state: "failed", role: "source" })), null);
  assert.equal(retryKind(transfer({ state: "completed" })), null);
  // A decline is the other device's answer: asking again needs a deliberate new pick.
  assert.equal(retryKind(transfer({ state: "declined" })), null);
  assert.equal(retryKind(transfer({ state: "transferring" })), null);
});

test("progress is a percentage only when the source said how much is coming", () => {
  assert.equal(progressPercent(transfer({ rows_done: 10 })), null);
  assert.equal(progressPercent(transfer({ rows_done: 25, rows_skipped: 25, estimated_total: 200 })), 25);
  assert.equal(progressPercent(transfer({ rows_done: 500, estimated_total: 200 })), 100);
});

test("sources: supported first, then online, then most recently active", () => {
  const sorted = sortSources([
    source({ device_id: "old", last_active_at: "2026-11-01 10:00:00" }),
    source({ device_id: "unsupported", online: true, supports_transfer: false }),
    source({ device_id: "recent", last_active_at: "2026-11-08 09:00:00" }),
    source({ device_id: "online", online: true, last_active_at: "2026-10-01 10:00:00" }),
  ]);
  assert.deepEqual(
    sorted.map((s) => s.device_id),
    ["online", "recent", "old", "unsupported"],
  );
});

test("link time defaults to the approving device, never to one that cannot send", () => {
  const list = [source({ device_id: "A" }), source({ device_id: "B" })];
  assert.equal(preferredSource(list, "B")?.device_id, "B");
  assert.equal(preferredSource(list, null), null, "two candidates and no approver: the user picks");
  assert.equal(preferredSource([source({ device_id: "A" })], null)?.device_id, "A");
  assert.equal(
    preferredSource([source({ device_id: "A" }), source({ device_id: "B", approved_this_device: true })], null)
      ?.device_id,
    "B",
  );
  assert.equal(preferredSource([source({ device_id: "A", supports_transfer: false })], "A"), null);
});

test("a page follows the unfinished transfer for its role, else the newest", () => {
  const list = [
    transfer({ transfer_id: "old-done", state: "completed", updated_at: "2026-11-08 08:00:00" }),
    transfer({ transfer_id: "live", state: "paused", updated_at: "2026-11-07 08:00:00" }),
    transfer({ transfer_id: "src", role: "source", state: "transferring" }),
  ];
  assert.equal(currentTransfer(list, "receiver")?.transfer_id, "live");
  assert.equal(currentTransfer(list, "source")?.transfer_id, "src");
  assert.equal(currentTransfer(list.slice(0, 1), "receiver")?.transfer_id, "old-done");
  assert.equal(currentTransfer([], "receiver"), null);
});

test("a fresh row replaces its stale copy instead of duplicating it", () => {
  const merged = mergeTransfer([transfer({ rows_done: 1 }), transfer({ transfer_id: "T2" })], transfer({ rows_done: 9 }));
  assert.equal(merged.length, 2);
  assert.equal(merged.find((t) => t.transfer_id === "T1")?.rows_done, 9);
});

// ── prompt gating ──

test("the approval page opens only in the unlocked app and never over another approval", () => {
  assert.ok(canPresentTransferApproval(["(tabs)", "groups"], false));
  assert.ok(!canPresentTransferApproval(["(tabs)", "groups"], true));
  assert.ok(!canPresentTransferApproval(["(auth)", "pin"], false));
  assert.ok(!canPresentTransferApproval(["approve-history-transfer"], false));
  assert.ok(!canPresentTransferApproval(["approve-device"], false));
  assert.ok(canPresentTransferApproval(["self", "security"], false));
});

test("the newest unseen request opens once; one answered inline never opens as a page", () => {
  const a = pending({ transfer_id: "A", created_at: "2026-11-08 09:00:00", requester_device_id: "DA" });
  const b = pending({ transfer_id: "B", created_at: "2026-11-08 10:00:00", requester_device_id: "DB" });
  assert.equal(nextTransferApproval([a, b], new Set())?.transfer_id, "B");
  assert.equal(nextTransferApproval([a, b], new Set(["B"]))?.transfer_id, "A");
  assert.equal(nextTransferApproval([a, b], new Set(["A", "B"])), null);

  const release = claimInline("DB");
  assert.equal(nextTransferApproval([a, b], new Set())?.transfer_id, "A");
  const seen: HistoryTransferNudge[] = [];
  const off = onHistoryTransferNudge((n) => seen.push(n));
  release();
  off();
  assert.equal(nextTransferApproval([a, b], new Set())?.transfer_id, "B");
  assert.equal(seen.length, 1, "releasing a claim re-checks, so an unanswered request still opens");
  assert.ok(nudgeWantsPendingCheck(seen[0].event));
});

test("nudges: requested re-checks pending; accepted (re)runs; counters only refresh", () => {
  assert.ok(nudgeWantsPendingCheck("requested"));
  assert.ok(nudgeWantsPendingCheck("cancelled"), "a request cancelled before approval leaves the list");
  assert.ok(nudgeWantsRun("accepted"));
  assert.ok(!nudgeWantsRun("chunk"));
  assert.ok(!nudgeWantsRun("acked"));
  assert.ok(!nudgeWantsPendingCheck("chunk"));
});

test("every listener hears a nudge, and a removed one does not", () => {
  const heard: string[] = [];
  const offA = onHistoryTransferNudge((n) => heard.push(`a:${n.event}`));
  const offB = onHistoryTransferNudge((n) => heard.push(`b:${n.event}`));
  noticeHistoryTransfer({ transfer_id: "T", event: "accepted" });
  offB();
  noticeHistoryTransfer({ transfer_id: "T", event: "done" });
  offA();
  assert.deepEqual(heard, ["a:accepted", "b:accepted", "a:done"]);
});

// ── runner ──

function deferred<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

test("one run per transfer at a time; a kick during a run joins it", async () => {
  const calls: string[] = [];
  const gate = deferred<HistoryTransfer>();
  const runner = createTransferRunner({
    run: (id) => {
      calls.push(id);
      return gate.promise;
    },
  });
  const first = runner.kick("T1");
  const second = runner.kick("T1");
  assert.deepEqual(runner.running(), ["T1"]);
  gate.resolve(transfer({ state: "completed" }));
  await Promise.all([first, second]);
  assert.deepEqual(calls, ["T1"], "a finished transfer is not run again for the joined kick");
  assert.deepEqual(runner.running(), []);
});

test("a wake-up during a run that comes back paused runs once more, not once per wake-up", async () => {
  let n = 0;
  const gates = [deferred<HistoryTransfer>(), deferred<HistoryTransfer>()];
  const results: TransferState[] = [];
  const runner = createTransferRunner({
    run: () => gates[n++].promise,
    onResult: (t) => results.push(t.state),
  });
  const done = runner.kick("T1");
  void runner.kick("T1");
  void runner.kick("T1");
  gates[0].resolve(transfer({ state: "paused" }));
  await new Promise((r) => setTimeout(r, 0));
  gates[1].resolve(transfer({ state: "paused" }));
  await done;
  assert.equal(n, 2);
  assert.deepEqual(results, ["paused", "paused"]);
  assert.deepEqual(runner.running(), []);
});

test("paused with no wake-up waits for the next trigger (no polling loop)", async () => {
  let n = 0;
  const runner = createTransferRunner({
    run: async () => {
      n += 1;
      return transfer({ state: "paused" });
    },
  });
  await runner.kick("T1");
  assert.equal(n, 1);
});

test("a failing run is reported and frees the slot for the next trigger", async () => {
  const errors: string[] = [];
  let fail = true;
  const runner = createTransferRunner({
    run: async (id) => {
      if (fail) {
        throw new Error("network");
      }
      return transfer({ transfer_id: id, state: "completed" });
    },
    onError: (id) => errors.push(id),
  });
  await runner.kick("T1");
  assert.deepEqual(errors, ["T1"]);
  fail = false;
  await runner.kick("T1");
  assert.deepEqual(runner.running(), []);
});

test("kickAll runs only the unfinished transfers", async () => {
  const ran: string[] = [];
  const runner = createTransferRunner({
    run: async (id) => {
      ran.push(id);
      return transfer({ transfer_id: id, state: "completed" });
    },
  });
  await runner.kickAll([
    transfer({ transfer_id: "a", state: "transferring" }),
    transfer({ transfer_id: "b", state: "completed" }),
    transfer({ transfer_id: "c", state: "paused" }),
    transfer({ transfer_id: "d", state: "awaiting_approval" }),
    transfer({ transfer_id: "e", state: "cancelled" }),
  ]);
  assert.deepEqual(ran.sort(), ["a", "c", "d"]);
});

// ── wiring (source text: the screens can't load under node) ──

test("the inbox handler forwards the packet, and the service is mounted with the other signed-in services", () => {
  assert.match(read("../hooks/useInboxRealtime.ts"), /case "history_transfer":[\s\S]*?noticeHistoryTransfer\(/);
  assert.match(read("../app/_layout.tsx"), /useHistoryTransferService\(\);/);
  assert.match(read("../app/_layout.tsx"), /name="approve-history-transfer"/);
  assert.match(read("../app/_layout.tsx"), /name="self\/history-transfer"/);
});

test("the service resumes on landing and on every foreground return, and opens the page like approve-device", () => {
  const svc = read("../hooks/useHistoryTransferService.ts");
  assert.match(svc, /AppState\.addEventListener\("change"/);
  assert.match(svc, /resumeHistoryTransfers\(\)/);
  assert.match(svc, /pathname: "\/approve-history-transfer"/);
  assert.doesNotMatch(svc, /setInterval|refetchInterval/, "event-driven, never polled");
});

test("the approval page and the PIN keypad never avoid a keyboard (#1642)", () => {
  assert.match(read("../app/approve-history-transfer.tsx"), /<Screen\b[^>]*avoidKeyboard=\{false\}/);
});

test("mobile copy never says plaintext, E2EE or passcode", () => {
  const en = JSON.parse(read("../../frontend/src/i18n/locales/en/mobile.json")).historyTransfer;
  const text = JSON.stringify(en);
  assert.doesNotMatch(text, /plaintext|end-to-end|e2ee|encrypt|passcode/i);
  assert.match(text, /message history/);
});
