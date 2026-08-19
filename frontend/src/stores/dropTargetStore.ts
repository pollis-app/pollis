import { makeAutoObservable } from "mobx";

// Tracks whether a file-drop target (a mounted ChatInput) currently exists, so
// the global drag-over overlay in AppShell only appears on views that can
// actually receive a dropped file. Without this the overlay showed app-wide —
// e.g. while watching a stream in a voice channel, where there's no chat input
// and a dropped file goes nowhere (confusing).
//
// Ref-counted rather than a plain boolean so transient double-mounts (React
// StrictMode) and any future layout with more than one input don't clear the
// flag prematurely.
//
// Two kinds of target, because there are two right answers to "what should a
// drag look like": a ChatInput wants the app-wide overlay (the file could be
// dropped anywhere), while a page with its own drop area — the custom-emoji
// upload zone — draws the affordance itself and would only be covered up by
// the overlay. Both receive the dropped paths; only the first raises it.
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

  // A target that draws its own drag-over affordance in the page (the emoji
  // upload zone). It still wants the dropped paths, but the full-window
  // overlay would sit on top of the very area it is highlighting.
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

// Read the current value outside React (e.g. inside the AppShell drag-event
// callback, which is registered once and would otherwise close over a stale
// value).
export const isDropTargetActive = (): boolean => dropTargetStore.active;

// Whether the drop that is about to land wants the app-wide overlay, as
// opposed to a page that highlights its own drop area.
export const shouldShowDropOverlay = (): boolean => dropTargetStore.overlay;
