import { useCallback, useEffect } from "react";
import { useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useAppStore } from "../../stores/appStore";
import {
  applyAccentColor,
  applyBackgroundColor,
  applyFontSize,
  loadDeviceFontSize,
  saveDeviceFontSize,
} from "../../utils/colorUtils";

/**
 * Mirrors `voice_apm::NsLevel` in src-tauri.
 */
export type NoiseSuppressionLevel = "off" | "low" | "moderate" | "high";

export interface PreferencesData {
  accent_color?: string;
  background_color?: string;
  /**
   * Legacy: font size used to be synced via the remote preferences blob.
   * It is now device-local (see `loadDeviceFontSize` / `saveDeviceFontSize`
   * in `colorUtils.ts`). This field is kept on the read path solely so
   * existing remote rows can seed the device-local value once on first
   * boot — it is never written back.
   */
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
  /** RNNoise click/keystroke suppression (separate from APM's spectral NS). */
  click_suppression?: boolean;
  auto_join_voice?: boolean;
  /** Whether the left sidebar is open by default at app start. */
  sidebar_open_by_default?: boolean;
  /**
   * Per-remote-user output volume multipliers, keyed by `user_id`.
   * Range 0.0..=2.0; 1.0 is unity. Absent users default to unity.
   *
   * NOTE (#140 — multi-device voice): when the same user can join a
   * voice channel from multiple devices, decide whether to keep this
   * user-scoped or shift to per-device. See `user_id_from_voice_identity`
   * in `src-tauri/src/commands/voice.rs`.
   */
  user_volumes?: { [userId: string]: number };
}

/**
 * Strip the LiveKit voice-channel identity wrapper down to the bare
 * `user_id`. Mirrors `user_id_from_voice_identity` in the Rust voice
 * module — keep the two in sync.
 */
export function userIdFromVoiceIdentity(identity: string): string {
  const stripped = identity.startsWith("voice-")
    ? identity.slice("voice-".length)
    : identity;
  const colon = stripped.indexOf(":");
  return colon >= 0 ? stripped.slice(0, colon) : stripped;
}

/** Volume slider range used by the per-remote-user output volume control. */
export const REMOTE_USER_VOLUME_MIN = 0.0;
export const REMOTE_USER_VOLUME_MAX = 2.0;
export const REMOTE_USER_VOLUME_DEFAULT = 1.0;

/** Clamp a remote-user volume to the supported range. NaN → unity. */
export function clampRemoteUserVolume(v: number): number {
  if (!Number.isFinite(v)) {
    return REMOTE_USER_VOLUME_DEFAULT;
  }
  return Math.max(REMOTE_USER_VOLUME_MIN, Math.min(REMOTE_USER_VOLUME_MAX, v));
}

/** Defaults must match `voice_apm::ApmConfig::default` in src-tauri. */
export const APM_DEFAULTS = {
  mic_boost_db: 0,
  auto_gain_control: true,
  agc_target_dbfs: 6,
  noise_suppression_level: "high" as NoiseSuppressionLevel,
  echo_cancellation: true,
  click_suppression: false,
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
  click_suppression: boolean;
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
    click_suppression: prefs?.click_suppression ?? APM_DEFAULTS.click_suppression,
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

// Remote write throttle window. The local React Query cache update + any
// callsite-side live effects (CSS vars, mixer state) fire on every call;
// only the actual `invoke("save_preferences")` is rate-limited. Leading +
// trailing edges always fire; sustained calls during the window collapse
// to one intermediate fire every SAVE_THROTTLE_MS.
const SAVE_THROTTLE_MS = 500;

interface PendingSave {
  userId: string;
  prefs: PreferencesData;
}

let pendingSave: PendingSave | null = null;
let saveThrottleTimer: ReturnType<typeof setTimeout> | null = null;
let lastSaveInvokeAt = 0;

async function flushPendingSave(): Promise<void> {
  if (saveThrottleTimer !== null) {
    clearTimeout(saveThrottleTimer);
    saveThrottleTimer = null;
  }
  const job = pendingSave;
  pendingSave = null;
  if (!job) {
    return;
  }
  lastSaveInvokeAt = performance.now();
  try {
    await invoke("save_preferences", {
      userId: job.userId,
      preferencesJson: JSON.stringify(job.prefs),
    });
  } catch (e) {
    console.warn("[prefs] save_preferences failed", e);
  }
}

function scheduleSave(
  userId: string,
  prefs: PreferencesData,
  queryClient: QueryClient,
): void {
  queryClient.setQueryData(prefsKey(userId), prefs);
  pendingSave = { userId, prefs };

  const elapsed = performance.now() - lastSaveInvokeAt;
  if (elapsed >= SAVE_THROTTLE_MS) {
    void flushPendingSave();
    return;
  }
  if (saveThrottleTimer === null) {
    saveThrottleTimer = setTimeout(() => {
      saveThrottleTimer = null;
      void flushPendingSave();
    }, SAVE_THROTTLE_MS - elapsed);
  }
}

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
        click_suppression: getPreference<boolean>(json, "click_suppression", APM_DEFAULTS.click_suppression),
        auto_join_voice: getPreference<boolean>(json, "auto_join_voice", false),
        sidebar_open_by_default: getPreference<boolean>(json, "sidebar_open_by_default", true),
        user_volumes: getPreference<{ [userId: string]: number } | undefined>(
          json,
          "user_volumes",
          undefined,
        ),
      };
    },
    enabled: !!currentUser,
    staleTime: 1000 * 60 * 5,
  });

  const save = useCallback(
    (prefs: PreferencesData) => {
      if (!currentUser) {
        return;
      }
      scheduleSave(currentUser.id, prefs, queryClient);
    },
    [currentUser, queryClient],
  );

  return { query, save };
}

/**
 * Apply loaded preferences (accent_color, background_color) to CSS vars.
 * Call this once after the preferences query resolves.
 *
 * Font size is device-local — it is NOT applied from `prefs` here. The
 * device-local value (or one-time seed from the legacy remote field) is
 * applied separately via `applyDeviceFontSize`.
 */
export function applyPreferences(prefs: PreferencesData): void {
  if (prefs.accent_color) {
    applyAccentColor(prefs.accent_color);
  }
  if (prefs.background_color) {
    applyBackgroundColor(prefs.background_color);
  }
}

/**
 * Apply the device-local font size for `userId`. If localStorage has no
 * value yet but the legacy remote `prefs.font_size` is present, seed
 * localStorage from it once and apply it — this preserves the user's
 * existing setting through the migration to per-device storage. After
 * the seed, the remote field is ignored.
 */
export function applyDeviceFontSize(
  userId: string | null | undefined,
  prefs?: PreferencesData,
): void {
  const local = loadDeviceFontSize(userId);
  if (local !== null) {
    applyFontSize(local);
    return;
  }
  if (prefs?.font_size) {
    const px = parseInt(prefs.font_size, 10);
    if (!isNaN(px) && px >= 10 && px <= 28) {
      saveDeviceFontSize(userId, px);
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
 *
 * Also handles the one-time seed of device-local font size from a legacy
 * remote `font_size` field — see `applyDeviceFontSize`.
 */
export function useApplyPreferences(): void {
  const { query } = usePreferences();
  const currentUser = useAppStore((state) => state.currentUser);
  const data = query.data;
  useEffect(() => {
    if (data) {
      applyPreferences(data);
      applyDeviceFontSize(currentUser?.id, data);
    }
  }, [data?.accent_color, data?.background_color, data?.font_size, currentUser?.id]);
}
