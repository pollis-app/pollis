// The tab bar's badges (#1561): unread plus pending requests/invites, Slack/Discord style. Pure.

export interface TabBadge {
  unread: number;
  pending: number;
}

export interface TabBadgeInput {
  unreadCounts: Record<string, number>;
  channelIds: string[];
  dmIds: string[];
  requestCount: number;
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

export function badgeTotal(badge: TabBadge | undefined): number {
  if (!badge) {
    return 0;
  }
  return badge.unread + badge.pending;
}

export function badgeText(total: number): string {
  return total > 99 ? "99+" : String(total);
}

type Translate = (key: string, opts: Record<string, unknown>) => string;

/** Unread and pending are announced separately: "Direct, 2 unread, 1 message request". */
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
