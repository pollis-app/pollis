// `useMediaUri`'s twin for a copy typed by its extension (#1248), which the
// players (AVPlayer), the photo library and the share sheet need.
// `enabled: false` takes no reference, so a viewer page can defer the copy.

import { useEffect, useState } from "react";
import {
  mediaCacheGeneration,
  releaseNamedMediaUri,
  resolveNamedMediaUri,
} from "../lib/media/cache";
import type { MessageAttachment } from "../types";

export interface NamedMediaUriState {
  uri: string | null;
  loading: boolean;
  error: Error | null;
}

export function useNamedMediaUri(
  attachment: Pick<
    MessageAttachment,
    "object_key" | "content_hash" | "content_type" | "filename" | "localPreviewUri" | "sha256" | "key_scope"
  > | null,
  enabled: boolean,
): NamedMediaUriState {
  const [state, setState] = useState<NamedMediaUriState>({
    uri: null,
    loading: !!attachment && enabled,
    error: null,
  });

  const contentHash = attachment?.content_hash ?? null;
  const objectKey = attachment?.object_key ?? null;
  const preview = attachment?.localPreviewUri ?? null;

  useEffect(() => {
    if (!attachment || !enabled || (!objectKey && !preview)) {
      setState({ uri: null, loading: false, error: null });
      return;
    }

    let active = true;
    // A release after a sign-out clear must not touch the next session's
    // counts (see `releaseNamedMediaUri`).
    const resolvedIn = mediaCacheGeneration();
    setState({ uri: null, loading: true, error: null });
    resolveNamedMediaUri(attachment)
      .then((uri) => {
        if (active) {
          setState({ uri, loading: false, error: null });
        }
      })
      .catch((err: unknown) => {
        if (active) {
          setState({
            uri: null,
            loading: false,
            error: err instanceof Error ? err : new Error(String(err)),
          });
        }
      });

    return () => {
      active = false;
      void releaseNamedMediaUri(
        { object_key: objectKey ?? "", content_hash: contentHash ?? "" },
        resolvedIn,
      );
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [contentHash, objectKey, preview, enabled]);

  return state;
}
