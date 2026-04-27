import React, { useState, useEffect, useRef, useMemo, useCallback } from "react";
import { useNavigate } from "@tanstack/react-router";
import { Hash, AtSign, Search, ArrowUp, ArrowDown, Volume2, Settings as SettingsIcon } from "lucide-react";
import { Avatar } from "./ui/Avatar";
import { useUserGroupsWithChannels, useAllGroupMembers, type GroupMemberWithGroup } from "../hooks/queries/useGroups";
import { useDMConversations } from "../hooks/queries/useMessages";
import { useAppStore } from "../stores/appStore";
import type { GroupWithChannels } from "../services/api";
import { warmVoiceChannel } from "../utils/voiceWarmup";

// ─── Types ───────────────────────────────────────────────────────────────────

type SearchResultItem =
  | {
      type: "channel";
      id: string;
      name: string;
      breadcrumb: string;
      groupId: string;
      channelId: string;
    }
  | {
      type: "voice";
      id: string;
      name: string;
      breadcrumb: string;
      groupId: string;
      channelId: string;
    }
  | {
      type: "dm";
      id: string;
      name: string;
      breadcrumb: string;
      conversationId: string;
    }
  | {
      type: "user";
      id: string;
      name: string;
      breadcrumb: string;
      userId: string;
      avatarKey?: string | null;
    }
  | {
      type: "page";
      id: string;
      name: string;
      breadcrumb: string;
      path: string;
      keywords: string;
    };

// ─── Props ───────────────────────────────────────────────────────────────────

interface SearchPanelProps {
  isOpen: boolean;
  onClose: () => void;
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

function buildChannelResults(
  groups: GroupWithChannels[] | undefined
): SearchResultItem[] {
  if (!groups) {
    return [];
  }
  const results: SearchResultItem[] = [];
  for (const group of groups) {
    const groupSlug = group.slug || group.name.toLowerCase().replace(/\s+/g, "-");
    for (const channel of group.channels) {
      if (channel.channel_type === "voice") {
        continue;
      }
      const channelSlug = channel.slug || channel.name.toLowerCase().replace(/\s+/g, "-");
      results.push({
        type: "channel",
        id: `channel-${channel.id}`,
        name: channel.name,
        breadcrumb: `/g/${groupSlug}/${channelSlug}`,
        groupId: group.id,
        channelId: channel.id,
      });
    }
  }
  return results;
}

function buildVoiceResults(
  groups: GroupWithChannels[] | undefined
): SearchResultItem[] {
  if (!groups) {
    return [];
  }
  const results: SearchResultItem[] = [];
  for (const group of groups) {
    const groupSlug = group.slug || group.name.toLowerCase().replace(/\s+/g, "-");
    for (const channel of group.channels) {
      if (channel.channel_type !== "voice") {
        continue;
      }
      const channelSlug = channel.slug || channel.name.toLowerCase().replace(/\s+/g, "-");
      results.push({
        type: "voice",
        id: `voice-${channel.id}`,
        name: channel.name,
        breadcrumb: `/g/${groupSlug}/voice/${channelSlug}`,
        groupId: group.id,
        channelId: channel.id,
      });
    }
  }
  return results;
}

function buildDMResults(
  conversations: Array<{ id: string; user2_identifier: string }> | undefined
): SearchResultItem[] {
  if (!conversations) {
    return [];
  }
  return conversations.map((conv) => ({
    type: "dm" as const,
    id: `dm-${conv.id}`,
    name: conv.user2_identifier,
    breadcrumb: `/dm/${conv.user2_identifier}`,
    conversationId: conv.id,
  }));
}

function buildUserResults(
  groupMembers: GroupMemberWithGroup[],
  dmConversations: Array<{ user2_id?: string; user2_identifier: string; user2_avatar_url?: string }> | undefined,
  currentUserId: string | null,
): SearchResultItem[] {
  const seen = new Set<string>();
  const out: SearchResultItem[] = [];

  for (const m of groupMembers) {
    if (!m.user_id || m.user_id === currentUserId || seen.has(m.user_id)) {
      continue;
    }
    seen.add(m.user_id);
    const username = m.username || m.display_name || m.user_id;
    out.push({
      type: "user",
      id: `user-${m.user_id}`,
      name: `@${username}`,
      breadcrumb: `/user/${username}`,
      userId: m.user_id,
      avatarKey: m.avatar_url ?? null,
    });
  }

  if (dmConversations) {
    for (const c of dmConversations) {
      const userId = c.user2_id;
      if (!userId || userId === currentUserId || seen.has(userId)) {
        continue;
      }
      seen.add(userId);
      out.push({
        type: "user",
        id: `user-${userId}`,
        name: `@${c.user2_identifier}`,
        breadcrumb: `/user/${c.user2_identifier}`,
        userId,
        avatarKey: c.user2_avatar_url ?? null,
      });
    }
  }

  return out;
}

const PAGE_RESULTS: SearchResultItem[] = [
  { type: "page", id: "page-settings", name: "User", breadcrumb: "/user", path: "/user", keywords: "account profile username email avatar settings" },
  { type: "page", id: "page-settings-hub", name: "Settings", breadcrumb: "/settings", path: "/settings", keywords: "preferences user security" },
  { type: "page", id: "page-preferences", name: "Preferences", breadcrumb: "/preferences", path: "/preferences", keywords: "theme color font notifications appearance" },
  { type: "page", id: "page-voice-settings", name: "Voice Settings", breadcrumb: "/settings/voice", path: "/voice-settings", keywords: "microphone speaker audio mic noise suppression echo cancellation agc auto join" },
  { type: "page", id: "page-security", name: "Security", breadcrumb: "/security", path: "/security", keywords: "audit log devices identity key rotation" },
  { type: "page", id: "page-invites", name: "Invites", breadcrumb: "/invites", path: "/invites", keywords: "pending invitations groups" },
  { type: "page", id: "page-join-requests", name: "Join Requests", breadcrumb: "/join-requests", path: "/join-requests", keywords: "pending group membership" },
  { type: "page", id: "page-dm-requests", name: "DM Requests", breadcrumb: "/dms/requests", path: "/dms/requests", keywords: "direct message pending" },
  { type: "page", id: "page-dm-blocked", name: "Blocked Users", breadcrumb: "/dms/blocked", path: "/dms/blocked", keywords: "block list direct message" },
  { type: "page", id: "page-dm-new", name: "Start DM", breadcrumb: "/dms/new", path: "/dms/new", keywords: "new direct message create" },
  { type: "page", id: "page-groups", name: "Groups", breadcrumb: "/groups", path: "/groups", keywords: "list all" },
  { type: "page", id: "page-groups-new", name: "Create Group", breadcrumb: "/groups/new", path: "/groups/new", keywords: "new create" },
  { type: "page", id: "page-groups-search", name: "Find Groups", breadcrumb: "/groups/search", path: "/groups/search", keywords: "discover search public" },
];

function filterResults(
  items: SearchResultItem[],
  query: string
): SearchResultItem[] {
  const trimmed = query.trim().toLowerCase();
  if (!trimmed) {
    return items;
  }
  return items.filter((item) => {
    const nameMatch = item.name.toLowerCase().includes(trimmed);
    const breadcrumbMatch = item.breadcrumb.toLowerCase().includes(trimmed);
    const keywordMatch = item.type === "page" && item.keywords.toLowerCase().includes(trimmed);
    return nameMatch || breadcrumbMatch || keywordMatch;
  });
}

// ─── SearchPanel ─────────────────────────────────────────────────────────────

export const SearchPanel: React.FC<SearchPanelProps> = ({ isOpen, onClose }) => {
  const [query, setQuery] = useState("");
  const [selectedIndex, setSelectedIndex] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const resultsRef = useRef<HTMLDivElement>(null);
  const itemRefs = useRef<(HTMLDivElement | null)[]>([]);
  const navigate = useNavigate();

  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const { data: dmConversations } = useDMConversations();
  const { members: allGroupMembers } = useAllGroupMembers();
  const activeVoiceChannelId = useAppStore((s) => s.activeVoiceChannelId);
  const currentUserId = useAppStore((s) => s.currentUser?.id ?? null);

  // Build the full list of searchable items, active voice channel sorted to top
  const allItems = useMemo(() => {
    const channels = buildChannelResults(groupsWithChannels);
    const voiceChannels = buildVoiceResults(groupsWithChannels);
    const dms = buildDMResults(dmConversations);
    const users = buildUserResults(allGroupMembers, dmConversations, currentUserId);
    const combined: SearchResultItem[] = [...channels, ...voiceChannels, ...dms, ...users, ...PAGE_RESULTS];
    if (activeVoiceChannelId) {
      const activeIdx = combined.findIndex(
        (i) => i.type === "voice" && i.channelId === activeVoiceChannelId
      );
      if (activeIdx > 0) {
        const [active] = combined.splice(activeIdx, 1);
        combined.unshift(active);
      }
    }
    return combined;
  }, [groupsWithChannels, dmConversations, allGroupMembers, activeVoiceChannelId, currentUserId]);

  // Filter based on query
  const filteredItems = useMemo(
    () => filterResults(allItems, query),
    [allItems, query]
  );

  // Reset selection when results change
  useEffect(() => {
    setSelectedIndex(0);
  }, [filteredItems.length, query]);

  // Focus input when panel opens, reset state
  useEffect(() => {
    if (isOpen) {
      setQuery("");
      setSelectedIndex(0);
      // Small delay to ensure the DOM is rendered
      requestAnimationFrame(() => {
        inputRef.current?.focus();
      });
    }
  }, [isOpen]);

  // Scroll selected item into view
  useEffect(() => {
    itemRefs.current[selectedIndex]?.scrollIntoView({
      behavior: "smooth",
      block: "nearest",
    });
  }, [selectedIndex]);

  // Issue #176: when the highlighted result is a voice channel, warm the
  // LiveKit connection so pressing Enter feels instant.
  useEffect(() => {
    const item = filteredItems[selectedIndex];
    if (item && item.type === "voice") {
      warmVoiceChannel(item.channelId);
    }
  }, [selectedIndex, filteredItems]);

  const handleSelect = useCallback(
    (item: SearchResultItem) => {
      onClose();
      if (item.type === "channel") {
        navigate({
          to: "/groups/$groupId/channels/$channelId",
          params: { groupId: item.groupId, channelId: item.channelId },
        });
      } else if (item.type === "voice") {
        navigate({
          to: "/groups/$groupId/voice/$channelId",
          params: { groupId: item.groupId, channelId: item.channelId },
        });
      } else if (item.type === "dm") {
        navigate({
          to: "/dms/$conversationId",
          params: { conversationId: item.conversationId },
        });
      } else if (item.type === "user") {
        navigate({
          to: "/user/$userId",
          params: { userId: item.userId },
        });
      } else {
        navigate({ to: item.path });
      }
    },
    [onClose, navigate]
  );

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      switch (e.key) {
        case "ArrowDown": {
          e.preventDefault();
          setSelectedIndex((prev) =>
            prev < filteredItems.length - 1 ? prev + 1 : 0
          );
          break;
        }
        case "ArrowUp": {
          e.preventDefault();
          setSelectedIndex((prev) =>
            prev > 0 ? prev - 1 : filteredItems.length - 1
          );
          break;
        }
        case "Enter": {
          e.preventDefault();
          const item = filteredItems[selectedIndex];
          if (item) {
            handleSelect(item);
          }
          break;
        }
        case "Escape": {
          e.preventDefault();
          e.stopPropagation();
          onClose();
          break;
        }
        case "Tab": {
          e.preventDefault();
          if (e.shiftKey) {
            setSelectedIndex((prev) =>
              prev > 0 ? prev - 1 : filteredItems.length - 1
            );
          } else {
            setSelectedIndex((prev) =>
              prev < filteredItems.length - 1 ? prev + 1 : 0
            );
          }
          break;
        }
      }
    },
    [filteredItems, selectedIndex, handleSelect, onClose]
  );

  if (!isOpen) {
    return null;
  }

  return (
    <div
      data-testid="search-panel-overlay"
      style={{
        position: "fixed",
        inset: 0,
        zIndex: 9999,
        background: "rgba(0, 0, 0, 0.80)",
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        paddingTop: "10vh",
        borderRadius: "10px",
        overflow: "hidden",
      }}
      onClick={(e) => {
        // Close when clicking the backdrop
        if (e.target === e.currentTarget) {
          onClose();
        }
      }}
    >
      <div
        data-testid="search-panel"
        style={{
          width: "100%",
          maxWidth: 560,
          maxHeight: "70vh",
          display: "flex",
          flexDirection: "column",
          background: "var(--c-surface)",
          border: "1px solid var(--c-border)",
          borderRadius: "0.75rem",
          overflow: "hidden",
          boxShadow: "0 25px 50px -12px rgba(0, 0, 0, 0.5)",
        }}
        onKeyDown={handleKeyDown}
      >
        {/* Search input */}
        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: "0.75rem",
            padding: "0.875rem 1rem",
            borderBottom: "1px solid var(--c-border)",
          }}
        >
          <Search
            className="flex-shrink-0"
            size={16}
            style={{ color: "var(--c-text-muted)" }}
          />
          <input
            ref={inputRef}
            data-testid="search-panel-input"
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Jump to channel, voice, DM, user, or settings..."
            autoComplete="off"
            autoCorrect="off"
            autoCapitalize="off"
            spellCheck={false}
            className="flex-1 font-mono text-sm"
            style={{
              background: "transparent",
              color: "var(--c-text)",
              border: "none",
              outline: "none",
            }}
          />
          <kbd
            className="font-mono text-xs"
            style={{
              color: "var(--c-text-muted)",
              background: "var(--c-bg)",
              padding: "2px 6px",
              borderRadius: "4px",
              border: "1px solid var(--c-border)",
            }}
          >
            esc
          </kbd>
        </div>

        {/* Keyboard hints */}
        <div
          className="flex items-center gap-1 px-4 py-1.5 text-xs font-mono flex-shrink-0"
          style={{
            borderBottom: "1px solid var(--c-border)",
            color: "var(--c-text-muted)",
          }}
        >
          <ArrowUp className="w-3 h-3" />
          <ArrowDown className="w-3 h-3" />
          <span>navigate</span>
          <span className="mx-1" style={{ color: "var(--c-border-active)" }}>
            &bull;
          </span>
          <span>Enter to select</span>
          <span className="mx-1" style={{ color: "var(--c-border-active)" }}>
            &bull;
          </span>
          <span>Esc to close</span>
        </div>

        {/* Results */}
        <div
          ref={resultsRef}
          data-testid="search-panel-results"
          className="overflow-y-auto"
          style={{ flex: 1 }}
        >
          {filteredItems.length === 0 ? (
            <div
              data-testid="search-panel-empty"
              className="text-center py-8 font-mono text-xs"
              style={{ color: "var(--c-text-muted)" }}
            >
              {query.trim()
                ? "No matches found"
                : "Jump to a channel, voice channel, DM, user, or settings page"}
            </div>
          ) : (
            filteredItems.map((item, index) => {
              const isSelected = index === selectedIndex;
              return (
                <div
                  key={item.id}
                  ref={(el) => {
                    itemRefs.current[index] = el;
                  }}
                  data-testid="search-panel-result-item"
                  className="flex items-center gap-3 px-4 py-2.5 cursor-pointer transition-colors"
                  style={{
                    borderLeft: `3px solid ${isSelected ? "var(--c-accent)" : "transparent"}`,
                    background: isSelected ? "var(--c-active)" : undefined,
                  }}
                  onClick={() => handleSelect(item)}
                  onMouseEnter={() => setSelectedIndex(index)}
                >
                  {/* Icon */}
                  <div
                    className="flex-shrink-0 w-5 h-5 flex items-center justify-center"
                    style={{
                      color: isSelected ? "var(--c-accent)" : "var(--c-text-dim)",
                    }}
                  >
                    {item.type === "channel" ? (
                      <Hash size={14} />
                    ) : item.type === "voice" ? (
                      <Volume2 size={14} />
                    ) : item.type === "dm" ? (
                      <AtSign size={14} />
                    ) : item.type === "user" ? (
                      <Avatar avatarKey={item.avatarKey ?? null} size={20} alt={item.name} />
                    ) : (
                      <SettingsIcon size={14} />
                    )}
                  </div>

                  {/* Name and breadcrumb */}
                  <div className="flex-1 min-w-0">
                    <div
                      className="font-sans text-sm truncate flex items-center gap-2"
                      style={{
                        color: isSelected ? "var(--c-accent)" : "var(--c-text)",
                      }}
                    >
                      <span>{item.name}</span>
                      {item.type === "voice" && item.channelId === activeVoiceChannelId && (
                        <span className="font-mono text-xs" style={{ color: "var(--c-accent)" }}>[live]</span>
                      )}
                    </div>
                    <div
                      className="font-mono text-xs truncate"
                      style={{ color: "var(--c-text-muted)" }}
                    >
                      {item.breadcrumb}
                    </div>
                  </div>
                </div>
              );
            })
          )}
        </div>
      </div>
    </div>
  );
};
