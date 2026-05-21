import { useEffect, useState } from "react";
import { View, ActivityIndicator } from "react-native";
import { useRouter } from "expo-router";
import { restoreSession } from "../hooks/queries/useAuth";
import { invoke } from "../lib/native";
import { palette, semantic } from "../theme/tokens";

interface UnlockStateSnapshot {
  pin_set: boolean;
  is_unlocked: boolean;
  last_active_user: string | null;
}

/**
 * Boot router. Decides where the first frame lands:
 *   - signed-out  → /(auth)/email
 *   - signed-in, locked, PIN set → /(auth)/pin (unlock flow)
 *   - signed-in, no PIN set (first run on this device) → /(auth)/pin (create)
 *   - signed-in and already unlocked → /(tabs)/groups
 *
 * Runs once; downstream screens own the rest of the flow.
 */
export default function Index() {
  const router = useRouter();
  const [routed, setRouted] = useState(false);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const profile = await restoreSession();
        if (cancelled) {
          return;
        }
        if (!profile) {
          router.replace("/(auth)/email");
          return;
        }
        // Have a session — find out where to land based on unlock state.
        const snap = await invoke<UnlockStateSnapshot>("get_unlock_state");
        if (cancelled) {
          return;
        }
        if (snap.is_unlocked) {
          router.replace("/(tabs)/groups");
        } else {
          router.replace("/(auth)/pin");
        }
      } catch (e) {
        console.warn("[index] boot routing failed:", e);
        if (!cancelled) {
          router.replace("/(auth)/email");
        }
      } finally {
        if (!cancelled) {
          setRouted(true);
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [router]);

  return (
    <View
      style={{
        flex: 1,
        backgroundColor: palette.bg,
        alignItems: "center",
        justifyContent: "center",
      }}
    >
      {!routed ? (
        <ActivityIndicator color={semantic.accent} />
      ) : null}
    </View>
  );
}
