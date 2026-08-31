import React, { useState, useEffect, useRef, useMemo, useCallback } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "@tanstack/react-router";
import { Hash, Search, ArrowUp, ArrowDown, Volume2, Settings as SettingsIcon, Link as LinkIcon } from "lucide-react";
import { PresenceAvatar } from "./ui/PresenceAvatar";
import { useUserGroupsWithChannels, useAllGroupMembers, type GroupMemberWithGroup } from "../hooks/queries/useGroups";
import { useDMConversations } from "../hooks/queries/useMessages";
import { observer } from "mobx-react-lite";
import { appStore } from "../stores/appStore";
import type { GroupWithChannels } from "../services/api";
import { warmVoiceChannel } from "../utils/voiceWarmup";
import { parseMessagePermalink } from "../utils/urlRouting";
import { useMessagePermalink } from "../hooks/useMessagePermalink";
import { useGlobalShortcut, useShortcutLabel } from "../keyboard";

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
      // A "person" entry: clicking opens the existing DM if one exists,
      // otherwise the user's profile page (where they can request a new DM).
      // Replaces the previous separate user / dm entry types so the same
      // person never appears twice in the menu.
      type: "person";
      id: string;
      name: string;
      breadcrumb: string;
      userId: string;
      avatarKey?: string | null;
      // Set when the current user already has an accepted DM with this person.
      conversationId?: string;
    }
  | {
      type: "page";
      id: string;
      name: string;
      breadcrumb: string;
      path: string;
      keywords: string;
    }
  | {
      // "Search messages for …" — the handoff to the full-text search page
      // (#850). Cmd+K stays a NAVIGATOR: it jumps to places you already know
      // the name of, instantly and locally. Message hits are a different
      // question with a different answer shape — ranked, snippeted, filterable,
      // paginated — and rendering them inline would make every keystroke run a
      // real query and turn one list into two competing ones.
      type: "messages";
      id: string;
      name: string;
      breadcrumb: string;
      query: string;
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

/**
 * Build the unified "person" list: every reachable user (group members
 * the current user shares a group with, plus existing DM partners) gets
 * one entry. If a DM already exists, `conversationId` is filled in so
 * clicking jumps straight there; otherwise clicking goes to the profile
 * page, which is where new DMs are initiated.
 */
function buildPersonResults(
  groupMembers: GroupMemberWithGroup[],
  dmConversations: Array<{ id: string; user2_id?: string; user2_identifier: string; user2_avatar_url?: string }> | undefined,
  currentUserId: string | null,
): SearchResultItem[] {
  // user_id → existing DM conversation_id, so we can attach it to any
  // matching group-member row and avoid emitting a duplicate person.
  const dmByUserId = new Map<string, { conversationId: string; identifier: string; avatarKey?: string | null }>();
  if (dmConversations) {
    for (const c of dmConversations) {
      if (c.user2_id) {
        dmByUserId.set(c.user2_id, {
          conversationId: c.id,
          identifier: c.user2_identifier,
          avatarKey: c.user2_avatar_url ?? null,
        });
      }
    }
  }

  const seen = new Set<string>();
  const out: SearchResultItem[] = [];

  for (const m of groupMembers) {
    if (!m.user_id || m.user_id === currentUserId || seen.has(m.user_id)) {
      continue;
    }
    seen.add(m.user_id);
    const dm = dmByUserId.get(m.user_id);
    const username = m.username || m.display_name || dm?.identifier || m.user_id;
    out.push({
      type: "person",
      id: `person-${m.user_id}`,
      name: `@${username}`,
      breadcrumb: dm ? `/dm/${username}` : `/user/${username}`,
      userId: m.user_id,
      avatarKey: m.avatar_url ?? dm?.avatarKey ?? null,
      conversationId: dm?.conversationId,
    });
  }

  // DM partners with no shared group still need to surface — add them now.
  for (const [userId, dm] of dmByUserId) {
    if (userId === currentUserId || seen.has(userId)) {
      continue;
    }
    seen.add(userId);
    out.push({
      type: "person",
      id: `person-${userId}`,
      name: `@${dm.identifier}`,
      breadcrumb: `/dm/${dm.identifier}`,
      userId,
      avatarKey: dm.avatarKey,
      conversationId: dm.conversationId,
    });
  }

  return out;
}

/**
 * A jumpable static page, before its copy is resolved.
 *
 * `nameKey` and `keywordsKey` are translation keys, not copy: this table is
 * module-level, so it cannot call `t()` itself without freezing the language at
 * import time. The panel resolves both at render time, which is also what keeps
 * matching honest — the query is compared against the label the user can
 * actually see, in whatever language they are reading.
 */
interface PageResultSpec {
  id: string;
  nameKey: string;
  keywordsKey: string;
  breadcrumb: string;
  path: string;
}

const PAGE_RESULTS: PageResultSpec[] = [
  { id: "page-settings", nameKey: "pages.userName", keywordsKey: "pages.userKeywords", breadcrumb: "/user", path: "/user" },
  { id: "page-settings-hub", nameKey: "pages.settingsName", keywordsKey: "pages.settingsKeywords", breadcrumb: "/settings", path: "/settings" },
  { id: "page-preferences", nameKey: "pages.preferencesName", keywordsKey: "pages.preferencesKeywords", breadcrumb: "/preferences", path: "/preferences" },
  { id: "page-voice-settings", nameKey: "pages.voiceName", keywordsKey: "pages.voiceKeywords", breadcrumb: "/settings/voice", path: "/voice-settings" },
  { id: "page-security", nameKey: "pages.securityName", keywordsKey: "pages.securityKeywords", breadcrumb: "/security", path: "/security" },
  { id: "page-shortcuts", nameKey: "pages.shortcutsName", keywordsKey: "pages.shortcutsKeywords", breadcrumb: "/shortcuts", path: "/shortcuts" },
  { id: "page-update", nameKey: "pages.updateName", keywordsKey: "pages.updateKeywords", breadcrumb: "/update", path: "/update" },
  { id: "page-invites", nameKey: "pages.invitesName", keywordsKey: "pages.invitesKeywords", breadcrumb: "/invites", path: "/invites" },
  { id: "page-join", nameKey: "pages.joinName", keywordsKey: "pages.joinKeywords", breadcrumb: "/join", path: "/join" },
  { id: "page-join-requests", nameKey: "pages.joinRequestsName", keywordsKey: "pages.joinRequestsKeywords", breadcrumb: "/join-requests", path: "/join-requests" },
  { id: "page-dm-requests", nameKey: "pages.dmRequestsName", keywordsKey: "pages.dmRequestsKeywords", breadcrumb: "/dms/requests", path: "/dms/requests" },
  { id: "page-dm-blocked", nameKey: "pages.dmBlockedName", keywordsKey: "pages.dmBlockedKeywords", breadcrumb: "/dms/blocked", path: "/dms/blocked" },
  { id: "page-dm-new", nameKey: "pages.dmNewName", keywordsKey: "pages.dmNewKeywords", breadcrumb: "/dms/new", path: "/dms/new" },
  { id: "page-groups", nameKey: "pages.groupsName", keywordsKey: "pages.groupsKeywords", breadcrumb: "/groups", path: "/groups" },
  { id: "page-groups-new", nameKey: "pages.groupsNewName", keywordsKey: "pages.groupsNewKeywords", breadcrumb: "/groups/new", path: "/groups/new" },
  { id: "page-groups-search", nameKey: "pages.groupsSearchName", keywordsKey: "pages.groupsSearchKeywords", breadcrumb: "/groups/search", path: "/groups/search" },
  { id: "page-saved", nameKey: "pages.savedName", keywordsKey: "pages.savedKeywords", breadcrumb: "/saved", path: "/saved" },
  { id: "page-search", nameKey: "pages.searchName", keywordsKey: "pages.searchKeywords", breadcrumb: "/search", path: "/search" },
  { id: "page-arcade", nameKey: "pages.arcadeName", keywordsKey: "pages.arcadeKeywords", breadcrumb: "/arcade", path: "/arcade" },
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

export const SearchPanel: React.FC<SearchPanelProps> = observer(({ isOpen, onClose }) => {
  const { t } = useTranslation("search");
  const [query, setQuery] = useState("");
  const [selectedIndex, setSelectedIndex] = useState(0);
  // Pasting a message permalink (#854) takes over the panel: there is nothing
  // to fuzzy-match, the answer is either "jump there" or "you don't have it".
  const [permalinkStatus, setPermalinkStatus] = useState<
    "checking" | "found" | "missing"
  >("checking");
  const { openPermalink } = useMessagePermalink();
  const permalink = useMemo(() => parseMessagePermalink(query), [query]);
  const inputRef = useRef<HTMLInputElement>(null);
  const resultsRef = useRef<HTMLDivElement>(null);
  const itemRefs = useRef<(HTMLDivElement | null)[]>([]);
  const navigate = useNavigate();

  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const { data: dmConversations } = useDMConversations();
  // Gated on `isOpen` (#874): this fans out to one `get_group_members` query
  // PER GROUP, and the panel is mounted-but-closed for the whole session until
  // someone presses Cmd+K.
  const { members: allGroupMembers } = useAllGroupMembers(isOpen);
  const activeVoiceChannelId =
    appStore.voiceState.kind === 'idle' ? null : appStore.voiceState.channelId;
  const currentUserId = appStore.currentUser?.id ?? null;

  // Page copy resolved in the active language, so `filterResults` matches what
  // is on screen rather than the translation keys behind it.
  const pageResults = useMemo<SearchResultItem[]>(
    () =>
      PAGE_RESULTS.map((page) => ({
        type: "page" as const,
        id: page.id,
        name: t(page.nameKey),
        breadcrumb: page.breadcrumb,
        path: page.path,
        keywords: t(page.keywordsKey),
      })),
    [t]
  );

  // Build the full list of searchable items, active voice channel sorted to top
  const allItems = useMemo(() => {
    const channels = buildChannelResults(groupsWithChannels);
    const voiceChannels = buildVoiceResults(groupsWithChannels);
    const people = buildPersonResults(allGroupMembers, dmConversations, currentUserId);
    const combined: SearchResultItem[] = [...channels, ...voiceChannels, ...people, ...pageResults];
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
  }, [groupsWithChannels, dmConversations, allGroupMembers, activeVoiceChannelId, currentUserId, pageResults]);

  // Resolve a pasted permalink against the LOCAL database only. There is no
  // server lookup here and there must never be one — that endpoint is what
  // would turn a permalink from an inert pointer into a leak.
  useEffect(() => {
    if (!permalink) {
      return;
    }
    let cancelled = false;
    setPermalinkStatus("checking");
    void (async () => {
      const jumped = await openPermalink(
        permalink.conversationId,
        permalink.messageId,
      );
      if (cancelled) {
        return;
      }
      if (jumped) {
        setPermalinkStatus("found");
        onClose();
      } else {
        setPermalinkStatus("missing");
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [permalink, openPermalink, onClose]);

  // Filter based on query, with the message-search handoff appended.
  //
  // LAST, not first. Enter in this panel has always meant "go to the
  // best-matching place", and this row is not a place — it is the door to a
  // different question. Pinning it to the top would quietly redefine the
  // default action of the app's most-used shortcut. It carries the query
  // across, so `/search` opens already searching instead of asking the user to
  // type it twice.
  const filteredItems = useMemo(() => {
    const matches = filterResults(allItems, query);
    const trimmed = query.trim();
    if (trimmed.length < 2) {
      return matches;
    }
    const handoff: SearchResultItem = {
      type: "messages",
      id: "search-messages-handoff",
      name: t("panel.searchMessagesFor", { query: trimmed }),
      breadcrumb: "/search",
      query: trimmed,
    };
    return [...matches, handoff];
  }, [allItems, query, t]);

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
      } else if (item.type === "messages") {
        navigate({ to: "/search", search: { q: item.query } });
      } else if (item.type === "person") {
        if (item.conversationId) {
          navigate({
            to: "/dms/$conversationId",
            params: { conversationId: item.conversationId },
          });
        } else {
          navigate({
            to: "/user/$userId",
            params: { userId: item.userId },
          });
        }
      } else {
        navigate({ to: item.path });
      }
    },
    [onClose, navigate]
  );

  // Shared by the arrow keys, Tab, and the rebindable search.next /
  // search.prev commands, so every way of moving the selection wraps
  // identically.
  const selectNext = useCallback(() => {
    setSelectedIndex((prev) => (prev < filteredItems.length - 1 ? prev + 1 : 0));
  }, [filteredItems.length]);

  const selectPrev = useCallback(() => {
    setSelectedIndex((prev) => (prev > 0 ? prev - 1 : filteredItems.length - 1));
  }, [filteredItems.length]);

  // Registered above app.toggleSearch (priority 0) and live only while the
  // menu is open, so the default mod+k moves the selection here instead of
  // toggling the menu shut behind the user's back.
  useGlobalShortcut("search.next", selectNext, {
    enabled: isOpen,
    priority: 10,
  });
  useGlobalShortcut("search.prev", selectPrev, {
    enabled: isOpen,
    priority: 10,
  });

  const nextLabel = useShortcutLabel("search.next");
  const prevLabel = useShortcutLabel("search.prev");

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      switch (e.key) {
        case "ArrowDown": {
          e.preventDefault();
          selectNext();
          break;
        }
        case "ArrowUp": {
          e.preventDefault();
          selectPrev();
          break;
        }
        case "Enter": {
          e.preventDefault();
          // A pasted permalink resolves on its own; Enter must not fall through
          // and navigate to whatever unrelated result happens to be selected.
          if (permalink) {
            break;
          }
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
            selectPrev();
          } else {
            selectNext();
          }
          break;
        }
      }
    },
    [filteredItems, selectedIndex, handleSelect, onClose, selectNext, selectPrev]
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
        className="bg-surface border border-line"
        style={{
          width: "100%",
          maxWidth: 560,
          maxHeight: "70vh",
          display: "flex",
          flexDirection: "column",
          borderRadius: "0.75rem",
          overflow: "hidden",
          boxShadow: "0 25px 50px -12px rgba(0, 0, 0, 0.5)",
        }}
        onKeyDown={handleKeyDown}
      >
        {/* Search input */}
        <div
          className="border-b border-line"
          style={{
            display: "flex",
            alignItems: "center",
            gap: "0.75rem",
            padding: "0.875rem 1rem",
          }}
        >
          <Search
            className="flex-shrink-0 text-muted"
            size={16}
          />
          <input
            ref={inputRef}
            data-testid="search-panel-input"
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={t("panel.placeholder")}
            autoComplete="off"
            autoCorrect="off"
            autoCapitalize="off"
            spellCheck={false}
            className="flex-1 font-mono text-sm text-fg border-0"
            style={{
              background: "transparent",
              outline: "none",
            }}
          />
          <kbd
            className="font-mono font-machine text-xs text-muted bg-bg border border-line"
            style={{
              padding: "2px 6px",
              borderRadius: "4px",
            }}
          >
            esc
          </kbd>
        </div>

        {/* Keyboard hints */}
        <div
          className="flex items-center gap-1 px-4 py-1.5 text-xs font-mono flex-shrink-0 border-b border-line text-muted"
        >
          <ArrowUp className="w-3 h-3" />
          <ArrowDown className="w-3 h-3" />
          <span className="text-line-strong">/</span>
          <span>{prevLabel}</span>
          <span>{nextLabel}</span>
          <span>{t("panel.hintNavigate")}</span>
          <span className="mx-1 text-line-strong">
            &bull;
          </span>
          <span>{t("panel.hintSelect")}</span>
          <span className="mx-1 text-line-strong">
            &bull;
          </span>
          <span>{t("panel.hintClose")}</span>
        </div>

        {/* Results */}
        <div
          ref={resultsRef}
          data-testid="search-panel-results"
          className="overflow-y-auto"
          style={{ flex: 1 }}
        >
          {permalink ? (
            // A permalink is a pointer, not a capability, so the two outcomes
            // are simply "we have it" or "we don't". The missing case must not
            // hint at whether the message exists, who sent it, or what it says.
            <div
              data-testid="search-panel-permalink"
              className="flex items-center gap-3 px-4 py-3 font-mono text-xs text-muted"
            >
              <LinkIcon size={14} className="flex-shrink-0" />
              {permalinkStatus === "missing" ? (
                <span data-testid="permalink-unresolved">
                  {t("panel.permalinkMissing")}
                </span>
              ) : (
                <span data-testid="permalink-resolving">{t("panel.permalinkResolving")}</span>
              )}
            </div>
          ) : filteredItems.length === 0 ? (
            <div
              data-testid="search-panel-empty"
              className="text-center py-8 font-mono text-xs text-muted"
            >
              {query.trim()
                ? t("panel.noMatches")
                : t("panel.emptyHint")}
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
                  className={`flex items-center gap-3 px-4 py-2.5 cursor-pointer transition-colors ${isSelected ? "bg-active" : ""}`}
                  style={{
                    borderInlineStart: `3px solid ${isSelected ? "var(--c-accent)" : "transparent"}`,
                  }}
                  onClick={() => handleSelect(item)}
                  onMouseEnter={() => setSelectedIndex(index)}
                >
                  {/* Icon */}
                  <div
                    className={`flex-shrink-0 w-5 h-5 flex items-center justify-center ${isSelected ? "text-accent" : "text-dim"}`}
                  >
                    {item.type === "messages" ? (
                      <Search size={14} />
                    ) : item.type === "channel" ? (
                      <Hash size={14} />
                    ) : item.type === "voice" ? (
                      <Volume2 size={14} />
                    ) : item.type === "person" ? (
                      <PresenceAvatar
                        userId={item.userId}
                        avatarKey={item.avatarKey ?? null}
                        size={20}
                        alt={item.name}
                      />
                    ) : (
                      <SettingsIcon size={14} />
                    )}
                  </div>

                  {/* Name and breadcrumb */}
                  <div className="flex-1 min-w-0">
                    <div
                      className={`font-sans text-sm truncate flex items-center gap-2 ${isSelected ? "text-accent" : "text-fg"}`}
                    >
                      <span>{item.name}</span>
                      {item.type === "voice" && item.channelId === activeVoiceChannelId && (
                        <span className="font-mono text-xs text-accent">{t("panel.live")}</span>
                      )}
                    </div>
                    <div
                      className="font-mono text-xs truncate text-muted"
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
});
