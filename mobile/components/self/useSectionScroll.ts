import { useCallback, useRef } from "react";
import type { LayoutChangeEvent, ScrollView } from "react-native";

/** Scrolls to one named section once laid out; sections must be direct children of the content. */
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
