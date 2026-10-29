import React from "react";
import { useTranslation } from "react-i18next";
import { SmilePlus } from "lucide-react";
import { EmojiPickerButton } from "../Emoji/EmojiPickerButton";
import { useToggleReaction } from "../../hooks/queries/useReactions";

interface ReactionAddButtonProps {
  messageId: string;
  iconSize: number;
  className: string;
  /** Lets the bar stay visible while the picker is open. */
  onOpenChange?: (open: boolean) => void;
}

/** Add-reaction trigger in the hover bar (#848). */
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
