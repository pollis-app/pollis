import { create } from "zustand";

// Tracks whether a file-drop target (a mounted ChatInput) currently exists, so
// the global drag-over overlay in AppShell only appears on views that can
// actually receive a dropped file. Without this the overlay showed app-wide —
// e.g. while watching a stream in a voice channel, where there's no chat input
// and a dropped file goes nowhere (confusing).
//
// Ref-counted rather than a plain boolean so transient double-mounts (React
// StrictMode) and any future layout with more than one input don't clear the
// flag prematurely.
interface DropTargetStore {
  count: number;
  register: () => void;
  unregister: () => void;
}

export const useDropTargetStore = create<DropTargetStore>((set) => ({
  count: 0,
  register: () => set((s) => ({ count: s.count + 1 })),
  unregister: () => set((s) => ({ count: Math.max(0, s.count - 1) })),
}));

// Read the current value outside React (e.g. inside the AppShell drag-event
// callback, which is registered once and would otherwise close over a stale
// value).
export const isDropTargetActive = (): boolean =>
  useDropTargetStore.getState().count > 0;
