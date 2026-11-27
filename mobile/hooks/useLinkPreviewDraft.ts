// The composer's sender-generated link preview (#1987); the rules are lib/linkPreview.ts, shared with desktop.

import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { invoke } from "../lib/native";
import {
  LinkPreviewComposer,
  NO_LINK_PREVIEW,
  type LinkPreviewDraft,
  type LinkPreviewState,
} from "../lib/linkPreview";
import { scanText } from "../../design/messageFormat.ts";

export interface LinkPreviewDraftHandle {
  state: LinkPreviewState;
  /** The card's ×: hides it, and that link stays unpreviewed for the rest of this draft. */
  remove: () => void;
  /** At send: the finished preview if its link is still in `text`, else null. Never waits. */
  takeForSend: (text: string) => LinkPreviewDraft | null;
  /** The draft was abandoned: release its staged thumbnail. */
  reset: () => void;
}

/**
 * Preview for `text` while `enabled` (the device setting, and not an unaccepted request). A new
 * `draftKey` (another conversation, another message being edited) starts a fresh draft.
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
        invoke("discard_staged_attachment", { stagedId }).catch(() => undefined);
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

  // Unmount (the screen goes): nothing staged outlives the draft.
  useEffect(() => () => composer.reset(), [composer]);

  return {
    state,
    remove: () => composer.remove(),
    takeForSend: (sent) => composer.takeForSend(scanText(sent)),
    reset: () => composer.reset(),
  };
}
