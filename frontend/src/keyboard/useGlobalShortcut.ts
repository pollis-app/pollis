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

  const enabled = options?.enabled ?? true;
  const priority = options?.priority ?? 0;
  const preventDefault = options?.preventDefault ?? true;

  useEffect(() => {
    return registerShortcut(id, {
      invoke: (e) => handlerRef.current(e),
      enabled,
      priority,
      preventDefault,
    });
  }, [id, enabled, priority, preventDefault]);
}
