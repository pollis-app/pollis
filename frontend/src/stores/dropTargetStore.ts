import { makeAutoObservable } from "mobx";

// Ref-counted drop targets, so the app-wide drag overlay shows only where a drop lands; inline targets draw their own.
class DropTargetStore {
  count = 0;
  inlineCount = 0;

  constructor() {
    makeAutoObservable(this, {}, { autoBind: true });
  }

  register() {
    this.count += 1;
  }

  unregister() {
    this.count = Math.max(0, this.count - 1);
  }

  // Draws its own affordance; still receives the paths.
  registerInline() {
    this.inlineCount += 1;
  }

  unregisterInline() {
    this.inlineCount = Math.max(0, this.inlineCount - 1);
  }

  get active(): boolean {
    return this.count + this.inlineCount > 0;
  }

  get overlay(): boolean {
    return this.count > 0;
  }
}

export const dropTargetStore = new DropTargetStore();

// For callbacks registered once (AppShell's drag handler).
export const isDropTargetActive = (): boolean => dropTargetStore.active;

export const shouldShowDropOverlay = (): boolean => dropTargetStore.overlay;
