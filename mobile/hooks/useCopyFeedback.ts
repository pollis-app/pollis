import { useCallback, useEffect, useRef, useState } from "react";
import * as Clipboard from "expo-clipboard";
import { haptic } from "../lib/haptics";

export type CopyState = "idle" | "copied" | "failed";

const COPY_FEEDBACK_MS = 2000;

/**
 * Copy text and say how it went for two seconds (#897): the copy is VERIFIED by the clipboard
 * call's own answer, never read back, and felt either way. Invite links, the emergency kit and the
 * bot pairing command (#1182) share it.
 */
export function useCopyFeedback(): { copyState: CopyState; copy: (text: string) => Promise<void> } {
  const [copyState, setCopyState] = useState<CopyState>("idle");
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    if (copyState === "idle") {
      return;
    }
    timer.current = setTimeout(() => setCopyState("idle"), COPY_FEEDBACK_MS);
    return () => {
      if (timer.current) {
        clearTimeout(timer.current);
      }
    };
  }, [copyState]);

  const copy = useCallback(async (text: string) => {
    setCopyState("idle");
    const copied = await Clipboard.setStringAsync(text).catch(() => false);
    haptic(copied ? "success" : "error");
    setCopyState(copied ? "copied" : "failed");
  }, []);

  return { copyState, copy };
}
