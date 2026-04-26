import { useEffect } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useAppStore } from "../../stores/appStore";
import { applyAccentColor, applyBackgroundColor, applyFontSize } from "../../utils/colorUtils";

/**
 * Mirrors `voice_apm::NsLevel` in src-tauri.
 */
export type NoiseSuppressionLevel = "off" | "low" | "moderate" | "high";

export interface PreferencesData {
  accent_color?: string;
  background_color?: string;
  font_size?: string;
  allow_desktop_notifications?: boolean;
  allow_sound_effects?: boolean;
  /** Pre-AGC mic boost in dB. 0..=20; 0 = off. */
  mic_boost_db?: number;
  auto_gain_control?: boolean;
  /** AGC target loudness (dB headroom from full scale). Smaller = louder. Range 3..=15. */
  agc_target_dbfs?: number;
  noise_suppression_level?: NoiseSuppressionLevel;
  /** Acoustic echo cancellation. */
  echo_cancellation?: boolean;
  auto_join_voice?: boolean;
}

/** Defaults must match `voice_apm::ApmConfig::default` in src-tauri. */
export const APM_DEFAULTS = {
  mic_boost_db: 0,
  auto_gain_control: true,
  agc_target_dbfs: 6,
  noise_suppression_level: "high" as NoiseSuppressionLevel,
  echo_cancellation: true,
} as const;

/**
 * The Rust-side `voice_apm::ApmConfig` shape. Sent over the IPC for both
 * `join_voice_channel` and the live `set_voice_audio_processing` command.
 * Field names are wire-format (no camelCase rewrite — they're inside an
 * object argument, not top-level invoke params).
 */
export interface ApmConfig {
  mic_boost_db: number;
  agc_enabled: boolean;
  agc_target_dbfs: number;
  ns_level: NoiseSuppressionLevel;
  aec_enabled: boolean;
}

/**
 * Project the user-facing voice prefs onto the APM config the backend
 * expects. Falls back to defaults whenever a pref is undefined so a
 * partially-loaded preferences row never produces NaNs or bad enum values.
 */
export function preferencesToApmConfig(prefs: PreferencesData | undefined): ApmConfig {
  return {
    mic_boost_db: clampMicBoost(prefs?.mic_boost_db ?? APM_DEFAULTS.mic_boost_db),
    agc_enabled: prefs?.auto_gain_control ?? APM_DEFAULTS.auto_gain_control,
    agc_target_dbfs: clampAgcTarget(prefs?.agc_target_dbfs ?? APM_DEFAULTS.agc_target_dbfs),
    ns_level: prefs?.noise_suppression_level ?? APM_DEFAULTS.noise_suppression_level,
    aec_enabled: prefs?.echo_cancellation ?? APM_DEFAULTS.echo_cancellation,
  };
}

/** AGC target is exposed in 3..=15 dB and the backend clamps the same. */
function clampAgcTarget(v: number): number {
  if (!Number.isFinite(v)) {
    return APM_DEFAULTS.agc_target_dbfs;
  }
  return Math.max(3, Math.min(15, Math.round(v)));
}

function clampMicBoost(v: number): number {
  if (!Number.isFinite(v)) {
    return APM_DEFAULTS.mic_boost_db;
  }
  return Math.max(0, Math.min(20, Math.round(v)));
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
        allow_sound_effects: getPreference<boolean>(json, "allow_sound_effects", true),
        mic_boost_db: getPreference<number>(json, "mic_boost_db", APM_DEFAULTS.mic_boost_db),
        auto_gain_control: getPreference<boolean>(json, "auto_gain_control", APM_DEFAULTS.auto_gain_control),
        agc_target_dbfs: getPreference<number>(json, "agc_target_dbfs", APM_DEFAULTS.agc_target_dbfs),
        noise_suppression_level: getPreference<NoiseSuppressionLevel>(
          json,
          "noise_suppression_level",
          APM_DEFAULTS.noise_suppression_level,
        ),
        echo_cancellation: getPreference<boolean>(json, "echo_cancellation", APM_DEFAULTS.echo_cancellation),
        auto_join_voice: getPreference<boolean>(json, "auto_join_voice", false),
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

/**
 * Subscribe to the preferences query and re-apply visual prefs to CSS vars
 * whenever the data arrives or changes. Mounted once near the app root so
 * both the login path and the app-reopen path (stored session) end up
 * applying CSS via the same mechanism, without a one-shot invoke in the
 * signed-in flow that could silently fail.
 */
export function useApplyPreferences(): void {
  const { query } = usePreferences();
  const data = query.data;
  useEffect(() => {
    if (data) {
      applyPreferences(data);
    }
  }, [data?.accent_color, data?.background_color, data?.font_size]);
}
