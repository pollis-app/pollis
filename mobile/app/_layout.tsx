import { useEffect, useState } from "react";
import { Stack } from "expo-router";
import { StatusBar } from "expo-status-bar";
import * as SplashScreen from "expo-splash-screen";
import { SafeAreaProvider } from "react-native-safe-area-context";
import { GestureHandlerRootView } from "react-native-gesture-handler";
import { KeyboardProvider } from "react-native-keyboard-controller";
import {
  useFonts,
  Geist_400Regular,
  Geist_500Medium,
  Geist_600SemiBold,
  Geist_700Bold,
} from "@expo-google-fonts/geist";
import { QueryClientProvider } from "@tanstack/react-query";
import { useObserver } from "mobx-react-lite";
import { semantic } from "../theme/tokens";
import { drillIn, settingsPage } from "../lib/transitions";
import { ThemeProvider } from "../components/theme";
import { queryClient } from "../lib/queryClient";
import { initializeNativeBridge } from "../lib/native";
import { usePushNotifications } from "../hooks/usePushNotifications";
import { useInboxRealtime } from "../hooks/useInboxRealtime";
import { AutoLockProvider } from "../lib/autolock";
import { adoptUserLanguage, hydrateLanguage } from "../i18n";
import { appStore } from "../stores/appStore";
import { sweepStalePlaintext } from "../lib/session";

SplashScreen.preventAutoHideAsync();

// App-level signed-in services, mounted under the providers so they have a
// QueryClient + router and only run after the bridge is ready. Each hook is a
// no-op until a user is signed in: push installs notification listeners; inbox
// realtime keeps the groups/DM lists live while foregrounded; the language
// re-resolves so a user's own stored choice wins over the pre-auth screens'.
function SignedInServicesGate() {
  usePushNotifications();
  useInboxRealtime();
  const userId = useObserver(() => appStore.currentUser?.id ?? null);
  useEffect(() => {
    if (userId) {
      void adoptUserLanguage(userId);
    }
  }, [userId]);
  return null;
}

export default function RootLayout() {
  const [loaded] = useFonts({
    Geist_400Regular,
    Geist_500Medium,
    Geist_600SemiBold,
    Geist_700Bold,
  });
  const [bridgeReady, setBridgeReady] = useState(false);
  const [bridgeError, setBridgeError] = useState<Error | null>(null);
  const [languageReady, setLanguageReady] = useState(false);
  const [sweepReady, setSweepReady] = useState(false);

  // Delete plaintext a crashed or killed run left on disk (decrypted media,
  // export archives) before the boot screen restores any session (#1256).
  // Here rather than in app/index.tsx, which is re-entered mid-session; and
  // once per process (lib/session), so a remount of this layout can't sweep
  // media out from under mounted screens either.
  useEffect(() => {
    sweepStalePlaintext().finally(() => setSweepReady(true));
  }, []);

  // The stored language is read asynchronously; holding the splash for it is
  // what keeps a Spanish user from seeing one English frame on every launch.
  useEffect(() => {
    hydrateLanguage().finally(() => setLanguageReady(true));
  }, []);

  useEffect(() => {
    initializeNativeBridge({
      r2Endpoint: process.env.EXPO_PUBLIC_R2_ENDPOINT,
      r2PublicUrl: process.env.EXPO_PUBLIC_R2_PUBLIC_URL,
      livekitUrl: process.env.EXPO_PUBLIC_LIVEKIT_URL,
      pollisDeliveryUrl: process.env.EXPO_PUBLIC_POLLIS_DELIVERY_URL,
    })
      .then(() => setBridgeReady(true))
      .catch((e) => {
        // Surfacing the error here at least makes it obvious during dev
        // that the bridge didn't come up — the alternative is silent
        // "every command throws" later, which is harder to diagnose.
        console.error("[bridge] initializeNativeBridge failed:", e);
        setBridgeError(e);
        setBridgeReady(true);
      });
  }, []);

  useEffect(() => {
    if (loaded && bridgeReady && languageReady && sweepReady) {
      SplashScreen.hideAsync();
    }
  }, [loaded, bridgeReady, languageReady, sweepReady]);

  if (!loaded || !bridgeReady || !languageReady || !sweepReady) {
    return null;
  }

  // Bridge errors are non-fatal at the layout level — the auth screen will
  // surface a clearer error when the user tries to sign in. We still log
  // above so device logs show what went wrong.
  void bridgeError;

  return (
    <GestureHandlerRootView style={{ flex: 1, backgroundColor: semantic.bg }}>
      <SafeAreaProvider>
        {/* Tracks the soft keyboard for every <Screen> and sheet (#1246):
            the real IME inset under edge-to-edge, animated in step. */}
        <KeyboardProvider>
        <QueryClientProvider client={queryClient}>
          <ThemeProvider>
            <StatusBar style="light" />
            <SignedInServicesGate />
            {/* Auto-lock (#899): captures touch starts to refresh the idle
                deadline and locks on background-elapsed / foreground-idle. */}
            <AutoLockProvider>
            <Stack
              screenOptions={{
                headerShown: false,
                contentStyle: { backgroundColor: semantic.bg },
                // Drilling further into a route (tab → group → channel) pushes
                // the new screen in from the right; back reverses it. Timing
                // lives in lib/transitions.
                ...drillIn,
              }}
            >
              {/* Boot router cuts in with nothing. Entering the tab container
                  fades — this is the Initializing → app handoff (and cold-boot
                  → app); tab switches *inside* (tabs) stay un-animated, handled
                  by the tab navigator. */}
              <Stack.Screen name="index" options={{ animation: "none" }} />
              <Stack.Screen name="(auth)" />
              <Stack.Screen name="(tabs)" options={{ animation: "fade" }} />
              <Stack.Screen name="group/[id]" />
              <Stack.Screen name="group/new" />
              <Stack.Screen name="group/invite" />
              <Stack.Screen name="group/members" />
              <Stack.Screen name="group/settings" />
              <Stack.Screen name="group/emoji" />
              <Stack.Screen name="group/requests" />
              <Stack.Screen name="group/discover" />
              <Stack.Screen name="dm/new" />
              <Stack.Screen name="dm/info" />
              <Stack.Screen name="dm/requests" />
              <Stack.Screen name="conversation/info" />
              <Stack.Screen name="chat/[id]" />
              <Stack.Screen name="chat/thread" />
              {/* Full-screen attachment viewer (#1248): a page, not a modal. */}
              <Stack.Screen name="media" />
              <Stack.Screen name="user/[id]" />
              <Stack.Screen name="report" options={settingsPage} />
              {/* Personal settings pages (settingsPage = drillIn since the
                  redesign, so the edge swipe pops them like any push). */}
              <Stack.Screen
                name="self/preferences"
                options={settingsPage}
              />
              <Stack.Screen
                name="self/user-settings"
                options={settingsPage}
              />
              <Stack.Screen
                name="self/security"
                options={settingsPage}
              />
              <Stack.Screen
                name="self/link-device"
                options={settingsPage}
              />
              <Stack.Screen
                name="self/blocked"
                options={settingsPage}
              />
              <Stack.Screen
                name="self/change-email"
                options={settingsPage}
              />
              <Stack.Screen
                name="self/saved"
                options={settingsPage}
              />
              <Stack.Screen name="m/[...permalink]" />
              <Stack.Screen
                name="self/delete-account"
                options={settingsPage}
              />
            </Stack>
            </AutoLockProvider>
          </ThemeProvider>
        </QueryClientProvider>
        </KeyboardProvider>
      </SafeAreaProvider>
    </GestureHandlerRootView>
  );
}
