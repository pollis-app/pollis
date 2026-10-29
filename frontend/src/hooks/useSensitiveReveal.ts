import { useState } from "react";
import { sensitiveRevealStore } from "../stores/sensitiveRevealStore";
import type { MessageAttachment } from "../types";

/** Hidden state of a sensitive attachment (#1568), shared by id; local until an uploading one has its id. */
export function useSensitiveReveal(attachment: MessageAttachment) {
  const [localRevealed, setLocalRevealed] = useState(false);
  const key = attachment.object_key ? attachment.id : null;
  const revealed = key ? sensitiveRevealStore.isRevealed(key) : localRevealed;
  const sensitive = attachment.sensitive === true;
  return {
    sensitive,
    hidden: sensitive && !revealed,
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
