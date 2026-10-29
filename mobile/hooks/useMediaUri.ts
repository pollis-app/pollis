// Resolves an attachment to a local `file://` URI and releases it on unmount (#346), so the decrypted file is unlinked. `null` no-ops.

import { useEffect, useState } from "react";
import {
  mediaCacheGeneration,
  resolveMediaUri,
  releaseMediaUri,
} from "../lib/media/cache";
import type { MessageAttachment } from "../types";

export interface MediaUriState {
  uri: string | null;
  loading: boolean;
  error: Error | null;
}

export function useMediaUri(
  attachment: Pick<
    MessageAttachment,
    "object_key" | "content_hash" | "content_type" | "sha256" | "key_scope"
  > | null,
): MediaUriState {
  const [state, setState] = useState<MediaUriState>({
    uri: null,
    loading: !!attachment,
    error: null,
  });

  // Re-resolve only when the bytes change: content_hash is the identity.
  const contentHash = attachment?.content_hash ?? null;
  const objectKey = attachment?.object_key ?? null;

  useEffect(() => {
    if (!attachment || !objectKey || !contentHash) {
      setState({ uri: null, loading: false, error: null });
      return;
    }

    let active = true;
  // A release after a sign-out clear must not touch the next session's counts.
    const resolvedIn = mediaCacheGeneration();
    setState({ uri: null, loading: true, error: null });

    resolveMediaUri(attachment)
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
      // The last outstanding release unlinks the decrypted file.
      void releaseMediaUri(contentHash, resolvedIn);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [contentHash, objectKey]);

  return state;
}
