// Responsive layout class for iPadOS adaptive layouts (issue #622).
//
// Two classes only: `compact` (phones, and any narrow pane) render exactly as
// today; `regular` (iPad portrait 768+, landscape) is the one that takes the
// adaptive paths (centered columns, and — Pass 2 — the two-pane master-detail).
//
// The threshold is deliberately above the iPad Split-View half-width (~507–678
// pt), so a narrow pane stays `compact` and behaves like a phone — which is the
// correct behavior for a squeezed column.

import { useWindowDimensions } from "react-native";

// Minimum width (pt) at which a surface is treated as `regular`. iPad portrait
// is 768+; a Split-View half is ~507–678, so it stays below this and resolves
// to `compact`.
export const REGULAR_MIN_WIDTH = 700;

export type LayoutClass = "compact" | "regular";

// Re-renders on rotation / Split-View resize because `useWindowDimensions` is
// reactive.
export function useLayoutClass(): LayoutClass {
  const { width } = useWindowDimensions();
  return width >= REGULAR_MIN_WIDTH ? "regular" : "compact";
}

export function useIsRegular(): boolean {
  return useLayoutClass() === "regular";
}
