/** The breadcrumb trail for a pathname (#1966): pure, so `tests/breadcrumb-trail.test.ts` can walk
 *  every route in `router.tsx`. Every key names its namespace. */

export interface Segment {
  label: string;
  to: string;
}

export interface TrailContext {
  groups?: { id: string; name: string; channels: { id: string; name: string }[] }[];
  /** `appStore.channels`: a group's channels by group id. */
  channels: Record<string, { id: string; name: string }[] | undefined>;
  dms: { id: string; user2_identifier: string }[];
}

type T = (key: string, options?: Record<string, unknown>) => string;

/** Pages under Account in the settings hub: path → the label key it is listed under. */
const ACCOUNT_PAGES: Record<string, string> = {
  "/diagnostics": "search:pages.diagnosticsName",
  "/open-source": "search:pages.openSourceName",
  "/shortcuts": "search:pages.shortcutsName",
  "/update": "search:pages.updateName",
};

/** Top-level pages of their own, one step past Home. */
const TOP_PAGES: Record<string, string> = {
  "/saved": "search:pages.savedName",
  "/vault": "search:pages.vaultName",
  "/arcade": "search:pages.arcadeName",
};

export function breadcrumbTrail(pathname: string, ctx: TrailContext, t: T): Segment[] {
  const groupsWithChannels = ctx.groups;
  const channels = ctx.channels;
  const dmConversations = ctx.dms;
  const out: Segment[] = [{ label: t("nav:breadcrumb.home"), to: "/" }];

  if (pathname === "/") {
    return out;
  }

  if (pathname.startsWith("/groups")) {
    out.push({ label: t("nav:breadcrumb.groups"), to: "/groups" });

    const groupIdMatch = pathname.match(/^\/groups\/([^/]+)/);
    const groupId = groupIdMatch?.[1];

    if (groupId === "new") {
      out.push({ label: t("nav:breadcrumb.createGroup"), to: "/groups/new" });
    } else if (groupId === "search") {
      out.push({ label: t("nav:breadcrumb.findGroup"), to: "/groups/search" });
    } else if (groupId) {
      const group = groupsWithChannels?.find((g) => g.id === groupId);
      if (group) {
        out.push({ label: group.name, to: `/groups/${groupId}` });
      }

      if (pathname.includes("/channels/")) {
        const channelIdMatch = pathname.match(/\/channels\/([^/]+)/);
        const channelId = channelIdMatch?.[1];
        if (channelId === "new") {
          out.push({ label: t("nav:breadcrumb.newChannel"), to: `/groups/${groupId}/channels/new` });
        } else if (channelId) {
          const groupChannels = channels[groupId] ?? [];
          const ch = groupChannels.find((c) => c.id === channelId);
          if (ch) {
            out.push({ label: ch.name, to: `/groups/${groupId}/channels/${channelId}` });
          }
          if (pathname.endsWith("/rename")) {
            out.push({ label: t("channels:renameChannel.pageTitle"), to: pathname });
          }
        }
      } else if (pathname.includes("/voice/")) {
        const channelIdMatch = pathname.match(/\/voice\/([^/]+)/);
        const channelId = channelIdMatch?.[1];
        const ch = group?.channels.find((c) => c.id === channelId);
        out.push({
          label: ch?.name ?? t("nav:breadcrumb.voice"),
          to: `/groups/${groupId}/voice/${channelId}`,
        });
      } else if (/^\/groups\/[^/]+\/bots(\/|$)/.test(pathname)) {
        // Bots (#1182): Bots → Add a bot, or Bots → Bot → Add a host / Delete bot.
        out.push({ label: t("nav:breadcrumb.bots"), to: `/groups/${groupId}/bots` });
        const botId = pathname.match(/\/bots\/([^/]+)/)?.[1];
        if (botId === "new") {
          out.push({ label: t("nav:breadcrumb.addBot"), to: pathname });
        } else if (botId) {
          out.push({ label: t("nav:breadcrumb.bot"), to: `/groups/${groupId}/bots/${botId}` });
          if (pathname.endsWith("/add-host")) {
            out.push({ label: t("nav:breadcrumb.addBotHost"), to: pathname });
          } else if (pathname.endsWith("/delete")) {
            out.push({ label: t("nav:breadcrumb.deleteBot"), to: pathname });
          }
        }
      } else if (pathname.endsWith("/join-requests")) {
        out.push({ label: t("nav:breadcrumb.joinRequests"), to: `/groups/${groupId}/join-requests` });
      } else if (pathname.endsWith("/invite")) {
        out.push({ label: t("nav:breadcrumb.inviteMember"), to: `/groups/${groupId}/invite` });
      } else if (pathname.endsWith("/leave")) {
        out.push({ label: t("nav:breadcrumb.leaveGroup"), to: `/groups/${groupId}/leave` });
      } else if (pathname.endsWith("/members")) {
        out.push({ label: t("nav:breadcrumb.members"), to: `/groups/${groupId}/members` });
      } else if (pathname.includes("/members/") && pathname.endsWith("/kick")) {
        out.push({ label: t("nav:breadcrumb.members"), to: `/groups/${groupId}/members` });
        out.push({ label: t("nav:breadcrumb.removeMember"), to: pathname });
      } else if (pathname.endsWith("/rename")) {
        out.push({ label: t("channels:renameGroup.pageTitle"), to: pathname });
      } else if (pathname.endsWith("/emoji")) {
        out.push({ label: t("emoji:manage.title"), to: pathname });
      }
    }
  } else if (pathname.startsWith("/dms")) {
    out.push({ label: t("nav:breadcrumb.directMessages"), to: "/dms" });

    const convIdMatch = pathname.match(/^\/dms\/([^/]+)/);
    const conversationId = convIdMatch?.[1];

    if (conversationId === "new") {
      out.push({ label: t("nav:breadcrumb.newMessage"), to: "/dms/new" });
    } else if (conversationId === "requests") {
      out.push({ label: t("nav:breadcrumb.requests"), to: "/dms/requests" });
    } else if (conversationId === "blocked") {
      out.push({ label: t("nav:breadcrumb.blockedUsers"), to: "/dms/blocked" });
    } else if (conversationId) {
      const conv = dmConversations.find((c) => c.id === conversationId);
      if (conv) {
        out.push({
          label: `@${conv.user2_identifier}`,
          to: `/dms/${conversationId}`,
        });
      }
      if (pathname.endsWith("/settings")) {
        out.push({
          label: t("nav:breadcrumb.conversationSettings"),
          to: `/dms/${conversationId}/settings`,
        });
      }
    }
  } else if (pathname === "/settings") {
    out.push({ label: t("nav:breadcrumb.account"), to: "/settings" });
  } else if (pathname === "/preferences") {
    out.push({ label: t("nav:breadcrumb.account"), to: "/settings" });
    out.push({ label: t("nav:breadcrumb.preferences"), to: "/preferences" });
  } else if (pathname === "/preferences/language") {
    out.push({ label: t("nav:breadcrumb.account"), to: "/settings" });
    out.push({ label: t("nav:breadcrumb.preferences"), to: "/preferences" });
    out.push({ label: t("nav:breadcrumb.language"), to: "/preferences/language" });
  } else if (pathname === "/user") {
    out.push({ label: t("nav:breadcrumb.account"), to: "/settings" });
    out.push({ label: t("nav:breadcrumb.userSettings"), to: "/user" });
  } else if (pathname.startsWith("/user/")) {
    out.push({ label: t("nav:breadcrumb.profile"), to: pathname });
  } else if (pathname === "/security") {
    out.push({ label: t("nav:breadcrumb.account"), to: "/settings" });
    out.push({ label: t("nav:breadcrumb.security"), to: "/security" });
  } else if (pathname === "/security/events") {
    out.push({ label: t("nav:breadcrumb.account"), to: "/settings" });
    out.push({ label: t("nav:breadcrumb.security"), to: "/security" });
    out.push({ label: t("nav:breadcrumb.securityEvents"), to: "/security/events" });
  } else if (pathname === "/security/copy-history") {
    out.push({ label: t("nav:breadcrumb.account"), to: "/settings" });
    out.push({ label: t("nav:breadcrumb.security"), to: "/security" });
    out.push({ label: t("nav:breadcrumb.copyHistory"), to: "/security/copy-history" });
  } else if (pathname === "/security/change-pin" || pathname === "/security/link-device") {
    // #1966: every page names its path, not just "Home". Labels reuse each page's own title.
    out.push({ label: t("nav:breadcrumb.account"), to: "/settings" });
    out.push({ label: t("nav:breadcrumb.security"), to: "/security" });
    out.push({
      label: pathname.endsWith("change-pin") ? t("settings:changePin.title") : t("settings:linkDevice.heading"),
      to: pathname,
    });
  } else if (pathname.startsWith("/security/history-transfer/")) {
    out.push({ label: t("nav:breadcrumb.account"), to: "/settings" });
    out.push({ label: t("nav:breadcrumb.security"), to: "/security" });
    out.push({ label: t("nav:breadcrumb.historyRequest"), to: pathname });
  } else if (pathname === "/voice-settings") {
    out.push({ label: t("nav:breadcrumb.account"), to: "/settings" });
    out.push({ label: t("nav:breadcrumb.voiceSettings"), to: "/voice-settings" });
  } else if (pathname === "/invites") {
    out.push({ label: t("nav:breadcrumb.invites"), to: "/invites" });
  } else if (pathname === "/join" || pathname.startsWith("/invite/")) {
    out.push({ label: t("nav:breadcrumb.joinAGroup"), to: "/join" });
  } else if (pathname === "/join-requests") {
    out.push({ label: t("nav:breadcrumb.joinRequests"), to: "/join-requests" });
  } else if (pathname === "/search") {
    out.push({ label: t("nav:breadcrumb.search"), to: "/search" });
  } else if (ACCOUNT_PAGES[pathname.split("/").slice(0, 2).join("/")]) {
    const base = pathname.split("/").slice(0, 2).join("/");
    out.push({ label: t("nav:breadcrumb.account"), to: "/settings" });
    out.push({ label: t(ACCOUNT_PAGES[base]), to: base });
    // An open-source package's own page.
    const rest = pathname.slice(base.length + 1);
    if (rest) {
      out.push({ label: decodeURIComponent(rest), to: pathname });
    }
  } else if (TOP_PAGES[pathname]) {
    out.push({ label: t(TOP_PAGES[pathname]), to: pathname });
  } else if (pathname.startsWith("/report/")) {
    out.push({ label: t("chat:report.submit"), to: pathname });
  } else if (pathname.startsWith("/call/") || pathname.startsWith("/incoming-call/")) {
    out.push({ label: t("voice:call.defaultTitle"), to: pathname });
  } else if (pathname === "/terminal") {
    out.push({ label: t("settings:appearance.skinTerminal"), to: pathname });
  }

  return out;
}
