import { useState } from "react";
import { sensitiveRevealStore } from "../stores/sensitiveRevealStore";
import type { MessageAttachment } from "../types";

/**
 * Whether a sensitive attachment (#1568) is hidden right now, and the two
 * things the user can do about it. Read inside an `observer()` component.
 *
 * Shared through `sensitiveRevealStore` by attachment id, so the message and
 * the gallery agree. An attachment still uploading has no id of its own yet
 * (its placeholder id repeats across messages), so until it has one the
 * reveal is this component's alone.
 */
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
