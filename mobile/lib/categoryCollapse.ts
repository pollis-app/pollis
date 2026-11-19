// Which channel categories (#2031) are collapsed on this device: a sandbox JSON file, never synced
// (desktop keeps the same set in localStorage). A missing or corrupt file means none collapsed.

import { useSyncExternalStore } from "react";
import * as FileSystem from "expo-file-system/legacy";

const FILE_URI = `${FileSystem.documentDirectory ?? ""}collapsed-categories.json`;
// Plenty for every category a person sees; the oldest drop off first.
const MAX_IDS = 500;

let collapsed: ReadonlySet<string> = new Set();
let hydrated = false;
const listeners = new Set<() => void>();

function emit(): void {
  for (const l of listeners) {
    l();
  }
}

async function hydrate(): Promise<void> {
  if (hydrated) {
    return;
  }
  hydrated = true;
  try {
    const info = await FileSystem.getInfoAsync(FILE_URI);
    if (!info.exists) {
      return;
    }
    const parsed: unknown = JSON.parse(await FileSystem.readAsStringAsync(FILE_URI));
    if (Array.isArray(parsed)) {
      collapsed = new Set(parsed.filter((v): v is string => typeof v === "string"));
      emit();
    }
  } catch {
    // None collapsed.
  }
}

function persist(): void {
  // Fire-and-forget: a lost write costs only the collapse state.
  void FileSystem.writeAsStringAsync(FILE_URI, JSON.stringify([...collapsed].slice(-MAX_IDS))).catch(() => {});
}

/** Collapse or expand one category on this device. */
export function toggleCategoryCollapsed(id: string): void {
  const next = new Set(collapsed);
  if (next.has(id)) {
    next.delete(id);
  } else {
    next.add(id);
  }
  collapsed = next;
  persist();
  emit();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  void hydrate();
  return () => listeners.delete(listener);
}

/** The ids collapsed on this device; re-renders on every change. */
export function useCollapsedCategories(): ReadonlySet<string> {
  return useSyncExternalStore(subscribe, () => collapsed);
}
