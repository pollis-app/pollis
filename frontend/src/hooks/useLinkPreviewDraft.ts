import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { discardStagedAttachment, invoke } from "../bridge";
import {
  LinkPreviewComposer,
  NO_LINK_PREVIEW,
  type LinkPreviewDraft,
  type LinkPreviewState,
} from "../utils/linkPreview";
import { scanText } from "../../../design/messageFormat";

export interface LinkPreviewDraftHandle {
  state: LinkPreviewState;
  /** The card's ×: hides it, and that link stays unpreviewed for the rest of this draft. */
  remove: () => void;
  /** At send: the finished preview if its link is still in `text`, else null. Never waits. */
  takeForSend: (text: string) => LinkPreviewDraft | null;
  /** The draft was abandoned (cancelled, room switched): release its staged thumbnail. */
  reset: () => void;
}

/**
 * The composer's sender-generated preview (#1987) for `text`. `enabled` is false in the Terminal
 * look, with the setting off, or in a composer that never sends one (the Vault). A new `draftKey`
 * (another room) starts a fresh draft: nothing previewed or suppressed carries over.
 */
export function useLinkPreviewDraft(
  text: string,
  enabled: boolean,
  draftKey: string | null = null,
): LinkPreviewDraftHandle {
  const [state, setState] = useState<LinkPreviewState>(NO_LINK_PREVIEW);
  const composerRef = useRef<LinkPreviewComposer | null>(null);
  if (composerRef.current === null) {
    composerRef.current = new LinkPreviewComposer({
      fetch: (url) => invoke<LinkPreviewDraft | null>("fetch_link_preview", { url }),
      discard: (stagedId) => {
        void discardStagedAttachment(stagedId).catch(() => undefined);
      },
      onChange: setState,
    });
  }
  const composer = composerRef.current;

  // Before paint, so a pasted link's loading card lands on the same frame as the text.
  const draftKeyRef = useRef(draftKey);
  useLayoutEffect(() => {
    if (draftKeyRef.current !== draftKey) {
      draftKeyRef.current = draftKey;
      composer.reset();
    }
    // Only a link outside code blocks gets a preview (#2142).
    composer.update(scanText(text), enabled);
  }, [composer, text, enabled, draftKey]);

  // Unmount (the bar closes, the panel goes): nothing staged outlives the draft.
  useEffect(() => () => composer.reset(), [composer]);

  return {
    state,
    remove: () => composer.remove(),
    takeForSend: (sent) => composer.takeForSend(scanText(sent)),
    reset: () => composer.reset(),
  };
}
