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
  persistBackground,
} from "../theme/accentStore";
import { deviceAccentBackend } from "../theme/deviceAccent";
import { backgroundFromPrefs, colorPreferencesPatch } from "../../design/theme.ts";
import { usePreferences } from "../hooks/queries/usePreferences";
import { saveThemeForExtension } from "../lib/push/extensionTheme";

type ThemeCtx = {
  accentHex: string;
  setAccent: (hex: string) => void;
  backgroundHex: string;
  // Synced as `background_color`, shared with desktop.
  setBackground: (hex: string) => void;
  // Changes whenever either base colour does: a list's `extraData`, so its rows repaint on a background change too (#2107).
  themeKey: string;
};

const Ctx = createContext<ThemeCtx>({
  accentHex: DEFAULT_ACCENT_HEX,
  setAccent: () => {},
  backgroundHex: DEFAULT_BACKGROUND_HEX,
  setBackground: () => {},
  themeKey: `${DEFAULT_ACCENT_HEX}|${DEFAULT_BACKGROUND_HEX}`,
});

export function ThemeProvider({ children }: { children: React.ReactNode }) {
  // Start from what tokens.ts already paints (#1447), never the default.
  const [accentHex, setAccentHex] = useState(currentAccentHex);
  const [backgroundHex, setBackgroundHex] = useState(currentBackgroundHex);
  const { data: prefs, dataUpdatedAt, update, refetch } = usePreferences();

  const applyAccent = useCallback((hex: string) => {
    setAccentRgb(hexToRgbTriplet(hex));
    setAccentHex(hex);
    persistAccent(deviceAccentBackend, hex);
  }, []);

  // The synced preference wins once loaded; refetched the moment the app unlocks (#1447).
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
      update(colorPreferencesPatch({ accent: hex }));
    },
    [applyAccent, update],
  );

  const applyBackground = useCallback((hex: string) => {
    setBackgroundRgb(hexToRgbTriplet(hex));
    setBackgroundHex(hex);
    persistBackground(deviceAccentBackend, hex);
  }, []);

  // The synced background (desktop picks it), else the default once a trusted read says there is none.
  const remoteBackground = backgroundFromPrefs(prefs) ?? (trusted || !userId ? DEFAULT_BACKGROUND_HEX : null);
  useEffect(() => {
    if (remoteBackground && remoteBackground !== currentBackgroundHex()) {
      applyBackground(remoteBackground);
    }
  }, [remoteBackground, applyBackground]);

  const setBackground = useCallback(
    (hex: string) => {
      applyBackground(hex);
      update(colorPreferencesPatch({ background: hex }));
    },
    [applyBackground, update],
  );

  // #1599: the notification extension draws its avatars in the same colours (iOS).
  useEffect(() => {
    saveThemeForExtension(accentHex, backgroundHex);
  }, [accentHex, backgroundHex]);

  const value = useMemo(
    () => ({ accentHex, setAccent, backgroundHex, setBackground, themeKey: `${accentHex}|${backgroundHex}` }),
    [accentHex, setAccent, backgroundHex, setBackground],
  );

  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

// Re-renders only the CALLER, not `children` a parent created (#1447): every route subscribes itself.
export function useTheme() {
  return useContext(Ctx);
}

export function useThemeRerender(): void {
  useContext(Ctx);
}
