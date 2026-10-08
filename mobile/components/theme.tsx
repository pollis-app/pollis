import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useState,
} from "react";
import {
  setAccentRgb,
  setBackgroundRgb,
  hexToRgbTriplet,
  DEFAULT_ACCENT_HEX,
  DEFAULT_BACKGROUND_HEX,
} from "../theme/tokens";
import { usePreferences } from "../hooks/queries/usePreferences";

// The two theme bases. Everything else derives from them (theme/derive.ts).
type ThemeCtx = {
  accentHex: string;
  setAccent: (hex: string) => void;
  backgroundHex: string;
  // Device-local for now: there is no synced preference key for it yet.
  setBackground: (hex: string) => void;
};

const Ctx = createContext<ThemeCtx>({
  accentHex: DEFAULT_ACCENT_HEX,
  setAccent: () => {},
  backgroundHex: DEFAULT_BACKGROUND_HEX,
  setBackground: () => {},
});

export function ThemeProvider({ children }: { children: React.ReactNode }) {
  const [accentHex, setAccentHex] = useState(DEFAULT_ACCENT_HEX);
  const [backgroundHex, setBackgroundHex] = useState(DEFAULT_BACKGROUND_HEX);
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

  const setBackground = useCallback((hex: string) => {
    setBackgroundRgb(hexToRgbTriplet(hex));
    setBackgroundHex(hex);
  }, []);

  return (
    <Ctx.Provider value={{ accentHex, setAccent, backgroundHex, setBackground }}>
      {children}
    </Ctx.Provider>
  );
}

// Subscribing to this re-renders the consumer when a base colour changes — so
// the token getters resolve to the new theme. <Screen> and <TabBar> both
// subscribe, which covers the whole tree.
export function useTheme() {
  return useContext(Ctx);
}
