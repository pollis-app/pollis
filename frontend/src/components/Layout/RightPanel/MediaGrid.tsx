import React from "react";
import { useTranslation } from "react-i18next";
import { useSkin } from "../../../hooks/queries/usePreferences";
import { AttachmentDisplay } from "../../Message/AttachmentDisplay";
import type { MessageAttachment } from "../../../types";

interface MediaGridProps {
  attachments: MessageAttachment[];
}

/**
 * Shared media for the active conversation, newest first (the caller orders and
 * filters). Cells are `AttachmentDisplay` in `tile` mode, identical to the chat.
 */
export const MediaGrid: React.FC<MediaGridProps> = ({ attachments }) => {
  const { t } = useTranslation("nav");
  const isTerminal = useSkin() === "terminal";
  const gutter = isTerminal ? "px-2.5" : "px-2";

  if (attachments.length === 0) {
    return <p className={`${gutter} text-xs text-muted`}>{t("media.empty")}</p>;
  }

  return (
    <div className={`grid grid-cols-3 gap-1 ${gutter}`}>
      {attachments.map((attachment) => (
        <AttachmentDisplay
          key={attachment.id}
          attachment={attachment}
          tile
          testId="right-panel-media-tile"
        />
      ))}
    </div>
  );
};
