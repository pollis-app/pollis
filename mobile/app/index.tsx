import { useEffect, useState } from "react";
import { useRouter } from "expo-router";
import { restoreSession } from "../hooks/queries/useAuth";
import { invoke } from "../lib/native";
import { useThemeRerender } from "../components/theme";
import { LaunchScreen } from "../components/LaunchScreen";

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

  // The native splash's glyph at the same size, so the handoff is seamless.
  return <LaunchScreen busy={!routed} />;
}
