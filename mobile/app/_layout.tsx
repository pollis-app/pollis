import { useEffect, useState } from "react";
import { Stack } from "expo-router";
import { StatusBar } from "expo-status-bar";
import * as SplashScreen from "expo-splash-screen";
import { SafeAreaProvider } from "react-native-safe-area-context";
import { GestureHandlerRootView } from "react-native-gesture-handler";
import {
  useFonts,
  Sora_400Regular,
  Sora_500Medium,
  Sora_600SemiBold,
  Sora_700Bold,
} from "@expo-google-fonts/sora";
import { QueryClientProvider } from "@tanstack/react-query";
import { palette } from "../theme/tokens";
import { ThemeProvider } from "../components/theme";
import { queryClient } from "../lib/queryClient";
import { initializeNativeBridge } from "../lib/native";
import { usePushNotifications } from "../hooks/usePushNotifications";
import { useInboxRealtime } from "../hooks/useInboxRealtime";

SplashScreen.preventAutoHideAsync();

// App-level signed-in services, mounted under the providers so they have a
// QueryClient + router and only run after the bridge is ready. Each hook is a
// no-op until a user is signed in: push installs notification listeners; inbox
// realtime keeps the groups/DM lists live while foregrounded.
function SignedInServicesGate() {
  usePushNotifications();
  useInboxRealtime();
  return null;
}

export default function RootLayout() {
  const [loaded] = useFonts({
    Sora_400Regular,
    Sora_500Medium,
    Sora_600SemiBold,
    Sora_700Bold,
  });
  const [bridgeReady, setBridgeReady] = useState(false);
  const [bridgeError, setBridgeError] = useState<Error | null>(null);

  useEffect(() => {
    initializeNativeBridge({
      tursoUrl: process.env.EXPO_PUBLIC_TURSO_URL ?? "",
      tursoToken: process.env.EXPO_PUBLIC_TURSO_TOKEN ?? "",
      r2Endpoint: process.env.EXPO_PUBLIC_R2_ENDPOINT,
      r2PublicUrl: process.env.EXPO_PUBLIC_R2_PUBLIC_URL,
      livekitUrl: process.env.EXPO_PUBLIC_LIVEKIT_URL,
      resendApiKey: process.env.EXPO_PUBLIC_RESEND_API_KEY,
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
    if (loaded && bridgeReady) {
      SplashScreen.hideAsync();
    }
  }, [loaded, bridgeReady]);

  if (!loaded || !bridgeReady) {
    return null;
  }

  // Bridge errors are non-fatal at the layout level — the auth screen will
  // surface a clearer error when the user tries to sign in. We still log
  // above so device logs show what went wrong.
  void bridgeError;

  return (
    <GestureHandlerRootView style={{ flex: 1, backgroundColor: palette.bg }}>
      <SafeAreaProvider>
        <QueryClientProvider client={queryClient}>
          <ThemeProvider>
            <StatusBar style="light" />
            <SignedInServicesGate />
            <Stack
              screenOptions={{
                headerShown: false,
                contentStyle: { backgroundColor: palette.bg },
                // Drilling further into a route (tab → group → channel) pushes
                // the new screen in from the right; back reverses it. Settings
                // pages override this to slide up from the bottom (below).
                animation: "slide_from_right",
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
              <Stack.Screen name="group/requests" />
              <Stack.Screen name="group/discover" />
              <Stack.Screen name="dm/new" />
              <Stack.Screen name="dm/info" />
              <Stack.Screen name="chat/[id]" />
              <Stack.Screen name="user/[id]" />
              {/* Personal settings pages pop up from the bottom (pushing the
                  current screen off), and reverse on back — a full-screen push,
                  not a bottom-sheet overlay. */}
              <Stack.Screen
                name="self/preferences"
                options={{ animation: "slide_from_bottom" }}
              />
              <Stack.Screen
                name="self/user-settings"
                options={{ animation: "slide_from_bottom" }}
              />
              <Stack.Screen
                name="self/security"
                options={{ animation: "slide_from_bottom" }}
              />
              <Stack.Screen
                name="self/blocked"
                options={{ animation: "slide_from_bottom" }}
              />
              <Stack.Screen
                name="self/change-email"
                options={{ animation: "slide_from_bottom" }}
              />
            </Stack>
          </ThemeProvider>
        </QueryClientProvider>
      </SafeAreaProvider>
    </GestureHandlerRootView>
  );
}
