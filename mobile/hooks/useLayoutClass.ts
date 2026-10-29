// Responsive layout class (#622): `regular` takes the iPad paths, `compact` behaves like a phone.

import { useWindowDimensions } from "react-native";

// Above a Split-View half (~507–678pt), so that stays `compact`.
export const REGULAR_MIN_WIDTH = 700;

export type LayoutClass = "compact" | "regular";

export function useLayoutClass(): LayoutClass {
  const { width } = useWindowDimensions();
  return width >= REGULAR_MIN_WIDTH ? "regular" : "compact";
}

export function useIsRegular(): boolean {
  return useLayoutClass() === "regular";
}
