// Whether a sensitive attachment (#1568) is covered right now, and the two
// things the user can do about it. Read inside an `observer()` component.
// Desktop's `frontend/src/hooks/useSensitiveReveal.ts`, same rules.

import { useState } from "react";
import { isCovered, revealKey } from "../lib/sensitiveMedia";
import { sensitiveRevealStore } from "../stores/sensitiveRevealStore";
import type { MessageAttachment } from "../types";

export function useSensitiveReveal(attachment: MessageAttachment) {
  const [localRevealed, setLocalRevealed] = useState(false);
  const key = revealKey(attachment);
  const revealed = key ? sensitiveRevealStore.isRevealed(key) : localRevealed;
  return {
    sensitive: attachment.sensitive === true,
    revealed,
    covered: isCovered(attachment, revealed),
    reveal: () => {
      if (key) {
        sensitiveRevealStore.reveal(key);
      } else {
        setLocalRevealed(true);
      }
    },
    hide: () => {
      if (key) {
        sensitiveRevealStore.hide(key);
      } else {
        setLocalRevealed(false);
      }
    },
  };
}
