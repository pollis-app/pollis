// Resolves an attachment to a local `file://` URI and releases it on unmount (#346). `null` no-ops.
// A file still on disk (held elsewhere, or just released) is answered on the first render, so a
// remount (a Vault tab switch, a row scrolled back) draws the picture with no placeholder.

import { useEffect, useState } from "react";
import {
  mediaCacheGeneration,
  peekMediaUri,
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
  const [state, setState] = useState<MediaUriState>(() => {
    const cached = attachment?.object_key ? peekMediaUri(attachment.content_hash) : null;
    return { uri: cached, loading: !!attachment && !cached, error: null };
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
    const cached = peekMediaUri(contentHash);
    setState((prev) =>
      cached
        ? prev.uri === cached && !prev.loading
          ? prev
          : { uri: cached, loading: false, error: null }
        : { uri: null, loading: true, error: null },
    );

    resolveMediaUri(attachment)
      .then((uri) => {
        if (active) {
          setState((prev) =>
            prev.uri === uri && !prev.loading ? prev : { uri, loading: false, error: null },
          );
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
