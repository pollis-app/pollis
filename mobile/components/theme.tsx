import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useState,
} from "react";
import {
  setAccentRgb,
  hexToRgbTriplet,
  DEFAULT_ACCENT_HEX,
} from "../theme/tokens";
import { usePreferences } from "../hooks/queries/usePreferences";

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
  const { data: prefs, update } = usePreferences();

  // Seed accent from server-side preferences when they first arrive (after
  // sign-in). Subsequent local changes go through `setAccent` below and
  // persist via `update({ accent_hex })`, so this effect only fires on the
  // initial load.
  useEffect(() => {
    const remote = prefs?.accent_hex;
    if (typeof remote === "string" && remote !== accentHex) {
      setAccentRgb(hexToRgbTriplet(remote));
      setAccentHex(remote);
    }
    // Only react to server data changes, not local accentHex updates —
    // otherwise we'd loop on every local pick.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [prefs?.accent_hex]);

  const setAccent = useCallback(
    (hex: string) => {
      setAccentRgb(hexToRgbTriplet(hex));
      setAccentHex(hex);
      update({ accent_hex: hex });
    },
    [update],
  );

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
