/**
 * Is the user at this window right now (#2056)? Focused, visible, and some input in the last
 * `IDLE_AFTER_MS`. One answer drives two things: marking the conversation on screen read
 * (`useReadWhileViewing`) and telling the DS to hold phone pushes (`useActivityReport`).
 *
 * Event-driven: the idle check is a single timeout re-armed from the last input, never a poll.
 */
import { makeAutoObservable } from "mobx";

/** No input for this long and the user is idle: nothing is read, pushes go to the phone. */
export const IDLE_AFTER_MS = 2 * 60 * 1000;

const INPUT_EVENTS = ["pointerdown", "pointermove", "keydown", "wheel", "touchstart"] as const;

export class UserActivityStore {
  focused = true;
  visible = true;
  idle = false;
  /** Bumped on input at most once a second; `useActivityReport` renews from it. */
  lastInputAt = Date.now();

  constructor() {
    makeAutoObservable(this);
  }

  get isActive(): boolean {
    return this.focused && this.visible && !this.idle;
  }

  setFocused(focused: boolean) {
    this.focused = focused;
  }

  setVisible(visible: boolean) {
    this.visible = visible;
  }

  setIdle(idle: boolean) {
    this.idle = idle;
  }

  noteInput(at: number) {
    this.lastInputAt = at;
    this.idle = false;
  }
}

export const userActivityStore = new UserActivityStore();

/** Start following focus, visibility and input; returns the disposer. Call once, from the shell. */
export function startUserActivity(store: UserActivityStore = userActivityStore): () => void {
  if (typeof window === "undefined" || typeof document === "undefined") {
    return () => {};
  }
  let lastInput = Date.now();
  let idleTimer: ReturnType<typeof setTimeout> | null = null;

  const armIdle = (delay: number) => {
    if (idleTimer) {
      clearTimeout(idleTimer);
    }
    idleTimer = setTimeout(() => {
      idleTimer = null;
      const since = Date.now() - lastInput;
      if (since >= IDLE_AFTER_MS) {
        store.setIdle(true);
      } else {
        armIdle(IDLE_AFTER_MS - since);
      }
    }, delay);
  };

  const onInput = () => {
    const now = Date.now();
    lastInput = now;
    // Observable writes at most once a second; pointermove fires far more often.
    if (store.idle || now - store.lastInputAt >= 1000) {
      store.noteInput(now);
    }
    if (!idleTimer) {
      armIdle(IDLE_AFTER_MS);
    }
  };
  const onFocus = () => {
    store.setFocused(true);
    onInput();
  };
  const onBlur = () => {
    store.setFocused(false);
  };
  const onVisibility = () => {
    store.setVisible(document.visibilityState === "visible");
  };

  store.setFocused(document.hasFocus());
  store.setVisible(document.visibilityState === "visible");
  store.noteInput(lastInput);
  armIdle(IDLE_AFTER_MS);

  for (const name of INPUT_EVENTS) {
    window.addEventListener(name, onInput, { passive: true, capture: true });
  }
  window.addEventListener("focus", onFocus);
  window.addEventListener("blur", onBlur);
  document.addEventListener("visibilitychange", onVisibility);

  return () => {
    if (idleTimer) {
      clearTimeout(idleTimer);
    }
    for (const name of INPUT_EVENTS) {
      window.removeEventListener(name, onInput, { capture: true });
    }
    window.removeEventListener("focus", onFocus);
    window.removeEventListener("blur", onBlur);
    document.removeEventListener("visibilitychange", onVisibility);
  };
}
