import { useEffect } from "react";
import { getMediaUrl } from "../services/r2-upload";
import type { MessageAttachment } from "../types";

/**
 * While `needed`, resolve the attachment's playable URL once and report it through `onResolved`, or `onFailed` on error.
 * A result that lands after `needed` turns false, or after the attachment changes, is dropped.
 */
export function useResolveMediaUrl(
  attachment: MessageAttachment,
  needed: boolean,
  onResolved: (url: string) => void,
  onFailed: () => void,
): void {
  useEffect(() => {
    if (!needed) {
      return;
    }
    let mounted = true;
    getMediaUrl(attachment)
      .then((resolved) => {
        if (mounted) {
          onResolved(resolved);
        }
      })
      .catch(() => {
        if (mounted) {
          onFailed();
        }
      });
    return () => {
      mounted = false;
    };
    // Callers pass state setters; the attachment's identity is its key, hash and type.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [needed, attachment.object_key, attachment.content_hash, attachment.content_type]);
}
