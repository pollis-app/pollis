import React, { useState } from "react";
import { MessageCircle, Plus } from "lucide-react";
import { updateURL } from "../../utils/urlRouting";

interface Conversation {
  id: string;
  user2_identifier: string;
}

interface DirectMessagesListProps {
  conversations: Conversation[];
  selectedConversationId: string | null;
  isCollapsed: boolean;
  onSelectConversation: (conversationId: string) => void;
  onStartDM?: () => void;
}

interface DmItemProps {
  conv: Conversation;
  isActive: boolean;
  isCollapsed: boolean;
  onSelect: (id: string) => void;
}

const DmItem: React.FC<DmItemProps> = ({ conv, isActive, isCollapsed, onSelect }) => {
  const [hovered, setHovered] = useState(false);

  return (
    <div
      style={{ position: "relative" }}
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => setHovered(false)}
    >
      <button
        data-testid={`dm-item-${conv.id}`}
        onClick={() => {
          onSelect(conv.id);
          updateURL(`/c/${conv.id}`);
        }}
        title={isCollapsed ? conv.user2_identifier : undefined}
        aria-label={`Direct message with ${conv.user2_identifier}`}
        className={`sidebar-item ${isActive ? "sidebar-item-active" : ""}`}
        style={{ paddingRight: hovered && !isCollapsed ? "1.75rem" : undefined }}
      >
        <MessageCircle size={15} aria-hidden="true" style={{ flexShrink: 0 }} />
        {!isCollapsed && (
          <span className="truncate text-xs">{conv.user2_identifier}</span>
        )}
      </button>

      {hovered && !isCollapsed && (
        <button
          data-testid={`dm-leave-${conv.id}`}
          onClick={(e) => {
            e.stopPropagation();
            updateURL(`/c/${conv.id}/leave`);
          }}
          title="Leave conversation"
          aria-label={`Leave conversation with ${conv.user2_identifier}`}
          style={{
            position: "absolute",
            right: "0.4rem",
            top: "50%",
            transform: "translateY(-50%)",
            background: "transparent",
            border: "none",
            cursor: "pointer",
            color: "var(--c-text-muted)",
            fontSize: "0.75rem",
            lineHeight: 1,
            padding: "0 0.2rem",
          }}
        >
          ×
        </button>
      )}
    </div>
  );
};

export const DirectMessagesList: React.FC<DirectMessagesListProps> = ({
  conversations,
  selectedConversationId,
  isCollapsed,
  onSelectConversation,
  onStartDM,
}) => (
  <div data-testid="dm-list" className="py-1 divider">
    {!isCollapsed && (
      <div className="section-label px-3 pt-2 pb-1">Direct Messages</div>
    )}

    {conversations.length === 0 ? (
      !isCollapsed && (
        <p className="px-3 py-1 text-xs" style={{ color: 'var(--c-text-muted)' }}>
          No DMs yet.
        </p>
      )
    ) : (
      conversations.map((conv) => (
        <DmItem
          key={conv.id}
          conv={conv}
          isActive={selectedConversationId === conv.id}
          isCollapsed={isCollapsed}
          onSelect={onSelectConversation}
        />
      ))
    )}

    {onStartDM && (
      <button
        data-testid="start-dm-button"
        onClick={onStartDM}
        aria-label="Start direct message"
        className="sidebar-item"
        style={{ color: 'var(--c-text-muted)' }}
      >
        <Plus size={17} aria-hidden="true" />
        {!isCollapsed && <span className="text-xs">New message</span>}
      </button>
    )}
  </div>
);
