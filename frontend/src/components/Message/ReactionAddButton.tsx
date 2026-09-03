import React from "react";
import { useTranslation } from "react-i18next";
import { SmilePlus } from "lucide-react";
import { EmojiPickerButton } from "../Emoji/EmojiPickerButton";
import { useToggleReaction } from "../../hooks/queries/useReactions";

interface ReactionAddButtonProps {
  messageId: string;
  iconSize: number;
  /** The hover-bar button chrome, owned by MessageActions so every action in
   * the bar shares one look. */
  className: string;
  /** Reported so the bar can stay visible while the picker is open even
   * though the pointer has left the row. */
  onOpenChange?: (open: boolean) => void;
}

/**
 * The add-reaction trigger in the message hover bar (#848). It used to be a
 * "+" pill on a permanent reactions row under every message; the row now
 * exists only once a reaction does, and adding one is an action like reply or
 * edit.
 *
 * `EmojiPickerButton` owns the panel: anchored, lazy-loaded, flipped to
 * whichever side of the row has room, closed on outside click or Escape. A
 * row near the top of the log opens downward; the composer's opens upward.
 */
export const ReactionAddButton: React.FC<ReactionAddButtonProps> = ({
  messageId,
  iconSize,
  className,
  onOpenChange,
}) => {
  const { t } = useTranslation("chat");
  const toggleReaction = useToggleReaction(messageId);

  return (
    <EmojiPickerButton
      data-testid="reaction-add-btn"
      panelTestId="reaction-picker"
      navAction="react"
      ariaLabel={t("reactions.add")}
      icon={SmilePlus}
      iconSize={iconSize}
      iconClassName="shrink-0"
      buttonClassName={className}
      className="flex items-center"
      placement="down"
      onSelect={toggleReaction}
      closeOnSelect
      onOpenChange={onOpenChange}
    />
  );
};
