import { makeAutoObservable } from "mobx";

// Whether a file-drop target is mounted, so AppShell's overlay shows only where
// a file can land. Ref-counted (StrictMode, multiple inputs). ChatInputs raise
// the app-wide overlay; inline targets (the emoji upload zone) draw their own
// and would be covered by it. Both receive the paths.
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

  // A target that draws its own affordance; it wants the paths, not the overlay.
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

// For the once-registered AppShell drag callback, which can't close over state.
export const isDropTargetActive = (): boolean => dropTargetStore.active;

export const shouldShowDropOverlay = (): boolean => dropTargetStore.overlay;
