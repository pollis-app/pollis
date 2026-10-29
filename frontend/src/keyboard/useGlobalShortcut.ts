import { useEffect, useRef } from "react";

import { registerShortcut } from "./registry";
import type { ShortcutCommandId } from "./commands";

export interface UseGlobalShortcutOptions {
  /** Skip dispatch while false; the registration stays enumerable. */
  enabled?: boolean;
  priority?: number;
  preventDefault?: boolean;
  /**
   * Makes this hold-style: `handler` on key down, `onRelease` on key up, blur
   * or mid-hold unmount. Held in a ref, so an inline closure is fine.
   */
  onRelease?: () => void;
}

/**
 * Bind a global command by its stable id; combos come from commands.ts and the
 * override map. The handler is held in a ref; only id/enabled/priority changes
 * re-register.
 */
export function useGlobalShortcut(
  id: ShortcutCommandId,
  handler: (e: KeyboardEvent) => void,
  options?: UseGlobalShortcutOptions,
): void {
  const handlerRef = useRef(handler);
  handlerRef.current = handler;

  const onRelease = options?.onRelease;
  const onReleaseRef = useRef(onRelease);
  onReleaseRef.current = onRelease;

  const enabled = options?.enabled ?? true;
  const priority = options?.priority ?? 0;
  const preventDefault = options?.preventDefault ?? true;
  // Only whether a release handler exists matters, not its identity.
  const isHold = !!onRelease;

  useEffect(() => {
    return registerShortcut(id, {
      invoke: (e) => handlerRef.current(e),
      enabled,
      priority,
      preventDefault,
      onRelease: isHold ? () => onReleaseRef.current?.() : undefined,
    });
  }, [id, enabled, priority, preventDefault, isHold]);
}
