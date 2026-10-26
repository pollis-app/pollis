import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useSegments } from "expo-router";
import { useObserver } from "mobx-react-lite";
import { appStore } from "../stores/appStore";
import { isInSignedInApp } from "../lib/enrollmentPrompt";
import {
  setAccentRgb,
  setBackgroundRgb,
  hexToRgbTriplet,
  currentAccentHex,
  currentBackgroundHex,
  DEFAULT_ACCENT_HEX,
  DEFAULT_BACKGROUND_HEX,
} from "../theme/tokens";
import {
  accentFromPreferences,
  accentOnUserChange,
  persistAccent,
} from "../theme/accentStore";
import { deviceAccentBackend } from "../theme/deviceAccent";
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
  // Start from what theme/tokens.ts already paints with — the accent this
  // device last showed, read synchronously at load (#1447) — never from the
  // default, or the provider and the tokens disagree until prefs arrive.
  const [accentHex, setAccentHex] = useState(currentAccentHex);
  const [backgroundHex, setBackgroundHex] = useState(currentBackgroundHex);
  const { data: prefs, dataUpdatedAt, update, refetch } = usePreferences();

  const applyAccent = useCallback((hex: string) => {
    setAccentRgb(hexToRgbTriplet(hex));
    setAccentHex(hex);
    persistAccent(deviceAccentBackend, hex);
  }, []);

  // The synced preference wins once it loads (sign-in, or a change made on
  // another device) and is remembered for the next cold start. Before the
  // PIN unlock the read cannot succeed, and its `{}` used to sit in the cache
  // for a minute, so the accent appeared only when some later mount refetched
  // it (#1447). Read it again the moment the app is unlocked. Local picks go
  // through `setAccent` and persist via `update({ accent_hex })`, after which
  // this resolves to the colour already on screen and does nothing.
  const userId = useObserver(() => appStore.currentUser?.id ?? null);
  const isLocked = useObserver(() => appStore.isLocked);
  const segments = useSegments();
  const unlocked = !!userId && isInSignedInApp(segments, isLocked);
  const [unlockedAt, setUnlockedAt] = useState<number | null>(null);
  useEffect(() => {
    if (unlocked) {
      setUnlockedAt(Date.now());
      void refetch();
    } else {
      setUnlockedAt(null);
    }
  }, [unlocked, refetch]);
  // Only a read made after the unlock can say "this account has no accent".
  const trusted = unlockedAt !== null && dataUpdatedAt >= unlockedAt;
  const remoteAccent = accentFromPreferences(prefs, trusted);
  useEffect(() => {
    if (remoteAccent && remoteAccent !== currentAccentHex()) {
      applyAccent(remoteAccent);
    }
  }, [remoteAccent, applyAccent]);

  // Sign-out returns to the default palette.
  const previousUserId = useRef(userId);
  useEffect(() => {
    const next = accentOnUserChange(previousUserId.current, userId);
    previousUserId.current = userId;
    if (next && next !== currentAccentHex()) {
      applyAccent(next);
    }
  }, [userId, applyAccent]);

  const setAccent = useCallback(
    (hex: string) => {
      applyAccent(hex);
      update({ accent_hex: hex });
    },
    [applyAccent, update],
  );

  const setBackground = useCallback((hex: string) => {
    setBackgroundRgb(hexToRgbTriplet(hex));
    setBackgroundHex(hex);
  }, []);

  const value = useMemo(
    () => ({ accentHex, setAccent, backgroundHex, setBackground }),
    [accentHex, setAccent, backgroundHex, setBackground],
  );

  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

// Subscribing re-renders the CALLER when a base colour changes, so the token
// getters it reads resolve to the new theme, and so does every element it
// creates. It does NOT reach elements a parent created and passed down as
// `children`: React bails out on those, so a subscribing <Screen> alone left
// the rest of the page in the old colours until something else re-rendered
// it (#1447). So every route component subscribes, and a list with a
// memoised `renderItem` passes the accent as `extraData`
// (tests/theme-startup.test.ts pins both).
export function useTheme() {
  return useContext(Ctx);
}

/** Re-renders the caller on a palette change, for components that only read tokens. */
export function useThemeRerender(): void {
  useContext(Ctx);
}
