// Layout class for iPadOS adaptive layouts (#622): `regular` takes the
// two-pane paths, `compact` renders like a phone.

import { useWindowDimensions } from "react-native";

// iPad portrait is 768+; a Split-View half (~507–678 pt) stays `compact`.
export const REGULAR_MIN_WIDTH = 700;

export type LayoutClass = "compact" | "regular";

export function useLayoutClass(): LayoutClass {
  const { width } = useWindowDimensions();
  return width >= REGULAR_MIN_WIDTH ? "regular" : "compact";
}

export function useIsRegular(): boolean {
  return useLayoutClass() === "regular";
}
