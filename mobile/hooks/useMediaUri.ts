// React lifecycle hook over the media transport (issue #346).
//
// Resolves a `MessageAttachment` to a local `file://` URI for rendering
// and — crucially — releases its reference when the component unmounts or
// the attachment changes, so the decrypted plaintext file gets unlinked
// once nothing is showing it. This is the "unlink-on-unmount" half of the
// file-path approach; the cache bookkeeping lives in `lib/media/cache`.
//
//   const { uri, loading, error } = useMediaUri(attachment);
//
// Pass `null` (e.g. for a non-media message, or while an optimistic send
// has no object key yet) to no-op cleanly.

import { useEffect, useState } from "react";
import { resolveMediaUri, releaseMediaUri } from "../lib/media/cache";
import type { MessageAttachment } from "../types";

export interface MediaUriState {
  uri: string | null;
  loading: boolean;
  error: Error | null;
}

export function useMediaUri(
  attachment: Pick<
    MessageAttachment,
    "object_key" | "content_hash" | "content_type"
  > | null,
): MediaUriState {
  const [state, setState] = useState<MediaUriState>({
    uri: null,
    loading: !!attachment,
    error: null,
  });

  // Re-resolve only when the underlying bytes change. content_hash is
  // content-addressed, so it's the precise identity key — a new object_key
  // for the same hash is the same file.
  const contentHash = attachment?.content_hash ?? null;
  const objectKey = attachment?.object_key ?? null;

  useEffect(() => {
    if (!attachment || !objectKey || !contentHash) {
      setState({ uri: null, loading: false, error: null });
      return;
    }

    let active = true;
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
      // Pairs with the reference taken inside resolveMediaUri. The last
      // outstanding release unlinks the decrypted file.
      void releaseMediaUri(contentHash);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [contentHash, objectKey]);

  return state;
}
