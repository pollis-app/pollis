// Bot Apps (#2005): the wire types of the four App bridge commands and the pure rules the bot page
// builds on (plain-language manifest, the grants an approval signs, the diff's order). Desktop's twin
// is `frontend/src/utils/botApps.ts`; the shapes mirror `pollis-core/src/commands/bots/apps.rs`.

type TriggerKind = "messages" | "mentions" | "commands" | "schedule" | "requests" | "webhook";
type EventKind = "created" | "edited";
type ActionKind = "post" | "reply" | "react" | "edit_own" | "delete_own" | "transfer_history";

/** `pollis-app.json`, snake_case as the file itself. */
export interface AppManifest {
  $schema?: string;
  name: string;
  version: string;
  description: string;
  triggers: TriggerKind[];
  read: { channels: string[]; history: number; events: EventKind[] };
  actions: ActionKind[];
  state: { quota: number };
  egress: string[];
}

export interface ChannelGrant {
  id: string;
  name: string;
}

/** The admin's narrowing of a manifest (snake_case); channel names lowercase, no `#`. */
export interface AdminGrants {
  channels: ChannelGrant[];
  history: number;
  events: EventKind[];
  actions: ActionKind[];
  egress: string[];
}

type ChangeField =
  | "trigger"
  | "channel"
  | "history"
  | "event"
  | "action"
  | "egress"
  | "state_quota"
  | "command"
  | "schedule";
type ChangeKind = "added" | "removed" | "increased" | "decreased";

export interface ManifestChange {
  field: ChangeField;
  kind: ChangeKind;
  value: string;
  previous?: string;
  widening: boolean;
}

type BotAppRunState = "running" | "awaiting_approval" | "refused";

export interface ApprovedAppView {
  seq: number;
  development: boolean;
  appHash: string | null;
  developerKey: string | null;
  manifestHash: string;
  manifest: AppManifest;
  grants: AdminGrants;
  source: string | null;
  autoUpdate: boolean;
  approverUserId: string;
  issuedAt: number;
  cannotSendOutside: boolean;
}

export interface PendingAppView {
  appHash: string;
  manifestHash: string;
  manifest: AppManifest;
  source: string | null;
  developerKey: string | null;
  changes: ManifestChange[];
  widens: boolean;
  suggestedGrants: AdminGrants;
  autoApprovable: boolean;
}

export interface BotAppView {
  botUserId: string;
  groupId: string;
  approved: ApprovedAppView | null;
  approvalRefusal: { code: string; detail: string } | null;
  report: { state: BotAppRunState; reason: string; detail: string; updatedAt: string } | null;
  pending: PendingAppView | null;
  channels: ChannelGrant[];
  latestSeq: number;
}

export interface ApproveBotAppInput {
  groupId: string;
  botUserId: string;
  development: boolean;
  appHash?: string | null;
  developerKey?: string | null;
  manifestHash: string;
  grants: AdminGrants;
  autoUpdate: boolean;
}

/** `list_group_apps`: what every member may know about a bot's approved App. */
export interface GroupAppView {
  botUserId: string;
  development: boolean;
  manifest: AppManifest;
  manifestJson: string;
  grants: AdminGrants;
  cannotSendOutside: boolean;
}

/** A translate function, as i18next's `t` is called here. */
export type Translate = (key: string, options?: Record<string, unknown>) => string;

// --- Reasons and errors --------------------------------------------------------------------------

/** The runtime is fine, it only waits for an admin. */
const WAITING_REASONS = new Set(["not_approved", "hash_mismatch", "developer_key_mismatch", "build_widens"]);

const REFUSAL_REASONS = new Set([
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
]);

/** The notice the bot page leads with, or null while the App runs and its approval verifies. */
export interface AppNotice {
  /** Only waiting for an admin to approve, not refused. */
  waiting: boolean;
  /** Catalogue key for the reason, or null for a code this build does not know. */
  reasonKey: string | null;
  /** The core's own words, for an unknown code. */
  detail: string;
}

export function appNotice(view: BotAppView | null | undefined): AppNotice | null {
  if (!view) {
    return null;
  }
  const from = (code: string, detail: string, awaiting: boolean): AppNotice => {
    const waiting = awaiting || WAITING_REASONS.has(code);
    return {
      waiting,
      reasonKey: waiting ? null : REFUSAL_REASONS.has(code) ? `mobile:bots.app.reasons.${code}` : null,
      detail,
    };
  };
  // The stored approval does not verify on this device: that outranks the runtime's report.
  if (view.approvalRefusal) {
    return from(view.approvalRefusal.code, view.approvalRefusal.detail, false);
  }
  if (view.report && view.report.state !== "running") {
    return from(view.report.reason, view.report.detail, view.report.state === "awaiting_approval");
  }
  return null;
}

/** "Reload and look again" refusals of an approval get their own copy; anything else is generic. */
export function appApprovalErrorKey(message: string | null | undefined): string {
  const bare = (message ?? "").replace(/^BridgeError\.\w+:\s*/, "").trim();
  if (bare.startsWith("app_approval_stale")) {
    return "mobile:bots.app.errors.stale";
  }
  if (bare.startsWith("app_approval_changed")) {
    return "mobile:bots.app.errors.changed";
  }
  return "mobile:bots.app.errors.failed";
}

/** Whether a failed approval means the page is out of date and should be refetched. */
export function isStaleApprovalError(message: string | null | undefined): boolean {
  return appApprovalErrorKey(message) !== "mobile:bots.app.errors.failed";
}

// --- Grants --------------------------------------------------------------------------------------

/** `"#Ops"` / `"ops"` to `"ops"`; null for `"*"` or an empty name. */
function channelName(raw: string): string | null {
  const name = (raw.startsWith("#") ? raw.slice(1) : raw).toLowerCase();
  if (!name || name.includes("*") || /\s/.test(name)) {
    return null;
  }
  return name;
}

/** Whether the manifest's `read.channels` takes this channel (by name, or `"*"`). */
function requestsChannel(manifest: AppManifest, name: string): boolean {
  const wanted = channelName(name);
  if (wanted === null) {
    return false;
  }
  return manifest.read.channels.some((sel) => sel === "*" || channelName(sel) === wanted);
}

/** The canonical form the core signs: channels sorted by id, deduplicated, names bare and lowercase. */
export function normalizeGrants(g: AdminGrants): AdminGrants {
  const channels = dedupeChannels([...g.channels].sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0)));
  return {
    channels,
    history: g.history,
    events: [...new Set(g.events)].sort(),
    actions: [...new Set(g.actions)].sort(),
    egress: [...new Set(g.egress)].sort(),
  };
}

/** What the runtime enforces: the manifest intersected with the grants (core `effective_grants`). */
export function effectiveGrants(manifest: AppManifest, grants: AdminGrants): AdminGrants {
  return {
    channels: grants.channels.filter((c) => requestsChannel(manifest, c.name)),
    history: Math.min(manifest.read.history, grants.history),
    events: manifest.read.events.filter((e) => grants.events.includes(e)),
    actions: manifest.actions.filter((a) => grants.actions.includes(a)),
    egress: manifest.egress.filter((h) => grants.egress.includes(h)),
  };
}

/** A stable id for one diff row (its switch's state and testID). */
export function changeKey(c: ManifestChange): string {
  return `${c.field}:${c.kind}:${c.value}`;
}

/** Widenings first, each group in the core's order: the rows that need a decision lead. */
export function sortChanges(changes: readonly ManifestChange[]): ManifestChange[] {
  return [...changes.filter((c) => c.widening), ...changes.filter((c) => !c.widening)];
}

/**
 * Widenings no grant can narrow (a new trigger, a larger data quota, a new or changed slash command,
 * a new or more frequent schedule): approving the build grants them whatever the grants say, so each
 * needs its own explicit opt-in before Approve is allowed.
 */
function needsExplicitAcceptance(c: ManifestChange): boolean {
  return (
    c.widening &&
    (c.field === "trigger" || c.field === "state_quota" || c.field === "command" || c.field === "schedule")
  );
}

/** True while a widening that only approval itself can grant is still switched off. */
export function approvalBlocked(changes: readonly ManifestChange[], accepted: ReadonlySet<string>): boolean {
  return changes.some((c) => needsExplicitAcceptance(c) && !accepted.has(changeKey(c)));
}

/**
 * The suggested grants plus exactly the widened items the admin switched on. Nothing widened is
 * ever added unless its key is in `accepted`; the result never exceeds the manifest.
 */
export function widenGrants(
  suggested: AdminGrants,
  changes: readonly ManifestChange[],
  accepted: ReadonlySet<string>,
  channels: readonly ChannelGrant[],
  manifest: AppManifest,
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
    if (c.field === "channel") {
      const wanted = c.value === "*" ? null : channelName(c.value);
      for (const ch of channels) {
        if ((wanted === null || channelName(ch.name) === wanted) && !out.channels.some((x) => x.id === ch.id)) {
          out.channels.push({ id: ch.id, name: ch.name });
        }
      }
    } else if (c.field === "history") {
      out.history = Math.max(out.history, Number(c.value) || 0);
    } else if (c.field === "event") {
      out.events.push(c.value as EventKind);
    } else if (c.field === "action") {
      out.actions.push(c.value as ActionKind);
    } else if (c.field === "egress") {
      out.egress.push(c.value);
    }
  }
  // Never wider than what the build asks for, whatever came in.
  const bounded = effectiveGrants(manifest, out);
  return normalizeGrants(bounded);
}

/** The admin's edits on the grant controls: a switch per channel id / host, and the history days. */
export interface GrantDraft {
  channels: Record<string, boolean>;
  egress: Record<string, boolean>;
  history: number | null;
}

export const EMPTY_DRAFT: GrantDraft = { channels: {}, egress: {}, history: null };

/** What the grant controls offer, and the grants they start from. */
export interface GrantOptions {
  manifest: AppManifest;
  base: AdminGrants;
  channels: ChannelGrant[];
  egress: string[];
  maxHistory: number;
}

export type ApprovalTarget = "approved" | "pending";

/**
 * The controls for narrowing the approved App in place, or for the build waiting for approval. A
 * first approval and an in-place edit offer everything the manifest asks for; an update offers only
 * what is carried over plus the widenings the admin switched on, so a control can never re-add
 * access that the update's diff is asking about.
 */
export function grantOptions(
  view: BotAppView,
  target: ApprovalTarget,
  accepted: ReadonlySet<string>,
): GrantOptions | null {
  if (target === "approved") {
    const a = view.approved;
    if (!a) {
      return null;
    }
    const channels = dedupeChannels([
      ...a.grants.channels,
      ...view.channels.filter((c) => requestsChannel(a.manifest, c.name)),
    ]);
    return {
      manifest: a.manifest,
      base: normalizeGrants(a.grants),
      channels,
      egress: [...a.manifest.egress],
      maxHistory: a.manifest.read.history,
    };
  }
  const p = view.pending;
  if (!p) {
    return null;
  }
  const base = widenGrants(p.suggestedGrants, p.changes, accepted, view.channels, p.manifest);
  if (!view.approved) {
    return {
      manifest: p.manifest,
      base,
      channels: dedupeChannels([...base.channels, ...view.channels]),
      egress: [...p.manifest.egress],
      maxHistory: p.manifest.read.history,
    };
  }
  return {
    manifest: p.manifest,
    base,
    channels: base.channels,
    egress: base.egress,
    maxHistory: base.history,
  };
}

function dedupeChannels(list: readonly ChannelGrant[]): ChannelGrant[] {
  const seen = new Set<string>();
  const out: ChannelGrant[] = [];
  for (const c of list) {
    if (!seen.has(c.id)) {
      seen.add(c.id);
      out.push({ id: c.id, name: (c.name.startsWith("#") ? c.name.slice(1) : c.name).toLowerCase() });
    }
  }
  return out;
}

/** Whether a channel / host is switched on: the admin's choice, else whether the base grants it. */
export function channelOn(opts: GrantOptions, draft: GrantDraft, id: string): boolean {
  return draft.channels[id] ?? opts.base.channels.some((c) => c.id === id);
}

export function egressOn(opts: GrantOptions, draft: GrantDraft, host: string): boolean {
  return draft.egress[host] ?? opts.base.egress.includes(host);
}

/** The history the controls show: the admin's number, clamped to 0..maxHistory. */
export function draftHistory(opts: GrantOptions, draft: GrantDraft): number {
  const raw = draft.history ?? Math.min(opts.base.history, opts.maxHistory);
  return Math.max(0, Math.min(opts.maxHistory, Math.floor(raw)));
}

/** The grants an approval signs: only what the controls offer and have switched on. */
export function buildGrants(opts: GrantOptions, draft: GrantDraft): AdminGrants {
  return normalizeGrants({
    channels: opts.channels.filter((c) => channelOn(opts, draft, c.id)),
    history: draftHistory(opts, draft),
    events: opts.base.events,
    actions: opts.base.actions,
    egress: opts.egress.filter((h) => egressOn(opts, draft, h)),
  });
}

/** Parse what the admin typed into the history field; null keeps the current value. */
export function parseHistoryInput(text: string): number | null {
  const digits = text.replace(/[^0-9]/g, "");
  if (!digits) {
    return 0;
  }
  const n = Number(digits);
  return Number.isFinite(n) ? n : null;
}

/**
 * The bridge input for an approval. `approved` re-signs the App that is approved now with new grants
 * (narrowing in place); `pending` approves the build the runtime reported, as production (its App
 * hash) or, with `development`, as a development installation naming its developer key.
 */
export function buildApproveInput(
  view: BotAppView,
  target: ApprovalTarget,
  choice: { development: boolean; grants: AdminGrants; autoUpdate: boolean },
): ApproveBotAppInput | null {
  const base = { groupId: view.groupId, botUserId: view.botUserId, grants: normalizeGrants(choice.grants) };
  if (target === "approved") {
    const a = view.approved;
    if (!a) {
      return null;
    }
    return {
      ...base,
      development: a.development,
      appHash: a.development ? null : a.appHash,
      developerKey: a.development ? a.developerKey : null,
      manifestHash: a.manifestHash,
      autoUpdate: a.development ? false : choice.autoUpdate,
    };
  }
  const p = view.pending;
  if (!p) {
    return null;
  }
  if (choice.development) {
    const key = p.developerKey ?? view.approved?.developerKey ?? null;
    if (!key) {
      return null;
    }
    return { ...base, development: true, appHash: null, developerKey: key, manifestHash: p.manifestHash, autoUpdate: false };
  }
  return {
    ...base,
    development: false,
    appHash: p.appHash,
    developerKey: null,
    manifestHash: p.manifestHash,
    autoUpdate: choice.autoUpdate,
  };
}

/** The developer key as people compare it: its first 16 characters. */
export function keyFingerprint(key: string): string {
  return key.slice(0, 16);
}

/** Only an https source is a link; anything else (a path) is shown as given. */
export function isLinkSource(source: string | null | undefined): boolean {
  return !!source && /^https:\/\//i.test(source);
}

// --- Plain language ------------------------------------------------------------------------------

/** A schedule period, "90 s", "10 min", "2 h", "1 d": units that read the same in every UI language. */
export function formatInterval(seconds: number): string {
  const units: [number, string][] = [
    [86400, "d"],
    [3600, "h"],
    [60, "min"],
  ];
  for (const [size, unit] of units) {
    if (seconds >= size && seconds % size === 0) {
      return `${seconds / size} ${unit}`;
    }
  }
  return `${seconds} s`;
}

/** The core words a changed schedule as `"<name> every <secs>s"` (previous `"every <secs>s"`). */
function parseSchedule(value: string): { name: string; every: number | null } {
  const m = /^(.*) every (\d+)s$/.exec(value);
  return m ? { name: m[1], every: Number(m[2]) } : { name: value, every: null };
}

/** "4 KB", "1.5 MB": units that read the same in every UI language. */
export function formatQuota(bytes: number): string {
  const trim = (n: number) => (Number.isInteger(n) ? String(n) : n.toFixed(1));
  if (bytes < 1024) {
    return `${bytes} B`;
  }
  if (bytes < 1024 * 1024) {
    return `${trim(bytes / 1024)} KB`;
  }
  return `${trim(bytes / (1024 * 1024))} MB`;
}

function joinList(t: Translate, items: readonly string[]): string {
  return items.join(t("mobile:bots.app.listSeparator"));
}

/** `"#ops"` / `"*"` as a person reads it. */
function channelLabel(t: Translate, value: string): string {
  if (value === "*") {
    return t("mobile:bots.app.plain.everyChannel");
  }
  return `#${value.startsWith("#") ? value.slice(1) : value}`;
}

function historyPhrase(t: Translate, days: number): string {
  return days > 0
    ? t("mobile:bots.app.plain.history", { count: days })
    : t("mobile:bots.app.plain.historyNone");
}

function eventsLine(t: Translate, events: readonly EventKind[]): string {
  const created = events.includes("created");
  const edited = events.includes("edited");
  if (created && edited) {
    return t("mobile:bots.app.plain.eventsBoth");
  }
  if (created) {
    return t("mobile:bots.app.plain.eventsCreated");
  }
  if (edited) {
    return t("mobile:bots.app.plain.eventsEdited");
  }
  return t("mobile:bots.app.plain.eventsNone");
}

const TRIGGER_KEYS: Record<TriggerKind, string> = {
  messages: "mobile:bots.app.triggers.messages",
  mentions: "mobile:bots.app.triggers.mentions",
  commands: "mobile:bots.app.triggers.commands",
  schedule: "mobile:bots.app.triggers.schedule",
  requests: "mobile:bots.app.triggers.requests",
  webhook: "mobile:bots.app.triggers.webhook",
};

const ACTION_KEYS: Record<ActionKind, string> = {
  post: "mobile:bots.app.actions.post",
  reply: "mobile:bots.app.actions.reply",
  react: "mobile:bots.app.actions.react",
  edit_own: "mobile:bots.app.actions.edit_own",
  delete_own: "mobile:bots.app.actions.delete_own",
  transfer_history: "mobile:bots.app.actions.transfer_history",
};

const EVENT_KEYS: Record<EventKind, string> = {
  created: "mobile:bots.app.events.created",
  edited: "mobile:bots.app.events.edited",
};

function triggerLabel(t: Translate, v: string): string {
  const key = TRIGGER_KEYS[v as TriggerKind];
  return key ? t(key) : v;
}

function actionLabel(t: Translate, v: string): string {
  const key = ACTION_KEYS[v as ActionKind];
  return key ? t(key) : v;
}

function eventLabel(t: Translate, v: string): string {
  const key = EVENT_KEYS[v as EventKind];
  return key ? t(key) : v;
}

export interface PlainLine {
  id: "reads" | "events" | "wakes" | "can" | "egress" | "state";
  text: string;
}

/**
 * The manifest in plain language, one line per item. Pass the grants that apply: for an approved
 * App the effective scope (manifest ∩ grants) is what is described, never the raw request.
 */
export function plainLines(t: Translate, manifest: AppManifest, grants: AdminGrants): PlainLine[] {
  const eff = effectiveGrants(manifest, grants);
  const reads =
    eff.channels.length === 0
      ? t("mobile:bots.app.plain.readsNothing")
      : t("mobile:bots.app.plain.reads", {
          channels: joinList(
            t,
            eff.channels.map((c) => channelLabel(t, c.name)),
          ),
          history: historyPhrase(t, eff.history),
        });
  const wakes =
    manifest.triggers.length === 0
      ? t("mobile:bots.app.plain.wakesNever")
      : t("mobile:bots.app.plain.wakesOn", { items: joinList(t, manifest.triggers.map((x) => triggerLabel(t, x))) });
  const can =
    eff.actions.length === 0
      ? t("mobile:bots.app.plain.canNothing")
      : t("mobile:bots.app.plain.can", { items: joinList(t, eff.actions.map((x) => actionLabel(t, x))) });
  const egress =
    eff.egress.length === 0
      ? t("mobile:bots.app.noEgressMark")
      : t("mobile:bots.app.plain.sendsTo", { hosts: joinList(t, eff.egress) });
  const state =
    manifest.state.quota > 0
      ? t("mobile:bots.app.plain.keeps", { size: formatQuota(manifest.state.quota) })
      : t("mobile:bots.app.plain.keepsNothing");
  return [
    { id: "reads", text: reads },
    { id: "events", text: eventsLine(t, eff.events) },
    { id: "wakes", text: wakes },
    { id: "can", text: can },
    { id: "egress", text: egress },
    { id: "state", text: state },
  ];
}

/** One diff row in plain language. */
export function changeText(t: Translate, c: ManifestChange): string {
  const added = c.kind === "added";
  switch (c.field) {
    case "trigger":
      return t(added ? "mobile:bots.app.change.triggerAdded" : "mobile:bots.app.change.triggerRemoved", {
        value: triggerLabel(t, c.value),
      });
    case "channel":
      return t(added ? "mobile:bots.app.change.channelAdded" : "mobile:bots.app.change.channelRemoved", {
        value: channelLabel(t, c.value),
      });
    case "history":
      return t("mobile:bots.app.change.history", {
        value: historyPhrase(t, Number(c.value) || 0),
        was: historyPhrase(t, Number(c.previous) || 0),
      });
    case "event":
      return t(added ? "mobile:bots.app.change.eventAdded" : "mobile:bots.app.change.eventRemoved", {
        value: eventLabel(t, c.value),
      });
    case "action":
      return t(added ? "mobile:bots.app.change.actionAdded" : "mobile:bots.app.change.actionRemoved", {
        value: actionLabel(t, c.value),
      });
    case "egress":
      return t(added ? "mobile:bots.app.change.egressAdded" : "mobile:bots.app.change.egressRemoved", {
        value: c.value,
      });
    case "command": {
      const name = `/${c.value.startsWith("/") ? c.value.slice(1) : c.value}`;
      if (c.kind === "added") {
        return t("mobile:bots.app.change.commandAdded", { value: name });
      }
      if (c.kind === "removed") {
        return t("mobile:bots.app.change.commandRemoved", { value: name });
      }
      // Switched between a posted command and a private request; `previous` is what it was.
      return t(
        c.previous === "request" ? "mobile:bots.app.change.commandToPosted" : "mobile:bots.app.change.commandToRequest",
        { value: name },
      );
    }
    case "schedule": {
      if (c.kind === "added") {
        return t("mobile:bots.app.change.scheduleAdded", { value: c.value });
      }
      if (c.kind === "removed") {
        return t("mobile:bots.app.change.scheduleRemoved", { value: c.value });
      }
      const now = parseSchedule(c.value);
      const was = parseSchedule(`x ${c.previous ?? ""}`).every;
      if (now.every === null || was === null) {
        return t("mobile:bots.app.change.scheduleChangedPlain", { value: c.value });
      }
      return t("mobile:bots.app.change.scheduleChanged", {
        value: now.name,
        every: formatInterval(now.every),
        was: formatInterval(was),
      });
    }
    case "state_quota":
      return t("mobile:bots.app.change.quota", {
        value: formatQuota(Number(c.value) || 0),
        was: formatQuota(Number(c.previous) || 0),
      });
    default:
      return c.value;
  }
}

/** First approval (nothing approved yet) or an update of the approved App; null when nothing waits. */
export function pendingKind(view: BotAppView | null | undefined): "first" | "update" | null {
  if (!view?.pending) {
    return null;
  }
  return view.approved ? "update" : "first";
}
