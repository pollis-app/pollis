import React, { useState, useRef, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { useReactions, useAddReaction, useRemoveReaction } from "../../hooks/queries/useReactions";
import { observer } from "mobx-react-lite";
import { appStore } from "../../stores/appStore";
import { EmojiPicker } from "../Emoji/EmojiPicker";
import { CustomEmojiImage } from "../Emoji/CustomEmojiImage";
import { splitEmojiSegments } from "../Emoji/emojiTokens";

interface MessageReactionsProps {
  messageId: string;
}

/**
 * Render a reaction's emoji — either a Unicode character or a custom
 * `<:name:hash>` token.
 *
 * Reactions are stored as opaque strings, so a custom reaction is simply its
 * wire token; splitting it here is what makes the same string render as an
 * image for someone who has never heard of the group that owns it (#848).
 */
const ReactionEmoji: React.FC<{ emoji: string }> = ({ emoji }) => {
  const segments = splitEmojiSegments(emoji);
  return (
    <>
      {segments.map((segment, index) =>
        segment.kind === "emoji" ? (
          <CustomEmojiImage
            key={index}
            contentHash={segment.contentHash}
            shortcode={segment.shortcode}
            sizeRem={1}
          />
        ) : (
          <span key={index}>{segment.text}</span>
        ),
      )}
    </>
  );
};

export const MessageReactions: React.FC<MessageReactionsProps> = observer(({ messageId }) => {
  const { t } = useTranslation("chat");
  const { currentUser } = appStore;
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

  const toggleReaction = (emoji: string) => {
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

  const handlePickerEmoji = (emoji: string) => {
    toggleReaction(emoji);
    setPickerOpen(false);
  };

  const hasReactions = reactions.length > 0;

  return (
    <div className="flex items-center flex-wrap gap-1 ms-[3.25rem] mt-0.5">
      {/* Existing reaction pills */}
      {hasReactions && reactions.map((reaction) => {
        const reacted = currentUser
          ? reaction.user_ids.includes(currentUser.id)
          : false;

        return (
          <button
            key={reaction.emoji}
            data-testid="reaction-pill"
            onClick={() => toggleReaction(reaction.emoji)}
            className="panel-raised flex items-center gap-1 px-1.5 py-0.5 text-xs font-mono transition-colors duration-75 hover:opacity-90"
            style={{
              color: reacted ? "var(--c-accent)" : "var(--c-text-muted)",
              borderColor: reacted ? "var(--c-border-active)" : undefined,
            }}
            aria-label={t("reactions.pillLabel", {
              emoji: reaction.emoji,
              count: reaction.count,
            })}
            aria-pressed={reacted}
          >
            <ReactionEmoji emoji={reaction.emoji} />
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
          aria-label={t("reactions.add")}
          aria-expanded={pickerOpen}
        >
          +
        </button>

        {/*
          The real picker (#848), replacing the eight hard-coded emoji this
          affordance used to be. Anchored `absolute` inside this `relative`
          wrapper — no portal, no fixed overlay, no backdrop.
        */}
        {pickerOpen && (
          <div data-testid="reaction-picker" className="absolute bottom-full mb-1 start-0 z-40">
            <EmojiPicker onSelect={handlePickerEmoji} onClose={() => setPickerOpen(false)} closeOnSelect />
          </div>
        )}
      </div>
    </div>
  );
});
