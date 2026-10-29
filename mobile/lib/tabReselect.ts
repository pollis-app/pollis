// A second tap on the focused tab (#1550). A plain registry: expo-router's `useNavigation()` does not reliably reach a custom tab bar's `emit`.

type Listener = () => void;

const listeners = new Map<string, Set<Listener>>();

export function onTabReselect(tab: string, listener: Listener): () => void {
  let set = listeners.get(tab);
  if (!set) {
    set = new Set();
    listeners.set(tab, set);
  }
  set.add(listener);
  return () => {
    set.delete(listener);
  };
}

export function emitTabReselect(tab: string): void {
  for (const listener of listeners.get(tab) ?? []) {
    listener();
  }
}
