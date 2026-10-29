// A sensitive attachment's (#1568) covered state, read inside an `observer()`; desktop's twin.

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
