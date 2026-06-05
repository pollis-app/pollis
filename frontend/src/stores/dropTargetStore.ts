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
class DropTargetStore {
  count = 0;

  constructor() {
    makeAutoObservable(this, {}, { autoBind: true });
  }

  register() {
    this.count += 1;
  }

  unregister() {
    this.count = Math.max(0, this.count - 1);
  }

  get active(): boolean {
    return this.count > 0;
  }
}

export const dropTargetStore = new DropTargetStore();

// Read the current value outside React (e.g. inside the AppShell drag-event
// callback, which is registered once and would otherwise close over a stale
// value).
export const isDropTargetActive = (): boolean => dropTargetStore.active;
