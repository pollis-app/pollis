import React, { useMemo } from "react";
import { AttachmentDisplay } from "../Message/AttachmentDisplay";
import { parseContent } from "../../hooks/queries/useMessages";
import type { SearchResult } from "../../types";

/** How many image thumbnails a single hit is willing to show. A result row is
 *  a preview, not the message — the rest are one click away in the log. */
const MAX_RESULT_THUMBNAILS = 3;

/**
 * The image half of a search hit's preview.
 *
 * A snippet is text, so an attached picture reached the row as its filename and
 * nothing else. The full content is already on the result (the backend returns
 * it for the snippet), so the attachment envelope is unwrapped here with the
 * same `parseContent` the message log uses, and each image drawn with the
 * log's own `AttachmentDisplay` in `tile` mode — same blurhash placeholder,
 * same loopback resolution, same lightbox. No URL or decrypt logic is
 * duplicated.
 *
 * Non-image attachments keep their filename in the snippet and add nothing
 * here.
 */
export const ResultThumbnails: React.FC<{ result: SearchResult }> = ({ result }) => {
  const images = useMemo(() => {
    if (!result.has_attachment) {
      return [];
    }
    return (parseContent(result.content).attachments ?? [])
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
