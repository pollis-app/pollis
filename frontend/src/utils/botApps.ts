/*
 * Bot Apps (#2005): pure helpers for the approval page and the members' marks. What an App may
 * do in plain language, what an approval grants (never wider than the manifest, widened items
 * only when the admin turned each one on), and the input `approve_bot_app` signs.
 * Mirrors pollis-app-manifest (`requests_channel`, `effective_grants`, `carried_to`).
 */

import type {
  ActionKind,
  AdminGrants,
  AppManifest,
  ApproveBotAppInput,
  BotAppView,
  ChannelGrant,
  EventKind,
  ManifestChange,
  TriggerKind,
} from "../types/botApps";

/** "#Ops", "ops" → "ops"; "*" stays. */
function channelName(selector: string): string {
  if (selector === "*") {
    return "*";
  }
  return (selector.startsWith("#") ? selector.slice(1) : selector).toLowerCase();
}

/** Whether the manifest asks to read the channel called `name` (by name, or "*"). */
export function requestsChannel(manifest: AppManifest, name: string): boolean {
  const wanted = channelName(name);
  if (!wanted || wanted === "*") {
    return false;
  }
  return manifest.read.channels.some((s) => s === "*" || channelName(s) === wanted);
}

/** Byte-order compare, as the core's sorts (never locale-aware). */
function byCodeUnit(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

function sortedUnique<T extends string>(items: ReadonlyArray<T>): T[] {
  return [...new Set(items)].sort();
}

/** The canonical form approvals are signed over: channels by id, deduplicated, names bare. */
function normalizeGrants(g: AdminGrants): AdminGrants {
  const seen = new Set<string>();
  const channels: ChannelGrant[] = [];
  for (const c of [...g.channels].sort((a, b) => byCodeUnit(a.id, b.id) || byCodeUnit(a.name, b.name))) {
    if (seen.has(c.id)) {
      continue;
    }
    seen.add(c.id);
    channels.push({ id: c.id, name: channelName(c.name) });
  }
  return {
    channels,
    history: g.history,
    events: sortedUnique(g.events),
    actions: sortedUnique(g.actions),
    egress: sortedUnique(g.egress),
  };
}

/** What an approved App actually gets: the manifest ∩ the admin's grants. */
export function effectiveGrants(manifest: AppManifest, grants: AdminGrants): AdminGrants {
  return normalizeGrants({
    channels: grants.channels.filter((c) => requestsChannel(manifest, c.name)),
    history: Math.min(manifest.read.history, grants.history),
    events: grants.events.filter((e) => manifest.read.events.includes(e)),
    actions: grants.actions.filter((a) => manifest.actions.includes(a)),
    egress: grants.egress.filter((h) => manifest.egress.includes(h)),
  });
}

/** Everything the manifest asks for, among `channels`. */
export function fullGrants(manifest: AppManifest, channels: ReadonlyArray<ChannelGrant>): AdminGrants {
  return normalizeGrants({
    channels: channels.filter((c) => requestsChannel(manifest, c.name)),
    history: manifest.read.history,
    events: [...manifest.read.events],
    actions: [...manifest.actions],
    egress: [...manifest.egress],
  });
}

/** One line of "What it can do". The page words each kind; this decides which lines there are. */
export type AppLine =
  | { kind: "reads"; channels: string[]; history: number; edited: boolean }
  | { kind: "readsNothing" }
  | { kind: "wakes"; triggers: TriggerKind[] }
  | { kind: "can"; actions: ActionKind[] }
  | { kind: "sendsTo"; hosts: string[] }
  | { kind: "cannotSendOutside" }
  | { kind: "keeps"; bytes: number };

const TRIGGER_ORDER: ReadonlyArray<TriggerKind> = ["messages", "mentions", "commands", "schedule", "requests", "webhook"];
const ACTION_ORDER: ReadonlyArray<ActionKind> = ["post", "reply", "react", "edit_own", "delete_own", "transfer_history"];

/** The App's effective scope (manifest ∩ grants), one line per item. */
export function appLines(manifest: AppManifest, grants: AdminGrants): AppLine[] {
  const eff = effectiveGrants(manifest, grants);
  const lines: AppLine[] = [];
  if (eff.channels.length > 0) {
    lines.push({
      kind: "reads",
      channels: eff.channels.map((c) => c.name).sort(),
      history: eff.history,
      edited: eff.events.includes("edited" as EventKind),
    });
  } else {
    lines.push({ kind: "readsNothing" });
  }
  const triggers = TRIGGER_ORDER.filter((tr) => manifest.triggers.includes(tr));
  if (triggers.length > 0) {
    lines.push({ kind: "wakes", triggers });
  }
  const actions = ACTION_ORDER.filter((a) => eff.actions.includes(a));
  if (actions.length > 0) {
    lines.push({ kind: "can", actions });
  }
  lines.push(eff.egress.length > 0 ? { kind: "sendsTo", hosts: eff.egress } : { kind: "cannotSendOutside" });
  if (manifest.state.quota > 0) {
    lines.push({ kind: "keeps", bytes: manifest.state.quota });
  }
  return lines;
}

/** A stable key for one change row (and for the set of widenings the admin turned on). */
export function changeKey(c: ManifestChange): string {
  return `${c.field}:${c.kind}:${c.value}`;
}

/** Widenings first (the admin must see them), each group in the order the core gave. */
export function orderChanges(changes: ReadonlyArray<ManifestChange>): ManifestChange[] {
  return [...changes.filter((c) => c.widening), ...changes.filter((c) => !c.widening)];
}

/** Fields no grant can narrow: the runtime takes every declared trigger, quota, command and schedule. */
const UNGRANTABLE_FIELDS: ReadonlyArray<ManifestChange["field"]> = ["trigger", "state_quota", "command", "schedule"];

/**
 * Widenings no grant can hold back (a new trigger, a larger storage quota, a new or switched
 * command, a schedule that fires more often): the App gets them with the build, so the update
 * can only be approved once the admin has turned each one on.
 */
export function requiredWidenings(changes: ReadonlyArray<ManifestChange>): ManifestChange[] {
  return changes.filter((c) => c.widening && UNGRANTABLE_FIELDS.includes(c.field));
}

/**
 * A changed schedule's parts: the core words it "digest every 600s" (was "every 3600s"), so the
 * page can say it in the app language. Null when the value isn't in that shape.
 */
export function scheduleChangeParts(c: ManifestChange): { name: string; every: number; previousEvery: number | null } | null {
  const m = /^(.*) every (\d+)s$/.exec(c.value);
  if (!m) {
    return null;
  }
  const prev = /every (\d+)s$/.exec(c.previous ?? "");
  return { name: m[1], every: Number(m[2]), previousEvery: prev ? Number(prev[1]) : null };
}

/**
 * The most an approval of the waiting build may grant: the suggested grants plus each widening
 * the admin turned on, never more than the manifest asks for. Nothing widened is in it unless
 * its key is in `accepted`.
 */
export function grantCeiling(
  suggested: AdminGrants,
  changes: ReadonlyArray<ManifestChange>,
  accepted: ReadonlySet<string>,
  manifest: AppManifest,
  channels: ReadonlyArray<ChannelGrant>,
): AdminGrants {
  const out: AdminGrants = {
    channels: [...suggested.channels],
    history: suggested.history,
    events: [...suggested.events],
    actions: [...suggested.actions],
    egress: [...suggested.egress],
  };
  for (const c of changes) {
    if (!c.widening || !accepted.has(changeKey(c))) {
      continue;
    }
    switch (c.field) {
      case "channel": {
        const name = channelName(c.value);
        out.channels.push(...channels.filter((ch) => name === "*" || channelName(ch.name) === name));
        break;
      }
      case "history":
        out.history = Math.max(out.history, Number(c.value) || 0);
        break;
      case "event":
        out.events.push(c.value as EventKind);
        break;
      case "action":
        out.actions.push(c.value as ActionKind);
        break;
      case "egress":
        out.egress.push(c.value);
        break;
      default:
        break;
    }
  }
  // Never wider than the manifest, whatever the inputs said.
  return effectiveGrants(manifest, out);
}

/** What the admin took off the ceiling with the grant controls. */
export interface GrantNarrowing {
  droppedChannelIds: string[];
  droppedEgress: string[];
  /** Days; null keeps the ceiling's. */
  history: number | null;
}

export const NO_NARROWING: GrantNarrowing = { droppedChannelIds: [], droppedEgress: [], history: null };

/** The ceiling with the admin's narrowing applied: what the approval grants. */
export function applyNarrowing(ceiling: AdminGrants, n: GrantNarrowing): AdminGrants {
  const history = n.history === null ? ceiling.history : Math.max(0, Math.min(ceiling.history, Math.floor(n.history)));
  return normalizeGrants({
    channels: ceiling.channels.filter((c) => !n.droppedChannelIds.includes(c.id)),
    history,
    events: ceiling.events,
    actions: ceiling.actions,
    egress: ceiling.egress.filter((h) => !n.droppedEgress.includes(h)),
  });
}

/** The narrowing that turns `ceiling` into `grants` (an approved App's current choice). */
export function narrowingFrom(ceiling: AdminGrants, grants: AdminGrants): GrantNarrowing {
  const granted = new Set(grants.channels.map((c) => c.id));
  return {
    droppedChannelIds: ceiling.channels.filter((c) => !granted.has(c.id)).map((c) => c.id),
    droppedEgress: ceiling.egress.filter((h) => !grants.egress.includes(h)),
    history: Math.min(grants.history, ceiling.history),
  };
}

/** Whether two grant sets grant the same. */
export function sameGrants(a: AdminGrants, b: AdminGrants): boolean {
  return JSON.stringify(normalizeGrants(a)) === JSON.stringify(normalizeGrants(b));
}

/** The input `approve_bot_app` signs: the waiting build, or the approved one narrowed in place. */
export function approveInput(args: {
  view: BotAppView;
  of: "pending" | "approved";
  development: boolean;
  autoUpdate: boolean;
  grants: AdminGrants;
}): ApproveBotAppInput {
  const { view, of, grants } = args;
  const base = { groupId: view.groupId, botUserId: view.botUserId, grants: normalizeGrants(grants) };
  if (of === "approved") {
    const a = view.approved;
    if (!a) {
      throw new Error("no approved App to narrow");
    }
    return {
      ...base,
      development: a.development,
      appHash: a.development ? null : a.appHash,
      developerKey: a.development ? a.developerKey : null,
      manifestHash: a.manifestHash,
      autoUpdate: a.development ? false : args.autoUpdate,
    };
  }
  const p = view.pending;
  if (!p) {
    throw new Error("no App is waiting for approval");
  }
  const developerKey = developerKeyFor(view);
  const development = args.development && developerKey !== null;
  return {
    ...base,
    development,
    appHash: development ? null : p.appHash,
    developerKey: development ? developerKey : null,
    manifestHash: p.manifestHash,
    autoUpdate: development ? false : args.autoUpdate,
  };
}

/** The developer key a development installation would name, or null when none is offered. */
export function developerKeyFor(view: BotAppView): string | null {
  return view.pending?.developerKey ?? view.approved?.developerKey ?? null;
}

/** A developer key, short enough to read and compare. */
export function keyFingerprint(key: string): string {
  return key.slice(0, 16);
}

/** Refusals the approval page explains in its own words; the core's errors start with these. */
export type AppApprovalErrorKind = "stale" | "changed";

export function appApprovalErrorKind(err: unknown): AppApprovalErrorKind | null {
  const message = err instanceof Error ? err.message : typeof err === "string" ? err : "";
  if (message.startsWith("app_approval_stale")) {
    return "stale";
  }
  if (message.startsWith("app_approval_changed")) {
    return "changed";
  }
  return null;
}

/** Why an App is not running, as the runtime and the approval check name it. */
const APP_REASON_CODES = [
  "not_approved",
  "hash_mismatch",
  "developer_key_mismatch",
  "build_widens",
  "malformed",
  "wrong_kind",
  "wrong_installation",
  "approver_not_in_group",
  "uncertified_approver",
  "bad_signature",
  "transparency_alarm",
  "not_admin",
  "manifest_mismatch",
  "widened_grants",
  "rolled_back",
  "bad_build_signature",
  "artifact_unavailable",
] as const;
export type AppReasonCode = (typeof APP_REASON_CODES)[number];

export function isAppReasonCode(code: string): code is AppReasonCode {
  return (APP_REASON_CODES as ReadonlyArray<string>).includes(code);
}

/** The admin notice: why the bot is not running, or null while it runs. */
export function appNotice(view: BotAppView): { code: string; detail: string; waiting: boolean } | null {
  if (view.approvalRefusal) {
    return { code: view.approvalRefusal.code, detail: view.approvalRefusal.detail, waiting: false };
  }
  if (view.report && view.report.state !== "running") {
    return {
      code: view.report.reason,
      detail: view.report.detail,
      waiting: view.report.state === "awaiting_approval",
    };
  }
  return null;
}

/** Whether `source` may be opened as a link (only https). */
export function isLinkableSource(source: string | null): source is string {
  return !!source && source.startsWith("https://");
}
