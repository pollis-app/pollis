// Bot Apps (#2005): what a bot runs, the admins' signed approval of it, and what members see.
// Mirrors pollis-core/src/commands/bots/apps.rs (views: camelCase) and pollis-app-manifest
// (the manifest, grants and diff: snake_case, as the manifest file itself).

export type TriggerKind = "messages" | "mentions" | "commands" | "schedule" | "requests" | "webhook";
export type EventKind = "created" | "edited";
export type ActionKind = "post" | "reply" | "react" | "edit_own" | "delete_own" | "transfer_history";

/** `pollis-app.json`. */
export interface AppManifest {
  $schema?: string;
  name: string;
  version: string;
  /** May be "". */
  description: string;
  triggers: TriggerKind[];
  /** `channels`: "#name" or "*"; `history` in days. */
  read: { channels: string[]; history: number; events: EventKind[] };
  actions: ActionKind[];
  /** Bytes. */
  state: { quota: number };
  /** Exact host names. */
  egress: string[];
}

/** One granted channel: its id and its name (lowercase, no '#'). */
export interface ChannelGrant {
  id: string;
  name: string;
}

/** The admin's narrowing of a manifest; never wider than it. */
export interface AdminGrants {
  channels: ChannelGrant[];
  /** Days, at most the manifest's `read.history`. */
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

/** One change between the approved manifest and the waiting one. */
export interface ManifestChange {
  field: ChangeField;
  kind: ChangeKind;
  /**
   * The item added or removed ("#ops", "api.linear.app", "post", a command or schedule name), a
   * number's new value, or a switched command's or changed schedule's name.
   */
  value: string;
  /** A number's old value; a switched command's old kind ("command" | "request"). */
  previous?: string;
  /** A changed schedule's period in seconds, after and before the change. */
  every_secs?: number;
  previous_every_secs?: number;
  /** The App may see or do more than before. */
  widening: boolean;
}

type BotAppRunState = "running" | "awaiting_approval" | "refused";

export interface ApprovedAppView {
  seq: number;
  development: boolean;
  /** Production: 64-char hex. */
  appHash: string | null;
  /** Development: the developer's base64 public key. */
  developerKey: string | null;
  manifestHash: string;
  manifest: AppManifest;
  grants: AdminGrants;
  /** As given: a path or an https URL. */
  source: string | null;
  autoUpdate: boolean;
  approverUserId: string;
  /** Unix seconds. */
  issuedAt: number;
  cannotSendOutside: boolean;
}

/** What the runtime has and an admin may approve. */
export interface PendingAppView {
  appHash: string;
  manifestHash: string;
  manifest: AppManifest;
  source: string | null;
  /** Set: the runtime runs a development build signed by this key. */
  developerKey: string | null;
  /** Against the approved manifest; [] on a first approval. */
  changes: ManifestChange[];
  widens: boolean;
  /** First approval: everything requested; update: the current grants carried over. */
  suggestedGrants: AdminGrants;
}

export interface BotAppView {
  botUserId: string;
  groupId: string;
  approved: ApprovedAppView | null;
  /** The stored approval does not verify on this device. */
  approvalRefusal: { code: string; detail: string } | null;
  report: { state: BotAppRunState; reason: string; detail: string; updatedAt: string } | null;
  pending: PendingAppView | null;
  /** What the runtime runs now, while it runs; `autoUpdated` when it took a newer build from the approved source itself. */
  running: { appHash: string; version: string; autoUpdated: boolean } | null;
  /** The group's text channels the shown manifest asks for. */
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
  /** Production only. */
  autoUpdate: boolean;
}

/** A group's approved App, as every member reads it. */
export interface GroupAppView {
  botUserId: string;
  development: boolean;
  manifest: AppManifest;
  manifestJson: string;
  grants: AdminGrants;
  cannotSendOutside: boolean;
}
