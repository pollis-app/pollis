import { useEffect, useRef } from "react";

import { registerShortcut } from "./registry";
import type { ShortcutCommandId } from "./commands";

export interface UseGlobalShortcutOptions {
  /** Skip dispatch while false; stays registered. */
  enabled?: boolean;
  /** Higher wins when several enabled commands match one event. */
  priority?: number;
  /** preventDefault on match. Default true; pass false for nav.back. */
  preventDefault?: boolean;
  /** Hold-style: `handler` on down, `onRelease` on up, blur or unmount mid-hold. Ref-held. */
  onRelease?: () => void;
}

/** Bind a global command by id; the combo comes from commands.ts. Ref-held handler. */
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
  // Structural, so a new inline closure doesn't re-register.
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
