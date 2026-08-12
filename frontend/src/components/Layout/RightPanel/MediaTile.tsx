import React, { useEffect, useState } from "react";
import { FileIcon } from "lucide-react";
import { useSkin } from "../../../hooks/queries/usePreferences";
import { getMediaUrl } from "../../../services/r2-upload";
import type { MessageAttachment } from "../../../types";

interface MediaTileProps {
  attachment: MessageAttachment;
}

/**
 * One square thumbnail in the right panel's media grid.
 *
 * Images resolve through the loopback media server exactly as
 * `AttachmentDisplay` does — `getMediaUrl` returns
 * `http://127.0.0.1:<port>/<token>/<hash>`, so bytes never cross the JSON IPC
 * and the on-disk cache stays encrypted. Non-images (and anything that fails
 * to resolve) fall back to an icon rather than an empty box, so the grid never
 * shows holes.
 */
export const MediaTile: React.FC<MediaTileProps> = ({ attachment }) => {
  const [url, setUrl] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  const isTerminal = useSkin() === "terminal";
  const isImage = attachment.content_type.startsWith("image/");
  // An attachment still uploading has no object_key yet; there is nothing to
  // fetch until the send confirms.
  const isPending = !attachment.object_key;

  useEffect(() => {
    if (!isImage || isPending || url || failed) {
      return;
    }
    let mounted = true;
    getMediaUrl(
      attachment.object_key,
      attachment.content_hash,
      attachment.content_type,
    )
      .then((resolved) => {
        if (mounted) {
          setUrl(resolved);
        }
      })
      .catch(() => {
        if (mounted) {
          setFailed(true);
        }
      });
    return () => {
      mounted = false;
    };
  }, [
    isImage,
    isPending,
    url,
    failed,
    attachment.object_key,
    attachment.content_hash,
    attachment.content_type,
  ]);

  return (
    <div
      // Terminal keeps hard corners — the skin's chrome is square everywhere
      // else, and a rounded thumbnail is the tell that this panel was styled
      // for refined only.
      className={`relative aspect-square overflow-hidden border border-line bg-surface-raised ${
        isTerminal ? "" : "rounded"
      }`}
      title={attachment.filename}
      data-testid="right-panel-media-tile"
    >
      {url ? (
        <img
          src={url}
          alt={attachment.filename}
          loading="lazy"
          className="h-full w-full object-cover"
          onError={() => setFailed(true)}
        />
      ) : (
        <div className="flex h-full w-full items-center justify-center text-muted">
          <FileIcon size={16} aria-hidden />
        </div>
      )}
    </div>
  );
};
