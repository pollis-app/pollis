import React, { useState, useRef, useEffect } from "react";
import { useReactions, useAddReaction, useRemoveReaction } from "../../hooks/queries/useReactions";
import { useAppStore } from "../../stores/appStore";

const COMMON_EMOJIS = ["👍", "❤️", "😂", "😮", "😢", "🔥", "🎉", "👀"];

interface MessageReactionsProps {
  messageId: string;
}

export const MessageReactions: React.FC<MessageReactionsProps> = ({ messageId }) => {
  const currentUser = useAppStore((s) => s.currentUser);
  const { data: reactions = [] } = useReactions(messageId);
  const addReaction = useAddReaction();
  const removeReaction = useRemoveReaction();

  const [pickerOpen, setPickerOpen] = useState(false);
  const pickerRef = useRef<HTMLDivElement>(null);

  // Close picker when clicking outside
  useEffect(() => {
    if (!pickerOpen) {
      return;
    }

    const handleClickOutside = (e: MouseEvent) => {
      if (pickerRef.current && !pickerRef.current.contains(e.target as Node)) {
        setPickerOpen(false);
      }
    };

    document.addEventListener("mousedown", handleClickOutside);
    return () => {
      document.removeEventListener("mousedown", handleClickOutside);
    };
  }, [pickerOpen]);

  const handlePickerEmoji = (emoji: string) => {
    if (!currentUser) {
      return;
    }

    const existing = reactions.find((r) => r.emoji === emoji);
    const alreadyReacted = existing?.user_ids.includes(currentUser.id) ?? false;

    if (alreadyReacted) {
      removeReaction.mutate({ messageId, userId: currentUser.id, emoji });
    } else {
      addReaction.mutate({ messageId, userId: currentUser.id, emoji });
    }

    setPickerOpen(false);
  };

  const handlePillClick = (emoji: string) => {
    if (!currentUser) {
      return;
    }

    const existing = reactions.find((r) => r.emoji === emoji);
    const alreadyReacted = existing?.user_ids.includes(currentUser.id) ?? false;

    if (alreadyReacted) {
      removeReaction.mutate({ messageId, userId: currentUser.id, emoji });
    } else {
      addReaction.mutate({ messageId, userId: currentUser.id, emoji });
    }
  };

  const hasReactions = reactions.length > 0;

  return (
    <div className="flex items-center flex-wrap gap-1 ml-[3.25rem] mt-0.5">
      {/* Existing reaction pills */}
      {hasReactions && reactions.map((reaction) => {
        const reacted = currentUser
          ? reaction.user_ids.includes(currentUser.id)
          : false;

        return (
          <button
            key={reaction.emoji}
            data-testid="reaction-pill"
            onClick={() => handlePillClick(reaction.emoji)}
            className="panel-raised flex items-center gap-1 px-1.5 py-0.5 text-xs font-mono transition-colors duration-75 hover:opacity-90"
            style={{
              color: reacted ? "var(--c-accent)" : "var(--c-text-muted)",
              borderColor: reacted ? "var(--c-border-active)" : undefined,
            }}
            aria-label={`${reaction.emoji} ${reaction.count} reaction${reaction.count !== 1 ? "s" : ""}`}
            aria-pressed={reacted}
          >
            <span>{reaction.emoji}</span>
            <span>{reaction.count}</span>
          </button>
        );
      })}

      {/* Add-reaction trigger — shown on group hover (controlled by parent) */}
      <div className="relative" ref={pickerRef}>
        <button
          data-testid="reaction-add-btn"
          onClick={() => setPickerOpen((prev) => !prev)}
          className="opacity-0 group-hover:opacity-60 hover:!opacity-100 transition-opacity panel-raised px-1.5 py-0.5 text-xs font-mono"
          style={{ color: "var(--c-text-muted)" }}
          aria-label="Add reaction"
          aria-expanded={pickerOpen}
        >
          +
        </button>

        {/* Emoji picker panel */}
        {pickerOpen && (
          <div
            data-testid="reaction-picker"
            className="absolute bottom-full mb-1 left-0 z-50 panel-raised flex gap-1 p-1.5"
            style={{ background: "var(--c-surface-raised)" }}
          >
            {COMMON_EMOJIS.map((emoji) => {
              const existing = reactions.find((r) => r.emoji === emoji);
              const reacted = currentUser
                ? (existing?.user_ids.includes(currentUser.id) ?? false)
                : false;

              return (
                <button
                  key={emoji}
                  onClick={() => handlePickerEmoji(emoji)}
                  className="text-base leading-none hover:scale-125 transition-transform duration-75 px-0.5"
                  style={{
                    filter: reacted ? "drop-shadow(0 0 4px var(--c-accent))" : undefined,
                  }}
                  aria-label={emoji}
                  aria-pressed={reacted}
                >
                  {emoji}
                </button>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
};
