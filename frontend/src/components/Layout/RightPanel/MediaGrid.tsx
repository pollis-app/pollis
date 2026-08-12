import React from "react";
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
  if (attachments.length === 0) {
    return (
      <p className="px-2 text-xs text-muted">No media shared yet.</p>
    );
  }

  return (
    <div className="grid grid-cols-3 gap-1 px-2">
      {attachments.map((attachment) => (
        <MediaTile key={attachment.id} attachment={attachment} />
      ))}
    </div>
  );
};
