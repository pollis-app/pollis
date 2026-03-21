import React, { useEffect, useCallback, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { ArrowLeft } from "lucide-react";
import { TitleBar } from "./Layout/TitleBar";
import { TerminalMenu, type TerminalMenuItem } from "./ui/TerminalMenu";
import { MainContent } from "./Layout/MainContent";
import { Settings } from "../pages/Settings";
import { Preferences } from "../pages/Preferences";
import { CreateGroup } from "../pages/CreateGroup";
import { CreateChannel } from "../pages/CreateChannel";
import { SearchGroup } from "../pages/SearchGroup";
import { StartDM } from "../pages/StartDM";
import { Invites } from "../pages/Invites";
import { JoinRequests } from "../pages/JoinRequests";
import { InviteMember } from "../pages/InviteMember";
import { SearchView } from "./Search/SearchView";
import { useAppStore } from "../stores/appStore";
import { useUserGroupsWithChannels, usePendingInvites, useLeaveGroup } from "../hooks/queries/useGroups";
import { LoadingSpinner } from "./ui/LoaderSpinner";
import { useDMConversations } from "../hooks/queries/useMessages";
import { useLiveKitRealtime } from "../hooks/useLiveKitRealtime";
import { VoiceBar } from "./Voice/VoiceBar";
import { VoiceChannelView } from "./Voice/VoiceChannelView";
import type { GroupWithChannels } from "../services/api";
import type { DMConversation } from "../types";

// ─── View types ───────────────────────────────────────────────────────────────

type View =
  | { type: "root" }
  | { type: "groups" }
  | { type: "group"; group: GroupWithChannels }
  | { type: "channel" }
  | { type: "dms" }
  | { type: "dm" }
  | { type: "start-dm" }
  | { type: "create-group" }
  | { type: "search-group" }
  | { type: "create-channel" }
  | { type: "preferences" }
  | { type: "settings" }
  | { type: "invites" }
  | { type: "join-requests"; group: GroupWithChannels }
  | { type: "invite-member"; group: GroupWithChannels }
  | { type: "leave-group"; group: GroupWithChannels }
  | { type: "search" }
  | { type: "voice-channel"; channelName: string };

// ─── TerminalApp ──────────────────────────────────────────────────────────────

interface TerminalAppProps {
  onLogout: () => void;
  onDeleteAccount?: () => void;
}

export const TerminalApp: React.FC<TerminalAppProps> = ({ onLogout, onDeleteAccount }) => {
  const [viewStack, setViewStack] = React.useState<View[]>([{ type: "root" }]);
  const [isSyncing, setIsSyncing] = useState(false);
  const queryClient = useQueryClient();

  const {
    currentUser,
    selectedGroupId,
    selectedChannelId,
    channels,
    selectedConversationId,
    setSelectedGroupId,
    setSelectedChannelId,
    setSelectedConversationId,
    setGroups,
    setChannels,
    unreadCounts,
    markRead,
    activeVoiceChannelId,
    setActiveVoiceChannelId,
  } = useAppStore();

  const { data: groupsWithChannels, isLoading: groupsLoading, error: groupsError } = useUserGroupsWithChannels();
  const { data: dmConversations = [] } = useDMConversations();
  const { data: pendingInvites = [] } = usePendingInvites();
  const leaveGroupMutation = useLeaveGroup();

  console.log("about to use livekit realtime hook with selectedChannelId", selectedChannelId);
  // Maintain a LiveKit room connection for the active channel/conversation
  useLiveKitRealtime();

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

  const currentView = viewStack[viewStack.length - 1];

  const push = useCallback((view: View) => {
    setViewStack((prev) => [...prev, view]);
  }, []);

  const pop = useCallback(() => {
    setViewStack((prev) => (prev.length > 1 ? prev.slice(0, -1) : prev));
  }, []);

  // Global Esc handler
  useEffect(() => {
    const handle = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        pop();
      }
    };
    window.addEventListener("keydown", handle);
    return () => window.removeEventListener("keydown", handle);
  }, [pop]);

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

  // ─── Render helpers ─────────────────────────────────────────────────────────

  const goBackItem: TerminalMenuItem = {
    id: "__back__",
    label: "← Go back",
    action: pop,
    type: "system",
  };

  const renderRootMenu = () => {
    const items: TerminalMenuItem[] = [
      {
        id: "groups",
        label: "Groups",
        description: groupsLoading
          ? "Loading…"
          : groupsWithChannels
            ? `${groupsWithChannels.length} group${groupsWithChannels.length !== 1 ? "s" : ""}`
            : "No groups yet",
        action: () => push({ type: "groups" }),
        testId: "menu-item-groups",
      },
      {
        id: "dms",
        label: "Direct Messages",
        description: dmConversations.length > 0
          ? `${dmConversations.length} conversation${dmConversations.length !== 1 ? "s" : ""}`
          : "Start a new conversation",
        action: () => push({ type: "dms" }),
        testId: "menu-item-dms",
      },
      {
        id: "invites",
        label: "Invites",
        description: pendingInvites.length > 0
          ? `${pendingInvites.length} pending`
          : "No pending invites",
        action: () => push({ type: "invites" }),
        type: "system" as const,
        testId: "menu-item-invites",
      },
      // Search disabled — unreliable, needs more work
      // {
      //   id: "search",
      //   label: "Search",
      //   description: "Search your message history",
      //   action: () => push({ type: "search" }),
      //   type: "system" as const,
      //   testId: "menu-item-search",
      // },
      { id: "__sep1__", label: "", type: "separator" },
      {
        id: "preferences",
        label: "Preferences",
        description: "Colors, font size",
        action: () => push({ type: "preferences" }),
        type: "system",
        testId: "menu-item-preferences",
      },
      {
        id: "settings",
        label: "Settings",
        description: currentUser ? currentUser.email : undefined,
        action: () => push({ type: "settings" }),
        type: "system",
        testId: "menu-item-settings",
      },
    ];

    return <TerminalMenu items={items} />
;
  };

  const renderGroupsMenu = () => {
    const groups = groupsWithChannels ?? [];

    const groupItems: TerminalMenuItem[] = groupsLoading
      ? [{ id: "__loading__", label: "Loading…", disabled: true }]
      : groupsError
        ? [{ id: "__error__", label: `Error: ${groupsError instanceof Error ? groupsError.message : "Failed to load"}`, disabled: true }]
        : groups.map((g) => ({
            id: g.id,
            label: g.name,
            description: g.description || undefined,
            action: () => {
              setSelectedGroupId(g.id);
              push({ type: "group" as const, group: g });
            },
            testId: `group-option-${g.id}`,
          }));

    const items: TerminalMenuItem[] = [
      ...groupItems,
      { id: "__sep__", label: "", type: "separator" },
      {
        id: "create-group",
        label: "Create Group",
        action: () => push({ type: "create-group" }),
        type: "system",
        testId: "menu-item-create-group",
      },
      {
        id: "search-group",
        label: "Find Group",
        action: () => push({ type: "search-group" }),
        type: "system",
        testId: "menu-item-find-group",
      },
      goBackItem,
    ];

    return <TerminalMenu items={items} onEsc={pop} />
;
  };

  const renderGroupMenu = (group: GroupWithChannels) => {
    const channels = group.channels ?? [];
    const items: TerminalMenuItem[] = [
      ...channels.map((ch) => ({
        id: ch.id,
        // Voice channels get a [v] prefix; text channels get #
        label: ch.channel_type === "voice" ? `[v] ${ch.name}` : `# ${ch.name}`,
        description: ch.description || undefined,
        action: () => {
          if (ch.channel_type === "voice") {
            setActiveVoiceChannelId(ch.id);
            push({ type: "voice-channel" as const, channelName: ch.name });
          } else {
            setSelectedChannelId(ch.id);
            markRead(ch.id);
            push({ type: "channel" as const });
          }
        },
        badge: ch.channel_type === "voice" ? 0 : (unreadCounts[ch.id] ?? 0),
        testId: `channel-option-${ch.id}`,
      })),
      { id: "__sep__", label: "", type: "separator" as const },
      {
        id: "create-channel",
        label: "+ New Channel",
        action: () => {
          setSelectedGroupId(group.id);
          push({ type: "create-channel" });
        },
        type: "system" as const,
        testId: "menu-item-create-channel",
      },
      {
        id: "invite-member",
        label: "Invite Member",
        action: () => push({ type: "invite-member", group }),
        type: "system" as const,
        testId: "menu-item-invite-member",
      },
      {
        id: "join-requests",
        label: "Join Requests",
        action: () => push({ type: "join-requests", group }),
        type: "system" as const,
        testId: "menu-item-join-requests",
      },
      {
        id: "leave-group",
        label: "Leave Group",
        action: () => push({ type: "leave-group", group }),
        type: "system" as const,
        testId: "menu-item-leave-group",
      },
      goBackItem,
    ];

    return <TerminalMenu items={items} onEsc={pop} />
;
  };

  const renderDMsMenu = (conversations: DMConversation[]) => {
    const items: TerminalMenuItem[] = [
      ...conversations.map((c) => ({
        id: c.id,
        label: c.user2_identifier,
        action: () => {
          setSelectedConversationId(c.id);
          markRead(c.id);
          push({ type: "dm" as const });
        },
        badge: unreadCounts[c.id] ?? 0,
        testId: `dm-option-${c.id}`,
      })),
      { id: "__sep__", label: "", type: "separator" as const },
      {
        id: "new-dm",
        label: "New Message",
        action: () => push({ type: "start-dm" }),
        type: "system" as const,
        testId: "menu-item-new-dm",
      },
      goBackItem,
    ];

    return <TerminalMenu items={items} onEsc={pop} />
;
  };

  // ─── View title for TopBar display ──────────────────────────────────────────

  const viewTitle = (): string => {
    switch (currentView.type) {
      case "root": return "pollis";
      case "groups": return "Groups";
      case "group": return currentView.group.name;
      case "channel": {
        const groupChannels = selectedGroupId ? (channels[selectedGroupId] ?? []) : [];
        const ch = groupChannels.find((c) => c.id === selectedChannelId);
        return ch ? `Channel : : ${ch.name}` : "Channel";
      }
      case "dms": return "Direct Messages";
      case "dm": {
        const conv = dmConversations.find((c) => c.id === selectedConversationId);
        return conv ? `dm : : @${conv.user2_identifier}` : "dm";
      }
      case "create-group": return "Create Group";
      case "search-group": return "Find Group";
      case "create-channel": return "New Channel";
      case "start-dm": return "New Message";
      case "preferences": return "Preferences";
      case "settings": return "Settings";
      case "invites": return "Invites";
      case "join-requests": return `Join Requests : : ${currentView.group.name}`;
      case "invite-member": return `Invite Member : : ${currentView.group.name}`;
      case "leave-group": return `Leave Group : : ${currentView.group.name}`;
      case "search": return "Search";
      case "voice-channel": return `[v] ${currentView.channelName}`;
      default: return "pollis";
    }
  };

  // ─── Content area ───────────────────────────────────────────────────────────

  const renderContent = () => {
    switch (currentView.type) {
      case "root":
        return renderRootMenu();
      case "groups":
        return renderGroupsMenu();
      case "group":
        return renderGroupMenu(currentView.group);
      case "channel":
        return (
          <div className="flex flex-col h-full">
            <div
              className="flex items-center px-4 py-2 flex-shrink-0 text-xs font-mono"
              style={{
                borderBottom: "1px solid var(--c-border)",
                color: "var(--c-text-muted)",
              }}
            >
              <button
                onClick={pop}
                className="mr-3 inline-flex items-center gap-1 leading-none transition-colors"
                style={{ color: "var(--c-text-muted)" }}
                onMouseEnter={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-accent)"; }}
                onMouseLeave={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-text-muted)"; }}
              >
                <ArrowLeft size={12} />
              </button>
              <span>{viewTitle()}</span>
            </div>
            <div className="flex-1 overflow-hidden flex flex-col min-h-0">
              <MainContent />
            </div>
          </div>
        );
      case "dms":
        return renderDMsMenu(dmConversations);
      case "dm":
        return (
          <div className="flex flex-col h-full">
            <div
              className="flex items-center px-4 py-2 flex-shrink-0 text-xs font-mono"
              style={{
                borderBottom: "1px solid var(--c-border)",
                color: "var(--c-text-muted)",
              }}
            >
              <button
                onClick={pop}
                className="mr-3 inline-flex items-center gap-1 leading-none transition-colors"
                style={{ color: "var(--c-text-muted)" }}
                onMouseEnter={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-accent)"; }}
                onMouseLeave={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-text-muted)"; }}
              >
                <ArrowLeft size={12} />
              </button>
              <span>Direct Message{(() => { const conv = dmConversations.find((c) => c.id === selectedConversationId); return conv ? ` : : @${conv.user2_identifier}` : ""; })()}</span>
            </div>
            <div className="flex-1 overflow-hidden flex flex-col min-h-0">
              <MainContent />
            </div>
          </div>
        );
      case "create-group":
        return (
          <div className="flex flex-col h-full">
            <MenuPageHeader title="Create Group" onBack={pop} />
            <div className="flex-1 overflow-hidden">
              <CreateGroup onSuccess={() => pop()} />
            </div>
          </div>
        );
      case "search-group":
        return (
          <div className="flex flex-col h-full">
            <MenuPageHeader title="Find Group" onBack={pop} />
            <div className="flex-1 overflow-hidden">
              <SearchGroup />
            </div>
          </div>
        );
      case "create-channel":
        return (
          <div className="flex flex-col h-full">
            <MenuPageHeader title="New Channel" onBack={pop} />
            <div className="flex-1 overflow-hidden">
              <CreateChannel onSuccess={(channelId) => {
                if (channelId) {
                  // Channel was created — pop back to the group and open the channel
                  pop();
                  push({ type: "channel" });
                } else {
                  pop();
                }
              }} />
            </div>
          </div>
        );
      case "start-dm":
        return (
          <div className="flex flex-col h-full">
            <MenuPageHeader title="New Message" onBack={pop} />
            <div className="flex-1 overflow-hidden">
              <StartDM onSuccess={(conversationId) => {
                setSelectedConversationId(conversationId);
                // Replace the start-dm view with the conversation
                pop();
                push({ type: "dm" });
              }} />
            </div>
          </div>
        );
      case "preferences":
        return (
          <div className="flex flex-col h-full">
            <MenuPageHeader title="Preferences" onBack={pop} />
            <div className="flex-1 overflow-auto">
              <Preferences />
            </div>
          </div>
        );
      case "settings":
        return (
          <div className="flex flex-col h-full">
            <MenuPageHeader title="Settings" onBack={pop} />
            <div className="flex-1 overflow-auto">
              <Settings onDeleteAccount={onDeleteAccount} />
            </div>
          </div>
        );
      case "invites":
        return (
          <div className="flex flex-col h-full">
            <MenuPageHeader title="Invites" onBack={pop} />
            <div className="flex-1 overflow-hidden">
              <Invites />
            </div>
          </div>
        );
      case "join-requests":
        return (
          <div className="flex flex-col h-full">
            <MenuPageHeader title="Join Requests" onBack={pop} />
            <div className="flex-1 overflow-hidden">
              <JoinRequests groupId={currentView.group.id} groupName={currentView.group.name} />
            </div>
          </div>
        );
      case "invite-member":
        return (
          <div className="flex flex-col h-full">
            <MenuPageHeader title="Invite Member" onBack={pop} />
            <div className="flex-1 overflow-hidden">
              <InviteMember groupId={currentView.group.id} groupName={currentView.group.name} />
            </div>
          </div>
        );
      case "leave-group":
        return (
          <div className="flex flex-col h-full">
            <MenuPageHeader title="Leave Group" onBack={pop} />
            <div className="flex-1 flex flex-col items-center justify-center gap-4 px-6">
              <p className="text-xs font-mono text-center" style={{ color: "var(--c-text-dim)" }}>
                Are you sure you want to leave <strong>{currentView.group.name}</strong>?
                <br />
                You will need a new invite to rejoin.
              </p>
              {leaveGroupMutation.isError && (
                <p className="text-xs font-mono" style={{ color: "#ff6b6b" }}>
                  {leaveGroupMutation.error instanceof Error ? leaveGroupMutation.error.message : "Failed to leave group"}
                </p>
              )}
              <div className="flex gap-3">
                <button
                  data-testid="leave-group-confirm"
                  onClick={async () => {
                    try {
                      await leaveGroupMutation.mutateAsync(currentView.group.id);
                      setSelectedGroupId(null);
                      setSelectedChannelId(null);
                      setViewStack([{ type: "root" }]);
                    } catch {
                      // error shown via isError above
                    }
                  }}
                  disabled={leaveGroupMutation.isPending}
                  className="px-4 py-2 text-xs font-mono font-medium transition-colors"
                  style={{
                    background: "var(--c-accent)",
                    color: "var(--c-bg)",
                    border: "1px solid var(--c-border-active)",
                    borderRadius: 4,
                    opacity: leaveGroupMutation.isPending ? 0.5 : 1,
                    cursor: leaveGroupMutation.isPending ? "not-allowed" : "pointer",
                  }}
                >
                  {leaveGroupMutation.isPending ? "Leaving…" : "Yes, Leave"}
                </button>
                <button
                  data-testid="leave-group-cancel"
                  onClick={pop}
                  className="px-4 py-2 text-xs font-mono font-medium transition-colors"
                  style={{
                    background: "transparent",
                    color: "var(--c-accent)",
                    border: "1px solid var(--c-border-active)",
                    borderRadius: 4,
                    cursor: "pointer",
                  }}
                >
                  Cancel
                </button>
              </div>
            </div>
          </div>
        );
      case "search":
        return (
          <div className="flex flex-col h-full">
            <MenuPageHeader title="Search" onBack={pop} />
            <div className="flex-1 overflow-hidden">
              <SearchView
                onNavigateToConversation={(conversationId) => {
                  setSelectedConversationId(conversationId);
                  // Navigate back, then open the DM conversation
                  pop();
                  push({ type: "dm" });
                }}
              />
            </div>
          </div>
        );
      case "voice-channel":
        return (
          <div className="flex flex-col h-full">
            <MenuPageHeader title={`[v] ${currentView.channelName}`} onBack={pop} />
            <div className="flex-1 overflow-hidden">
              <VoiceChannelView
                channelId={activeVoiceChannelId}
                channelName={currentView.channelName}
              />
            </div>
          </div>
        );
      default:
        return renderRootMenu();
    }
  };

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

      {/* Main content */}
      <div style={{ flex: 1, overflow: "hidden", display: "flex", flexDirection: "column" }}>
        {renderContent()}
      </div>

      {/* VoiceBar — shown above bottom bar while user is in a voice channel */}
      {activeVoiceChannelId !== null && (() => {
        const voiceView = viewStack.find((v) => v.type === "voice-channel") as { type: "voice-channel"; channelName: string } | undefined;
        const channelName = voiceView?.channelName ?? "voice";
        return (
          <VoiceBar
            channelId={activeVoiceChannelId}
            channelName={channelName}
          />
        );
      })()}

      {/* Bottom bar — reserved for notifications/shortcuts/status */}
      <div
        style={{
          height: 28,
          flexShrink: 0,
          borderTop: "1px solid var(--c-border)",
          background: "var(--c-surface)",
          display: "flex",
          alignItems: "center",
          paddingLeft: 12,
          paddingRight: 12,
        }}
      >
        <span className="text-xs font-mono" style={{ color: "var(--c-text-dim)" }}>
          {viewStack
            .filter((v) => v.type !== "root")
            .map((v) => {
              switch (v.type) {
                case "groups": return "Groups";
                case "group": return v.group.name;
                case "channel": {
                  const groupChannels = selectedGroupId ? (channels[selectedGroupId] ?? []) : [];
                  const ch = groupChannels.find((c) => c.id === selectedChannelId);
                  return ch ? `Channel : : ${ch.name}` : "Channel";
                }
                case "dms": return "Direct Messages";
                case "dm": {
                  const conv = dmConversations.find((c) => c.id === selectedConversationId);
                  return conv ? `dm : : @${conv.user2_identifier}` : "dm";
                }
                case "create-group": return "Create Group";
                case "search-group": return "Find Group";
                case "create-channel": return "New Channel";
                case "start-dm": return "New Message";
                case "preferences": return "Preferences";
                case "settings": return "Settings";
                case "invites": return "Invites";
                case "join-requests": return `Join Requests : : ${v.group.name}`;
                case "invite-member": return `Invite Member : : ${v.group.name}`;
                case "leave-group": return `Leave Group : : ${v.group.name}`;
                case "search": return "Search";
                default: return null;
              }
            })
            .filter(Boolean)
            .join(" › ")}
        </span>
      </div>
    </div>
  );
};

// ─── Small reusable page header inside terminal views ─────────────────────────

interface MenuPageHeaderProps {
  title: string;
  onBack: () => void;
  rightAction?: React.ReactNode;
}

const MenuPageHeader: React.FC<MenuPageHeaderProps> = ({ title, onBack, rightAction }) => (
  <div
    className="flex items-center px-4 py-2 flex-shrink-0 text-xs font-mono"
    style={{
      borderBottom: "1px solid var(--c-border)",
      color: "var(--c-text-muted)",
    }}
  >
    <button
      onClick={onBack}
      className="mr-3 inline-flex items-center gap-1 leading-none transition-colors"
      style={{ color: "var(--c-text-muted)" }}
      onMouseEnter={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-accent)"; }}
      onMouseLeave={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-text-muted)"; }}
    >
      <ArrowLeft size={12} />
    </button>
    <span style={{ flex: 1, color: "var(--c-text)" }}>{title}</span>
    {rightAction}
  </div>
);
