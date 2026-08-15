import React from "react";
import { useTranslation } from "react-i18next";
import { useSkin } from "../../../hooks/queries/usePreferences";
import { MediaTile } from "./MediaTile";
import type { MessageAttachment } from "../../../types";

interface MediaGridProps {
  attachments: MessageAttachment[];
}

/**
 * Shared-media grid for the active conversation, in the spirit of Messenger's
 * "shared media" block. Newest first — the caller orders the list.
 */
export const MediaGrid: React.FC<MediaGridProps> = ({ attachments }) => {
  const { t } = useTranslation("nav");
  const isTerminal = useSkin() === "terminal";
  // Terminal aligns to the same 2.5 gutter the sidebar rows and section
  // headers use, so the grid reads as part of the same column.
  const gutter = isTerminal ? "px-2.5" : "px-2";

  if (attachments.length === 0) {
    return <p className={`${gutter} text-xs text-muted`}>{t("media.empty")}</p>;
  }

  return (
    <div className={`grid grid-cols-3 gap-1 ${gutter}`}>
      {attachments.map((attachment) => (
        <MediaTile key={attachment.id} attachment={attachment} />
      ))}
    </div>
  );
};
