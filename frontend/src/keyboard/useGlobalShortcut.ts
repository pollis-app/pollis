import { useEffect, useRef } from "react";

import { registerShortcut } from "./registry";
import type { ShortcutCommandId } from "./commands";

export interface UseGlobalShortcutOptions {
  /**
   * Skip dispatch while false (e.g. voice shortcuts only in a call). The
   * registration stays so the command is still enumerable for a future
   * shortcuts page.
   */
  enabled?: boolean;
  /** Higher wins when several enabled commands match one event. */
  priority?: number;
  /** preventDefault on match. Default true; pass false for nav.back. */
  preventDefault?: boolean;
  /**
   * Makes this a hold-style command: `handler` fires on key down and
   * `onRelease` on key up. The registry also fires `onRelease` when the
   * window loses focus or the command unmounts mid-hold, so a held key can
   * never be stranded in the down state.
   *
   * Held in a ref like `handler`, so an inline closure is fine.
   */
  onRelease?: () => void;
}

/**
 * Bind a global keyboard command by its stable id. The actual key combo is
 * resolved from commands.ts (and, in future, a user-override map) — callers
 * never name a key, so remapping never touches this call site.
 *
 * The handler is held in a ref, so an inline closure does not churn the
 * registration; only id/enabled/priority changes re-register.
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
  // Whether this is a hold command is structural, not per-render: it must
  // not churn the registration just because the caller passed a new inline
  // closure. Only the presence of a release handler matters.
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
