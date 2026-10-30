import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { AppState, Linking, Platform } from "react-native";
import { Stack, router } from "expo-router";
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
import { ThemeProvider, useThemeRerender } from "../components/theme";
import { queryClient } from "../lib/queryClient";
import { initializeNativeBridge, invoke } from "../lib/native";
import { usePushNotifications } from "../hooks/usePushNotifications";
import { useInboxRealtime } from "../hooks/useInboxRealtime";
import { useEnrollmentApprovals } from "../hooks/useEnrollmentApprovals";
import { useUnreadSync } from "../hooks/queries/useUnread";
import { AutoLockProvider } from "../lib/autolock";
import i18n, { adoptUserLanguage, hydrateLanguage } from "../i18n";
import { appStore } from "../stores/appStore";
import { endSession, sweepStalePlaintext } from "../lib/session";
import { onDeviceRevoked, setSessionNotice } from "../lib/deviceRevoked";
import { getBuildInfo } from "../lib/buildInfo";
import {
  clientPlatformFor,
  getClientPolicy,
  onUpgradeRequired,
  applyCoreVerdict,
  launchWaitsForCheck,
  parseDevPolicyLink,
  setClientPolicy,
  subscribeClientPolicy,
  type ClientPolicyStatus,
} from "../lib/clientPolicy";
import { ClientBlockedScreen } from "../components/ClientBlockedScreen";
import { usePersistedFeed } from "../hooks/usePersistedFeed";

SplashScreen.preventAutoHideAsync();

// #1611: fetch the published minimum and record the verdict. `force` skips the core's 5-minute
// throttle (launch, Retry, a dev override). Never throws: a failure leaves the last verdict.
async function checkClientPolicy(force: boolean): Promise<void> {
  try {
    const status = await invoke<ClientPolicyStatus>("check_client_policy", { force });
    setClientPolicy(status);
  } catch (e) {
    console.warn("[client-policy] check failed:", e);
  }
}

// Dev builds only (Maestro): pollis://dev/client-policy?policy=…|url=…|clear=1.
async function applyDevPolicyLink(url: string | null): Promise<void> {
  const link = url ? parseDevPolicyLink(url) : null;
  if (!link || !(__DEV__ || getBuildInfo().channel === "dev")) {
    return;
  }
  try {
    if (link.kind === "clear") {
      await invoke("set_client_policy_override", {});
    } else {
      await invoke("set_client_policy_override", { url: link.url ?? null, policy: link.policy ?? null });
    }
  } catch (e) {
    console.warn("[client-policy] dev override refused:", e);
    return;
  }
  await checkClientPolicy(true);
}

// After an unblock the navigator mounts again; land it on the boot router (PIN / sign-in).
function BootReset() {
  useEffect(() => {
    const id = setTimeout(() => router.replace("/"), 0);
    return () => clearTimeout(id);
  }, []);
  return null;
}

// App-level signed-in services; each is a no-op until a user is signed in.
function SignedInServicesGate() {
  usePushNotifications();
  useInboxRealtime();
  useEnrollmentApprovals();
  useUnreadSync();
  const userId = useObserver(() => appStore.currentUser?.id ?? null);
  usePersistedFeed(userId);
  useEffect(() => {
    if (userId) {
      void adoptUserLanguage(userId);
    }
  }, [userId]);
  return null;
}

// Under ThemeProvider so the scene background follows a palette change (#1447).
function RootStack({ children }: { children: React.ReactNode }) {
  useThemeRerender();
  return (
    <Stack
      screenOptions={{
        headerShown: false,
        contentStyle: { backgroundColor: semantic.bg },
        ...drillIn,
      }}
    >
      {children}
    </Stack>
  );
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
  const [policyReady, setPolicyReady] = useState(false);
  const policy = useSyncExternalStore(subscribeClientPolicy, getClientPolicy);
  const blocked = policy?.blocked === true;
  // Set once a block lifts, so the navigator that mounts again re-runs boot routing.
  const wasBlocked = useRef(false);
  const [resetBoot, setResetBoot] = useState(false);
  useEffect(() => {
    if (blocked) {
      wasBlocked.current = true;
      setResetBoot(false);
    } else if (wasBlocked.current) {
      wasBlocked.current = false;
      setResetBoot(true);
    }
  }, [blocked]);

  // Sweep plaintext a killed run left on disk (#1256), once per process, before any session restores.
  useEffect(() => {
    sweepStalePlaintext().finally(() => setSweepReady(true));
  }, []);

  // Holding the splash for the stored language avoids an English first frame.
  useEffect(() => {
    hydrateLanguage().finally(() => setLanguageReady(true));
  }, []);

  // #1338: this device was revoked; end the session and say why on sign-in.
  useEffect(
    () =>
      onDeviceRevoked(async () => {
        // Act only if the core says THIS device is the revoked one.
        const revoked = await invoke<boolean>("is_current_device_revoked").catch(() => false);
        if (!revoked) {
          return;
        }
        console.warn("[session] this device was removed from the account — signing out");
        await invoke("logout", { deleteData: false }).catch((e) =>
          console.warn("[session] logout of a revoked device failed:", e),
        );
        setSessionNotice(i18n.t("auth:login.deviceRemoved"));
        await endSession();
      }),
    [],
  );

  // #1611: an `upgrade_required` asks the core; only its own `blocked: true` verdict blocks, so a
  // late or stray rejection after an unblock never re-blocks.
  useEffect(
    () =>
      onUpgradeRequired(async () => {
        applyCoreVerdict(await invoke<ClientPolicyStatus>("get_client_policy_status"));
      }),
    [],
  );

  useEffect(() => {
    const build = getBuildInfo();
    initializeNativeBridge({
      r2Endpoint: process.env.EXPO_PUBLIC_R2_ENDPOINT,
      r2PublicUrl: process.env.EXPO_PUBLIC_R2_PUBLIC_URL,
      livekitUrl: process.env.EXPO_PUBLIC_LIVEKIT_URL,
      pollisDeliveryUrl: process.env.EXPO_PUBLIC_POLLIS_DELIVERY_URL,
      // Dev builds only (#1242).
      devAccessKey: process.env.EXPO_PUBLIC_POLLIS_DEV_ACCESS_KEY,
      // #1611: the store binary's own version, never the OTA bundle's.
      clientPlatform: clientPlatformFor(Platform.OS === "ios" ? "ios" : "android", build.channel),
      clientVersion: build.version,
    })
      // Before unlock or sign-in: the minimum-version check. A cache that already allows this
      // build lets launch go on and checks in the background; a cached block or no cache waits
      // (the core bounds the fetch to a few seconds).
      .then(async () => {
        const cached = await invoke<ClientPolicyStatus>("peek_client_policy").catch(() => null);
        if (launchWaitsForCheck(cached)) {
          await checkClientPolicy(true);
        } else {
          void checkClientPolicy(true);
        }
        await applyDevPolicyLink(await Linking.getInitialURL().catch(() => null));
      })
      .then(() => setBridgeReady(true))
      .catch((e) => {
        console.error("[bridge] initializeNativeBridge failed:", e);
        setBridgeError(e);
        setBridgeReady(true);
      })
      .finally(() => setPolicyReady(true));
  }, []);

  // Re-check on the way back to the foreground (throttled in the core); no timers.
  useEffect(() => {
    if (!policyReady || bridgeError) {
      return;
    }
    const sub = AppState.addEventListener("change", (next) => {
      if (next === "active") {
        void checkClientPolicy(false);
      }
    });
    const links = Linking.addEventListener("url", ({ url }) => void applyDevPolicyLink(url));
    return () => {
      sub.remove();
      links.remove();
    };
  }, [policyReady, bridgeError]);

  useEffect(() => {
    if (loaded && bridgeReady && languageReady && sweepReady && policyReady) {
      SplashScreen.hideAsync();
    }
  }, [loaded, bridgeReady, languageReady, sweepReady, policyReady]);

  if (!loaded || !bridgeReady || !languageReady || !sweepReady || !policyReady) {
    return null;
  }

  // Non-fatal here: the auth screen surfaces a clearer error.
  void bridgeError;

  return (
    <GestureHandlerRootView style={{ flex: 1, backgroundColor: semantic.bg }}>
      <SafeAreaProvider>
        {/* Tracks the soft keyboard (#1246). */}
        <KeyboardProvider>
        <QueryClientProvider client={queryClient}>
          <ThemeProvider>
            <StatusBar style="light" />
            {/* #1611: a blocked build sees only this; no navigator, nothing behind it. */}
            {blocked && policy ? (
              <ClientBlockedScreen status={policy} onRetry={() => checkClientPolicy(true)} />
            ) : (
            <>
            <SignedInServicesGate />
            {/* Auto-lock (#899). */}
            <AutoLockProvider>
            <RootStack>
              {/* Entering the tab container fades (the Initializing handoff). */}
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
              <Stack.Screen name="dm/safety" />
              <Stack.Screen name="dm/requests" />
              <Stack.Screen name="conversation/info" />
              <Stack.Screen name="conversation/pins" />
              <Stack.Screen name="chat/[id]" />
              <Stack.Screen name="chat/thread" />
              <Stack.Screen name="media" />
              <Stack.Screen name="vault" />
              <Stack.Screen name="user/[id]" />
              <Stack.Screen name="report" options={settingsPage} />
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
                name="self/security-events"
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
              <Stack.Screen
                name="self/diagnostics"
                options={settingsPage}
              />
              <Stack.Screen name="m/[...permalink]" />
              <Stack.Screen name="dev/client-policy" options={{ animation: "none" }} />
              <Stack.Screen name="approve-device" options={settingsPage} />
              <Stack.Screen
                name="self/delete-account"
                options={settingsPage}
              />
            </RootStack>
            {resetBoot ? <BootReset /> : null}
            </AutoLockProvider>
            </>
            )}
          </ThemeProvider>
        </QueryClientProvider>
        </KeyboardProvider>
      </SafeAreaProvider>
    </GestureHandlerRootView>
  );
}
