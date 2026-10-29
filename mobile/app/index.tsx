import { useEffect, useState } from "react";
import { View, ActivityIndicator, Image } from "react-native";
import { useRouter } from "expo-router";
import { restoreSession } from "../hooks/queries/useAuth";
import { invoke } from "../lib/native";
import { semantic } from "../theme/tokens";
import { useThemeRerender } from "../components/theme";

interface UnlockStateSnapshot {
  pin_set: boolean;
  is_unlocked: boolean;
  last_active_user: string | null;
}

/** Boot router: where the first frame lands. Runs once. */
export default function Index() {
  useThemeRerender();
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
        backgroundColor: semantic.bg,
        alignItems: "center",
        justifyContent: "center",
      }}
    >
      {/* The native splash's glyph at the same size (app.json `imageWidth`), so the handoff is seamless. */}
      <Image
        source={require("../assets/splash-glyph.png")}
        style={{ width: 120, height: 120 }}
        accessibilityIgnoresInvertColors
      />
      {!routed ? (
        <ActivityIndicator
          color={semantic.accent}
          style={{ position: "absolute", top: "50%", marginTop: 96 }}
        />
      ) : null}
    </View>
  );
}
