// One lazily-attached bubble-phase keydown dispatcher; capture-phase Escape handlers still run first.

import { resolveCombo } from "./bindings";
import { comboMatchesEvent, comboReleasedByEvent, parseCombo } from "./keyCombo";
import type { ShortcutCommandId } from "./commands";

export interface ShortcutRegistration {
  invoke: (e: KeyboardEvent) => void;
  /** When false the command is skipped (e.g. voice shortcuts off-call). */
  enabled: boolean;
  /** Higher wins when multiple enabled commands match the same event. */
  priority: number;
  /** preventDefault on a match. Default true; nav.back opts out. */
  preventDefault: boolean;
  /** Hold-style release (key up, or focus lost); `invoke` fires once per press, not per auto-repeat. */
  onRelease?: () => void;
}

// Token identity guards StrictMode double-invokes.
const registry = new Map<ShortcutCommandId, ShortcutRegistration>();
const tokens = new Map<ShortcutCommandId, object>();

/** Held hold-style commands: once per press, and released on focus loss. */
const held = new Set<ShortcutCommandId>();

let listenerAttached = false;

/** Release one held command, if it is held. Safe to call redundantly. */
function release(id: ShortcutCommandId): void {
  if (!held.delete(id)) {
    return;
  }
  registry.get(id)?.onRelease?.();
}

/** Drop every held key; on blur the keyup goes to another app and the mic would stay open. */
function releaseAllHeld(): void {
  for (const id of [...held]) {
    release(id);
  }
}

function onKeyDown(e: KeyboardEvent): void {
  let bestId: ShortcutCommandId | null = null;
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
      bestId = id;
    }
  }

  if (!best || !bestId) {
    return;
  }
  if (best.preventDefault) {
    e.preventDefault();
  }
  if (best.onRelease) {
    if (held.has(bestId)) {
      return;
    }
    held.add(bestId);
  }
  best.invoke(e);
}

function onKeyUp(e: KeyboardEvent): void {
  if (held.size === 0) {
    return;
  }
  for (const id of [...held]) {
    const parsed = parseCombo(resolveCombo(id));
    if (comboReleasedByEvent(parsed, e)) {
      release(id);
    }
  }
}

/** A hidden document is a focus loss `blur` may miss. */
function onVisibilityChange(): void {
  if (document.visibilityState === "hidden") {
    releaseAllHeld();
  }
}

function ensureListener(): void {
  if (listenerAttached) {
    return;
  }
  window.addEventListener("keydown", onKeyDown);
  window.addEventListener("keyup", onKeyUp);
  window.addEventListener("blur", releaseAllHeld);
  window.addEventListener("pagehide", releaseAllHeld);
  document.addEventListener("visibilitychange", onVisibilityChange);
  listenerAttached = true;
}

function maybeDetachListener(): void {
  if (listenerAttached && registry.size === 0) {
    releaseAllHeld();
    window.removeEventListener("keydown", onKeyDown);
    window.removeEventListener("keyup", onKeyUp);
    window.removeEventListener("blur", releaseAllHeld);
    window.removeEventListener("pagehide", releaseAllHeld);
    document.removeEventListener("visibilitychange", onVisibilityChange);
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
      // Release on unregister mid-hold, or the mic never closes.
      release(id);
      registry.delete(id);
      tokens.delete(id);
      maybeDetachListener();
    }
  };
}
