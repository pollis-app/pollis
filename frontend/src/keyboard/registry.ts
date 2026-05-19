// Single global keydown dispatcher.
//
// One window listener (lazily attached on first registration, detached
// when the last command unregisters) replaces the previous ~8 per-shortcut
// useEffect lifecycles. It resolves each command's combo lazily per
// keydown via resolveCombo, so a future user-override map takes effect
// with zero extra wiring.
//
// Phase: bubble (non-capture), matching the app's prior global Esc
// listener. Capture-phase modal-cancel handlers (ChatInput, MainContent,
// MessageItem, VoiceChannel) still run first and may stopImmediatePropagation
// to claim Escape before nav.back ever sees it — that behavior is preserved.

import { resolveCombo } from "./bindings";
import { comboMatchesEvent, parseCombo } from "./keyCombo";
import type { ShortcutCommandId } from "./commands";

export interface ShortcutRegistration {
  invoke: (e: KeyboardEvent) => void;
  /** When false the command is skipped (e.g. voice shortcuts off-call). */
  enabled: boolean;
  /** Higher wins when multiple enabled commands match the same event. */
  priority: number;
  /** preventDefault on a match. Default true; nav.back opts out. */
  preventDefault: boolean;
}

// Token identity guards against StrictMode / fast-refresh double-invokes:
// unregister only clears the slot if it still holds *this* registration.
const registry = new Map<ShortcutCommandId, ShortcutRegistration>();
const tokens = new Map<ShortcutCommandId, object>();

let listenerAttached = false;

function onKeyDown(e: KeyboardEvent): void {
  let best: ShortcutRegistration | null = null;

  for (const [id, reg] of registry) {
    if (!reg.enabled) {
      continue;
    }
    const parsed = parseCombo(resolveCombo(id));
    if (!comboMatchesEvent(parsed, e)) {
      continue;
    }
    if (!best || reg.priority > best.priority) {
      best = reg;
    }
  }

  if (!best) {
    return;
  }
  if (best.preventDefault) {
    e.preventDefault();
  }
  best.invoke(e);
}

function ensureListener(): void {
  if (listenerAttached) {
    return;
  }
  window.addEventListener("keydown", onKeyDown);
  listenerAttached = true;
}

function maybeDetachListener(): void {
  if (listenerAttached && registry.size === 0) {
    window.removeEventListener("keydown", onKeyDown);
    listenerAttached = false;
  }
}

export function registerShortcut(
  id: ShortcutCommandId,
  reg: ShortcutRegistration,
): () => void {
  if (registry.has(id)) {
    console.warn(
      `[keyboard] "${id}" is already registered; overwriting. Each global ` +
        `command should have a single owner.`,
    );
  }
  const token = {};
  registry.set(id, reg);
  tokens.set(id, token);
  ensureListener();

  return () => {
    if (tokens.get(id) === token) {
      registry.delete(id);
      tokens.delete(id);
      maybeDetachListener();
    }
  };
}
