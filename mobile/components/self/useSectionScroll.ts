import { useCallback, useRef } from "react";
import type { LayoutChangeEvent, ScrollView } from "react-native";

/**
 * Scrolls a settings page to one named section once that section has laid
 * out — used when a Self-tab row (Language, Notifications, Auto-lock) opens a
 * page that holds several settings. Sections must be direct children of the
 * ScrollView's content so their layout `y` is in content coordinates.
 */
export function useSectionScroll(target: string | undefined) {
  const scrollRef = useRef<ScrollView>(null);
  const done = useRef(false);
  const sectionLayout = useCallback(
    (name: string) => (e: LayoutChangeEvent) => {
      if (done.current || !target || name !== target) {
        return;
      }
      done.current = true;
      const y = Math.max(0, e.nativeEvent.layout.y - 8);
      requestAnimationFrame(() => {
        scrollRef.current?.scrollTo({ y, animated: false });
      });
    },
    [target],
  );
  return { scrollRef, sectionLayout };
}
