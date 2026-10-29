// A second tap on the tab you are already on (#1550): the custom `TabBar`
// calls `emitTabReselect`, and a tab root subscribes to react — Self ends a
// search, the way a tab root pops on iOS. A plain registry rather than
// React Navigation's `tabPress` event: expo-router's `useNavigation()` inside
// a tab route does not reliably reach the listeners of a custom tab bar's
// `emit`, and this keeps the rule testable without a renderer.

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
