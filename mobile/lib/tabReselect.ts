// A second tap on the focused tab (#1550). A plain registry, not React
// Navigation's `tabPress`: expo-router's `useNavigation()` in a tab route does
// not reliably reach a custom tab bar's `emit`.

type Listener = () => void;

const listeners = new Map<string, Set<Listener>>();

/** Subscribe to re-taps of the tab named `tab`; returns the unsubscribe. */
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

/** The tab bar calls this when the focused tab is tapped again. */
export function emitTabReselect(tab: string): void {
  for (const listener of listeners.get(tab) ?? []) {
    listener();
  }
}
