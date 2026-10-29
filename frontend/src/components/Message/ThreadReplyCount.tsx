import React from "react";
import { useTranslation } from "react-i18next";
import { MessagesSquare } from "lucide-react";
import { PressButton } from "../ui/PressButton";

interface ThreadReplyCountProps {
  count: number;
  onOpen?: () => void;
}

/** "N replies" under a message with a thread (#825); nothing when 0. */
export const ThreadReplyCount: React.FC<ThreadReplyCountProps> = ({
  count,
  onOpen,
}) => {
  const { t } = useTranslation("chat");
  if (count <= 0 || !onOpen) {
    return null;
  }

  return (
    <PressButton
      type="button"
      onClick={onOpen}
      data-testid="thread-reply-count"
      className="mt-1 flex items-center gap-1 rounded px-1 py-0.5 text-xs text-accent hover:bg-hover"
    >
      <MessagesSquare size={12} aria-hidden />
      {t("thread.replyCount", { count })}
    </PressButton>
  );
};
