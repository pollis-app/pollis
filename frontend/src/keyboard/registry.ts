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
  /**
   * Hold-style commands (push-to-talk) supply this. `invoke` then means
   * "key went down" and `onRelease` means "key went up — or the window
   * lost focus, so the keyup is never coming".
   *
   * `invoke` fires exactly once per physical press: keyboard auto-repeat
   * re-delivers keydown at ~30 Hz and must not be mistaken for a re-press.
   */
  onRelease?: () => void;
}

// Token identity guards against StrictMode / fast-refresh double-invokes:
// unregister only clears the slot if it still holds *this* registration.
const registry = new Map<ShortcutCommandId, ShortcutRegistration>();
const tokens = new Map<ShortcutCommandId, object>();

/**
 * Hold-style commands whose key is currently down. Membership is what
 * makes `invoke` fire once per press rather than once per auto-repeat, and
 * what `releaseAllHeld` walks on focus loss.
 */
const held = new Set<ShortcutCommandId>();

let listenerAttached = false;

/** Release one held command, if it is held. Safe to call redundantly. */
function release(id: ShortcutCommandId): void {
  if (!held.delete(id)) {
    return;
  }
  registry.get(id)?.onRelease?.();
}

/**
 * Drop every held key.
 *
 * The window losing focus is the important caller. The OS delivers the
 * keydown to us and then routes the *keyup* to whatever the user switched
 * to, so without this a held push-to-talk key stays logically down forever
 * and the microphone stays open in the background. Releasing here is also
 * why the Rust gate exposes `release_voice_ptt` — both ends fail closed.
 */
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
    // Auto-repeat: already down, nothing changed.
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

/**
 * A hidden document is a focus loss the `blur` event does not always
 * report (workspace switch, screen lock). Same failure mode, same fix.
 */
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
      // Unregistering while the key is down (unmount mid-hold, or the
      // command being disabled by leaving the call) must still run the
      // release side — otherwise the mic never closes.
      release(id);
      registry.delete(id);
      tokens.delete(id);
      maybeDetachListener();
    }
  };
}
