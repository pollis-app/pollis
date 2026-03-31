import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useAppStore } from "../../stores/appStore";
import { applyAccentColor, applyBackgroundColor, applyFontSize } from "../../utils/colorUtils";

export interface PreferencesData {
  accent_color?: string;
  background_color?: string;
  font_size?: string;
  allow_desktop_notifications?: boolean;
  auto_gain_control?: boolean;
}

/**
 * Safely read a typed value from a preferences JSON string.
 * Returns defaultValue if the JSON is malformed or the key is absent.
 */
export function getPreference<T>(json: string, key: string, defaultValue: T): T {
  try {
    const parsed: unknown = JSON.parse(json);
    if (parsed !== null && typeof parsed === "object" && key in (parsed as Record<string, unknown>)) {
      const val = (parsed as Record<string, unknown>)[key];
      if (val !== undefined && val !== null) {
        return val as T;
      }
    }
  } catch {
    // Malformed JSON — fall through to default
  }
  return defaultValue;
}

const prefsKey = (userId: string | null) => ["user", "preferences", userId] as const;

export function usePreferences() {
  const currentUser = useAppStore((state) => state.currentUser);
  const queryClient = useQueryClient();

  const query = useQuery({
    queryKey: prefsKey(currentUser?.id ?? null),
    queryFn: async (): Promise<PreferencesData> => {
      if (!currentUser) {
        return {};
      }
      const json = await invoke<string>("get_preferences", { userId: currentUser.id });
      return {
        accent_color: getPreference<string | undefined>(json, "accent_color", undefined),
        background_color: getPreference<string | undefined>(json, "background_color", undefined),
        font_size: getPreference<string | undefined>(json, "font_size", undefined),
        allow_desktop_notifications: getPreference<boolean>(json, "allow_desktop_notifications", false),
        auto_gain_control: getPreference<boolean>(json, "auto_gain_control", true),
      };
    },
    enabled: !!currentUser,
    staleTime: 1000 * 60 * 5,
  });

  const mutation = useMutation({
    mutationFn: async (prefs: PreferencesData) => {
      if (!currentUser) { return; }
      await invoke("save_preferences", {
        userId: currentUser.id,
        preferencesJson: JSON.stringify(prefs),
      });
      return prefs;
    },
    onSuccess: (prefs) => {
      if (prefs) {
        queryClient.setQueryData(prefsKey(currentUser?.id ?? null), prefs);
      }
    },
  });

  return { query, mutation };
}

/**
 * Apply loaded preferences (accent_color, font_size) to CSS vars.
 * Call this once after the preferences query resolves.
 */
export function applyPreferences(prefs: PreferencesData): void {
  if (prefs.accent_color) {
    applyAccentColor(prefs.accent_color);
  }
  if (prefs.background_color) {
    applyBackgroundColor(prefs.background_color);
  }
  if (prefs.font_size) {
    const px = parseInt(prefs.font_size, 10);
    if (!isNaN(px) && px >= 10 && px <= 28) {
      applyFontSize(px);
    }
  }
}
