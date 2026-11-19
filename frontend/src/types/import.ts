// Mirrors `pollis_import::plan` and `pollis_core::commands::import` (#1180 Discord, #1179 Slack). Keep in sync.

/** `pollis_import::SourceKind`: which platform an export comes from. */
export type ImportSource = "discord" | "slack";

type Media = { Local: string } | { Remote: string };

export type ChannelNote =
  | { kind: "announcement" }
  | { kind: "forum" }
  | { kind: "stage" }
  | { kind: "renamed"; from: string }
  | { kind: "archived" };

export type EmojiNote =
  | { kind: "renamed"; collided_with: string }
  | { kind: "no_valid_shortcode" }
  | { kind: "image_remote" }
  | { kind: "image_missing" }
  | { kind: "alias_target_missing" };

export type SkipReason =
  | { kind: "orphan_thread" }
  | { kind: "direct" }
  | { kind: "unsupported"; source_type: string }
  | { kind: "archived" };

export type PlanNote =
  | { kind: "no_emails" }
  | { kind: "email_invites"; with_email: number; without_email: number }
  | { kind: "no_snapshot" }
  | { kind: "no_workspace_snapshot" }
  | { kind: "archived_skipped"; count: number }
  | { kind: "user_groups_not_imported"; count: number }
  | { kind: "private_channels"; count: number }
  | { kind: "roles_collapsed"; discarded: number }
  | { kind: "history_not_imported"; messages: number };

type Permission =
  | "view"
  | "send_messages"
  | "read_history"
  | "connect"
  | "speak"
  | "manage_channel"
  | "manage_messages"
  | "manage_permissions"
  | "attach_files"
  | "add_reactions"
  | "mention_everyone"
  | "create_threads";

export interface OverwritePlan {
  target: string;
  member: boolean;
  allow: Permission[];
  deny: Permission[];
  other_bits: number;
}

interface HistorySummary {
  messages: number;
  first_ms: number | null;
  last_ms: number | null;
}

interface CategoryPlan {
  source_id: string;
  name: string;
  exists: boolean;
}

export interface ChannelPlan {
  source_id: string;
  source_name: string;
  name: string;
  kind: "text" | "voice";
  category: string | null;
  description: string | null;
  exists: boolean;
  private: boolean;
  archived: boolean;
  overwrites: OverwritePlan[];
  history: HistorySummary;
  notes: ChannelNote[];
}

export interface EmojiPlan {
  source_id: string;
  source_name: string;
  shortcode: string | null;
  exists: boolean;
  animated: boolean;
  uses: number;
  image: Media | null;
  alias_of: string | null;
  note: EmojiNote | null;
}

export interface RolePlan {
  source_id: string;
  name: string;
  maps_to: "admin" | "member" | "discarded" | "bot";
  holders: number;
}

export interface PersonPlan {
  source_id: string;
  username: string;
  display_name: string;
  is_bot: boolean;
  admin: boolean;
  roles: string[];
  email: string | null;
  channels: string[];
  messages: number;
}

interface UserGroupPlan {
  source_id: string;
  handle: string;
  name: string;
  description: string | null;
  members: string[];
}

interface ImportTotals {
  categories: number;
  channels: number;
  to_create_categories: number;
  to_create_channels: number;
  to_upload_emoji: number;
  emoji: number;
  people: number;
  messages: number;
  overwrites: number;
  private_channels: number;
  archived_skipped: number;
  people_with_email: number;
  user_groups: number;
}

export interface ImportPlan {
  source: ImportSource;
  group: { name: string; icon: Media | null };
  categories: CategoryPlan[];
  channels: ChannelPlan[];
  skipped_channels: { name: string; reason: SkipReason }[];
  emoji: EmojiPlan[];
  roles: RolePlan[];
  permissions_known: boolean;
  people: PersonPlan[];
  user_groups: UserGroupPlan[];
  totals: ImportTotals;
  notes: PlanNote[];
  warnings: string[];
}

interface ImportTarget {
  group_id: string;
  name: string;
}

export interface ImportPreview {
  source: ImportSource;
  path: string;
  plan: ImportPlan;
  invite_sheet: string;
  target: ImportTarget | null;
  candidates: ImportTarget[];
  include_archived: boolean;
}

/** `pollis_core::commands::import::ImportApplyOptions`. */
export interface ImportApplyOptions {
  include_archived: boolean;
  invite_by_email: boolean;
}

interface ImportFailure {
  kind: "category" | "channel" | "emoji" | "icon" | "order" | "invite_link" | "invite";
  name: string;
  error: string;
}

export type EmailInviteOutcome = "invited" | "already_invited" | "already_member" | "you" | "no_account" | "failed";

export interface EmailInvite {
  username: string;
  display_name: string;
  email: string;
  outcome: EmailInviteOutcome;
}

export interface ImportResult {
  group_id: string;
  group_name: string;
  created_group: boolean;
  icon_set: boolean;
  categories_created: number;
  channels_created: number;
  channels_moved: number;
  emoji_uploaded: number;
  categories_existing: number;
  channels_existing: number;
  emoji_existing: number;
  failures: ImportFailure[];
  invite_link: { id: string; token: string; url: string; expires_at: string | null; max_uses: number | null } | null;
  invite_link_existing: boolean;
  email_invites: EmailInvite[];
}

/** The `target` argument meaning "a new group" (`pollis_core::commands::import::TARGET_NEW`). */
export const IMPORT_TARGET_NEW = "new";
