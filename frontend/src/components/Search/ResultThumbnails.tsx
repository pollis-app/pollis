import React, { useMemo } from "react";
import { AttachmentDisplay } from "../Message/AttachmentDisplay";
import { parseContent } from "../../hooks/queries/useMessages";
import type { SearchResult } from "../../types";

/** Image thumbnails per hit; the rest are a click away in the log. */
const MAX_RESULT_THUMBNAILS = 3;

/**
 * The image half of a search hit's preview: the attachment envelope unwrapped
 * with `parseContent`, each image drawn by `AttachmentDisplay` in `tile` mode.
 * Non-image attachments add nothing here.
 */
export const ResultThumbnails: React.FC<{ result: SearchResult }> = ({ result }) => {
  const images = useMemo(() => {
    if (!result.has_attachment) {
      return [];
    }
    return (parseContent(result.content, { conversationId: result.conversation_id }).attachments ?? [])
      .filter((a) => a.content_type.startsWith("image/"))
      .slice(0, MAX_RESULT_THUMBNAILS);
  }, [result.has_attachment, result.content]);

  if (images.length === 0) {
    return null;
  }

  return (
    <div className="mt-1.5 flex gap-1.5" data-testid="search-result-thumbnails">
      {images.map((attachment) => (
        <div key={attachment.id} className="w-12 flex-shrink-0">
          <AttachmentDisplay
            attachment={attachment}
            tile
            testId="search-result-thumbnail"
          />
        </div>
      ))}
    </div>
  );
};
