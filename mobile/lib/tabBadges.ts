// The bottom tab bar's badges (#1561). Each tab's number is its unread
// messages plus the pending items waiting on the user there, counted in the
// way Slack and Discord count message requests into the DM badge: Direct adds
// pending message requests, Groups adds pending group invites. Pure, so the
// counting and the spoken label are pinned by tests/tab-badges.test.ts.

/** What one tab's badge is made of. */
export interface TabBadge {
  unread: number;
  /** Message requests (Direct) or group invites (Groups). */
  pending: number;
}

export interface TabBadgeInput {
  /** Per-conversation unread counts (the app store's `unreadCounts`). */
  unreadCounts: Record<string, number>;
  /** Every channel id the Groups tab lists. */
  channelIds: string[];
  /** Every DM conversation id the Direct tab lists. */
  dmIds: string[];
  /** Pending message requests (`useDMRequests`). */
  requestCount: number;
  /** Pending group invites (`usePendingGroupInvites`). */
  inviteCount: number;
}

function sumUnread(unreadCounts: Record<string, number>, ids: string[]): number {
  return ids.reduce((sum, id) => sum + (unreadCounts[id] ?? 0), 0);
}

export function tabBadges(input: TabBadgeInput): Record<string, TabBadge> {
  return {
    groups: {
      unread: sumUnread(input.unreadCounts, input.channelIds),
      pending: input.inviteCount,
    },
    direct: {
      unread: sumUnread(input.unreadCounts, input.dmIds),
      pending: input.requestCount,
    },
  };
}

/** The number drawn on the badge; 0 draws none. */
export function badgeTotal(badge: TabBadge | undefined): number {
  if (!badge) {
    return 0;
  }
  return badge.unread + badge.pending;
}

/** The text drawn on the badge: the total, capped at "99+". */
export function badgeText(total: number): string {
  return total > 99 ? "99+" : String(total);
}

type Translate = (key: string, opts: Record<string, unknown>) => string;

/**
 * The tab's spoken label. The number alone would not say what is waiting, so
 * unread and pending are announced separately: "Direct, 2 unread, 1 message
 * request". Each catalogue entry takes the label built so far as `{{label}}`.
 */
export function tabA11yLabel(
  t: Translate,
  tab: string,
  label: string,
  badge: TabBadge | undefined,
): string {
  let spoken = label;
  if (!badge) {
    return spoken;
  }
  if (badge.unread > 0) {
    spoken = t("mobile:tabs.withUnread", { label: spoken, count: badge.unread });
  }
  if (badge.pending > 0) {
    spoken =
      tab === "groups"
        ? t("mobile:tabs.withInvites", { label: spoken, count: badge.pending })
        : t("mobile:tabs.withRequests", { label: spoken, count: badge.pending });
  }
  return spoken;
}
