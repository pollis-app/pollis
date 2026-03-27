import React, { useState, useEffect, useRef, useMemo, useCallback } from "react";
import { useNavigate } from "@tanstack/react-router";
import { Hash, AtSign, Search, ArrowUp, ArrowDown } from "lucide-react";
import { useUserGroupsWithChannels } from "../hooks/queries/useGroups";
import { useDMConversations } from "../hooks/queries/useMessages";
import type { GroupWithChannels } from "../services/api";

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
      type: "dm";
      id: string;
      name: string;
      breadcrumb: string;
      conversationId: string;
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
    return nameMatch || breadcrumbMatch;
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

  // Build the full list of searchable items
  const allItems = useMemo(() => {
    const channels = buildChannelResults(groupsWithChannels);
    const dms = buildDMResults(dmConversations);
    return [...channels, ...dms];
  }, [groupsWithChannels, dmConversations]);

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

  const handleSelect = useCallback(
    (item: SearchResultItem) => {
      onClose();
      if (item.type === "channel") {
        navigate({
          to: "/groups/$groupId/channels/$channelId",
          params: { groupId: item.groupId, channelId: item.channelId },
        });
      } else {
        navigate({
          to: "/dms/$conversationId",
          params: { conversationId: item.conversationId },
        });
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
            placeholder="Jump to channel or DM..."
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
                ? "No channels or conversations found"
                : "Type to search channels and DMs"}
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
                    ) : (
                      <AtSign size={14} />
                    )}
                  </div>

                  {/* Name and breadcrumb */}
                  <div className="flex-1 min-w-0">
                    <div
                      className="font-sans text-sm truncate"
                      style={{
                        color: isSelected ? "var(--c-accent)" : "var(--c-text)",
                      }}
                    >
                      {item.name}
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
