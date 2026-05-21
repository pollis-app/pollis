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

SplashScreen.preventAutoHideAsync();

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
      r2AccessKeyId: process.env.EXPO_PUBLIC_R2_ACCESS_KEY_ID,
      r2SecretAccessKey: process.env.EXPO_PUBLIC_R2_SECRET_KEY,
      r2PublicUrl: process.env.EXPO_PUBLIC_R2_PUBLIC_URL,
      livekitUrl: process.env.EXPO_PUBLIC_LIVEKIT_URL,
      livekitApiKey: process.env.EXPO_PUBLIC_LIVEKIT_API_KEY,
      livekitApiSecret: process.env.EXPO_PUBLIC_LIVEKIT_API_SECRET,
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
            <Stack
              screenOptions={{
                headerShown: false,
                contentStyle: { backgroundColor: palette.bg },
                animation: "fade",
              }}
            >
              <Stack.Screen name="index" />
              <Stack.Screen name="(auth)" />
              <Stack.Screen name="(tabs)" />
              <Stack.Screen name="group/[id]" />
              <Stack.Screen name="chat/[id]" />
              <Stack.Screen name="self/preferences" />
              <Stack.Screen name="self/user-settings" />
              <Stack.Screen name="self/security" />
            </Stack>
          </ThemeProvider>
        </QueryClientProvider>
      </SafeAreaProvider>
    </GestureHandlerRootView>
  );
}
