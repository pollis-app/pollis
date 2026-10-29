import React from "react";
import { useTranslation } from "react-i18next";
import { useReactions, useToggleReaction } from "../../hooks/queries/useReactions";
import { observer } from "mobx-react-lite";
import { appStore } from "../../stores/appStore";
import { CustomEmojiImage } from "../Emoji/CustomEmojiImage";
import { splitEmojiSegments } from "../Emoji/emojiTokens";

interface MessageReactionsProps {
  messageId: string;
  className?: string;
}

/** Unicode or a custom `<:name:hash>` token (#848). */
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

/** Reaction pills; renders nothing until the message has one. */
export const MessageReactions: React.FC<MessageReactionsProps> = observer(({ messageId, className = "" }) => {
  const { t } = useTranslation("chat");
  const { currentUser } = appStore;
  const { data: reactions = [] } = useReactions(messageId);
  const toggleReaction = useToggleReaction(messageId);

  if (reactions.length === 0) {
    return null;
  }

  return (
    <div data-testid="message-reactions" className={`flex items-center flex-wrap gap-1 ${className}`}>
      {reactions.map((reaction) => {
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
    </div>
  );
});
