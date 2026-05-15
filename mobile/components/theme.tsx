import { createContext, useContext, useState, useCallback } from "react";
import {
  setAccentRgb,
  hexToRgbTriplet,
  DEFAULT_ACCENT_HEX,
} from "../theme/tokens";

type ThemeCtx = {
  accentHex: string;
  setAccent: (hex: string) => void;
};

const Ctx = createContext<ThemeCtx>({
  accentHex: DEFAULT_ACCENT_HEX,
  setAccent: () => {},
});

export function ThemeProvider({ children }: { children: React.ReactNode }) {
  const [accentHex, setAccentHex] = useState(DEFAULT_ACCENT_HEX);

  const setAccent = useCallback((hex: string) => {
    setAccentRgb(hexToRgbTriplet(hex));
    setAccentHex(hex);
  }, []);

  return (
    <Ctx.Provider value={{ accentHex, setAccent }}>{children}</Ctx.Provider>
  );
}

// Subscribing to this re-renders the consumer when the accent changes — so
// the token getters resolve to the new color. <Screen> and <TabBar> both
// subscribe, which covers the whole tree.
export function useTheme() {
  return useContext(Ctx);
}
