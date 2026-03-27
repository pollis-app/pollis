import React, { useEffect, useState, useMemo, useCallback } from "react";
import { Outlet, useRouter, useRouterState } from "@tanstack/react-router";
import { useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { TitleBar } from "./TitleBar";
import { VoiceBar } from "../Voice/VoiceBar";
import { LoadingSpinner } from "../ui/LoaderSpinner";
import { SearchPanel } from "../SearchPanel";
import { useAppStore } from "../../stores/appStore";
import { useUserGroupsWithChannels } from "../../hooks/queries/useGroups";
import { useDMConversations } from "../../hooks/queries/useMessages";
import { useLiveKitRealtime } from "../../hooks/useLiveKitRealtime";
import { useBadge } from "../../hooks/useBadge";
import { Mail } from "lucide-react";

/**
 * AppShell is the root route component rendered by RouterProvider.
 * It owns the terminal chrome (TitleBar, VoiceBar, bottom breadcrumb bar)
 * and renders the matched child route via <Outlet />.
 */
export const AppShell: React.FC = () => {
  const [isSyncing, setIsSyncing] = useState(false);
  const [isSearchOpen, setIsSearchOpen] = useState(false);
  const queryClient = useQueryClient();
  const router = useRouter();

  const {
    channels,
    setGroups,
    setChannels,
    activeVoiceChannelId,
    statusBarAlert,
    setStatusBarAlert,
  } = useAppStore();

  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const { data: dmConversations = [] } = useDMConversations();

  const currentUser = useAppStore((s) => s.currentUser);

  // On startup, apply any MLS Welcome messages that arrived while offline.
  useEffect(() => {
    if (!currentUser) {
      return;
    }
    invoke('poll_mls_welcomes', { userId: currentUser.id }).catch((err) => {
      console.warn('[mls] poll_mls_welcomes failed:', err);
    });
  }, [currentUser?.id]);

  // Maintain a LiveKit room connection for the active channel/conversation
  useLiveKitRealtime();

  // Sync unread count to OS dock/taskbar badge
  useBadge();

  // Sync groups+channels into the store once loaded
  useEffect(() => {
    if (!groupsWithChannels) {
      return;
    }
    setGroups(groupsWithChannels);
    for (const g of groupsWithChannels) {
      setChannels(g.id, g.channels);
    }
  }, [groupsWithChannels, setGroups, setChannels]);

  const closeSearch = useCallback(() => setIsSearchOpen(false), []);

  // Cmd/Ctrl+K — open search panel
  useEffect(() => {
    const handle = (e: KeyboardEvent) => {
      if (e.key === "k" && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        setIsSearchOpen((prev) => !prev);
      }
    };
    window.addEventListener("keydown", handle);
    return () => window.removeEventListener("keydown", handle);
  }, []);

  // Global Esc handler — navigate back in history (skip when search panel is open)
  useEffect(() => {
    const handle = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !isSearchOpen) {
        router.history.back();
      }
    };
    window.addEventListener("keydown", handle);
    return () => window.removeEventListener("keydown", handle);
  }, [router, isSearchOpen]);

  // Cmd/Ctrl+R — refetch all queries without a page reload
  useEffect(() => {
    const handle = (e: KeyboardEvent) => {
      if (e.key === "r" && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        setIsSyncing(true);
        queryClient.invalidateQueries().finally(() => setIsSyncing(false));
      }
    };
    window.addEventListener("keydown", handle);
    return () => window.removeEventListener("keydown", handle);
  }, [queryClient]);

  // Auto-focus when the window gains focus (e.g. switching back from another app)
  useEffect(() => {
    const handleWindowFocus = () => {
      if (!document.activeElement || document.activeElement === document.body) {
        const menu = document.querySelector<HTMLElement>('[role="menu"]');
        if (menu) {
          menu.focus();
          return;
        }
        const input = document.querySelector<HTMLElement>(
          'input:not([type="hidden"]), textarea'
        );
        input?.focus();
      }
    };
    window.addEventListener("focus", handleWindowFocus);
    return () => window.removeEventListener("focus", handleWindowFocus);
  }, []);

  // ─── Breadcrumb derived from current route location ──────────────────────────

  const pathname = useRouterState({ select: (s) => s.location.pathname });

  // Clear the status bar alert when the user navigates to the room that
  // triggered it.
  useEffect(() => {
    if (statusBarAlert && pathname.includes(statusBarAlert.roomId)) {
      setStatusBarAlert(null);
    }
  }, [pathname, statusBarAlert, setStatusBarAlert]);

  // Chat screens: channel view or DM conversation (not /new)
  const isChatScreen = useMemo(() => {
    const channelMatch = pathname.match(/^\/groups\/[^/]+\/channels\/([^/]+)/);
    if (channelMatch && channelMatch[1] !== "new") {
      return true;
    }
    const dmMatch = pathname.match(/^\/dms\/([^/]+)/);
    if (dmMatch && dmMatch[1] !== "new") {
      return true;
    }
    return false;
  }, [pathname]);

  const breadcrumb = useMemo(() => {
    if (pathname === "/") {
      return "";
    }

    const segments: string[] = [];

    if (pathname.startsWith("/groups")) {
      segments.push("Groups");

      const groupIdMatch = pathname.match(/^\/groups\/([^/]+)/);
      const groupId = groupIdMatch?.[1];

      if (groupId && groupId !== "new" && groupId !== "search") {
        const group = groupsWithChannels?.find((g) => g.id === groupId);
        if (group) {
          segments.push(group.name);
        }

        if (pathname.includes("/channels/")) {
          const channelIdMatch = pathname.match(/\/channels\/([^/]+)/);
          const channelId = channelIdMatch?.[1];

          if (channelId && channelId !== "new") {
            const groupChannels = channels[groupId] ?? [];
            const ch = groupChannels.find((c) => c.id === channelId);
            if (ch) {
              segments.push(ch.name);
            }
          } else if (pathname.endsWith("/channels/new")) {
            segments.push("New Channel");
          }
        } else if (pathname.includes("/voice/")) {
          const channelIdMatch = pathname.match(/\/voice\/([^/]+)/);
          const channelId = channelIdMatch?.[1];
          const group = groupsWithChannels?.find((g) => g.id === groupId);
          const ch = group?.channels.find((c) => c.id === channelId);
          segments.push(`[v] ${ch?.name ?? "voice"}`);
        } else if (pathname.endsWith("/join-requests")) {
          segments.push("Join Requests");
        } else if (pathname.endsWith("/invite")) {
          segments.push("Invite Member");
        } else if (pathname.endsWith("/leave")) {
          segments.push("Leave Group");
        }
      } else if (groupId === "new") {
        segments.push("Create Group");
      } else if (groupId === "search") {
        segments.push("Find Group");
      }
    } else if (pathname.startsWith("/dms")) {
      segments.push("Direct Messages");

      const convIdMatch = pathname.match(/^\/dms\/([^/]+)/);
      const conversationId = convIdMatch?.[1];

      if (conversationId && conversationId !== "new") {
        const conv = dmConversations.find((c) => c.id === conversationId);
        if (conv) {
          segments.push(`@${conv.user2_identifier}`);
        }
        if (pathname.endsWith("/settings")) {
          segments.push("Conversation Settings");
        }
      } else if (conversationId === "new") {
        segments.push("New Message");
      }
    } else if (pathname === "/preferences") {
      segments.push("Preferences");
    } else if (pathname === "/settings") {
      segments.push("Settings");
    } else if (pathname === "/invites") {
      segments.push("Invites");
    } else if (pathname === "/search") {
      segments.push("Search");
    }

    return segments.join(" / ");
  }, [pathname, groupsWithChannels, dmConversations, channels]);

  // Find the voice channel name for the VoiceBar
  const voiceChannelName = useMemo(() => {
    if (!activeVoiceChannelId) {
      return "voice";
    }
    for (const g of groupsWithChannels ?? []) {
      const ch = g.channels.find((c) => c.id === activeVoiceChannelId);
      if (ch) {
        return ch.name;
      }
    }
    return "voice";
  }, [activeVoiceChannelId, groupsWithChannels]);

  return (
    <div
      data-testid="terminal-app"
      style={{
        height: "100%",
        width: "100%",
        display: "flex",
        flexDirection: "column",
        overflow: "hidden",
        background: "var(--c-bg)",
        position: "relative",
      }}
    >
      {/* Cmd/Ctrl+K search panel */}
      <SearchPanel isOpen={isSearchOpen} onClose={closeSearch} />

      {/* Title bar */}
      <TitleBar />

      {/* Sync indicator — floats top-right below title bar */}
      {isSyncing && (
        <div
          className="flex items-center gap-1.5 text-xs font-mono pointer-events-none"
          style={{
            position: "absolute",
            top: 36 + 7,
            right: 12,
            zIndex: 50,
            color: "var(--c-accent-dim)",
          }}
        >
          <span>syncing…</span>
          <LoadingSpinner size="sm" />
        </div>
      )}

      {/* Main content — matched child route renders here */}
      <div style={{ flex: 1, overflow: "hidden", display: "flex", flexDirection: "column" }}>
        <Outlet />
      </div>

      {/* VoiceBar — shown above bottom bar while user is in a voice channel */}
      {activeVoiceChannelId !== null && (
        <VoiceBar
          channelId={activeVoiceChannelId}
          channelName={voiceChannelName}
        />
      )}

      {/* Bottom bar — breadcrumb left, unread alert right */}
      {/* On chat screens, invert: dark bg with accent text. Otherwise: accent bg with dark text. */}
      <div
        style={{
          height: 28,
          flexShrink: 0,
          borderTop: "1px solid var(--c-border)",
          background: isChatScreen ? "var(--c-bg)" : "var(--c-accent)",
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          paddingLeft: 12,
          paddingRight: 12,
        }}
      >
        <span
          className="text-xs font-mono"
          style={{ color: isChatScreen ? "var(--c-accent)" : "black" }}
        >
          {breadcrumb}
        </span>
        {statusBarAlert && (
          <span
            className="text-xs font-mono status-bar-blink flex items-center gap-1"
            style={{ color: isChatScreen ? "var(--c-accent)" : "var(--c-surface)" }}
          >
            <Mail className="w-4 h-4" />: @{statusBarAlert.senderUsername}
          </span>
        )}
      </div>
    </div>
  );
};
