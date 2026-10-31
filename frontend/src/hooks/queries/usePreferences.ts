import { useCallback, useEffect } from "react";
import { useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import { invoke, setTrayCloseToTray, setTrayEnabled } from "../../bridge";
import { appStore } from "../../stores/appStore";
import { useObserver } from "mobx-react-lite";
import {
  applyAccentColor,
  applyBackgroundColor,
  applyFontSize,
  applySkin,
  normalizeSkin,
  loadDeviceFontSize,
  saveDeviceFontSize,
  type Skin,
} from "../../utils/colorUtils";
import {
  setShortcutOverrides,
  type ShortcutCommandId,
} from "../../keyboard";
import {
  VOICE_INPUT_MODE_DEFAULT,
  type VoiceInputMode,
} from "../../types/voice-state";
import { voiceSession } from "../../voice";
import {
  INCOMING_CALL_TAKEOVER_DEFAULT,
  INCOMING_CALL_TAKEOVER_PREF,
} from "../../utils/incomingCallTakeover";
import {
  mergePreferencesSave,
  parsePreferencesBlob,
  preferencesSaveJson,
  type MergedSave,
  type PreferencesBlob,
} from "../../utils/preferencesBlob";

/**
 * Mirrors `voice_apm::NsLevel` in src-tauri.
 */
export type NoiseSuppressionLevel = "off" | "low" | "moderate" | "high";

/** Relay overlay mode (`pollis_relay::OverlayMode`); `strict` pauses sending rather than reveal the client IP. */
export type OverlayMode = "off" | "prefer" | "strict";

/** Coerce an arbitrary string to a known `OverlayMode`; unknown/absent → `off`. */
export function normalizeOverlayMode(v: string | undefined | null): OverlayMode {
  return v === "prefer" || v === "strict" ? v : "off";
}

/** Coerce to a known `VoiceInputMode`; unknown/absent → `voice_activity`. */
export function normalizeVoiceInputMode(v: string | undefined | null): VoiceInputMode {
  return v === "push_to_talk" ? v : VOICE_INPUT_MODE_DEFAULT;
}

export interface PreferencesData {
  accent_color?: string;
  background_color?: string;
  /** UI skin, synced across devices. Absent → terminal. */
  skin?: Skin;
  /** Legacy: read only to seed the device-local font size once; never written. */
  font_size?: string;
  allow_desktop_notifications?: boolean;
  allow_sound_effects?: boolean;
  /** #1639: a ringing DM call takes over the screen (`/incoming-call/$callId`). Default true; synced. */
  incoming_call_takeover?: boolean;
  /** DM receipts (#857), default true. Reciprocal: off emits and records none. Enforced in Rust; synced. */
  send_read_receipts?: boolean;
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
  /** Screen-share fps ceiling, one of `SCREEN_SHARE_FPS_OPTIONS`. */
  screen_share_max_fps?: number;
  /** Share machine sound; stored because Linux has no in-app picker. Absent → off. */
  screen_share_audio?: boolean;
  auto_join_voice?: boolean;
  /** Whether the left sidebar is open by default at app start. */
  sidebar_open_by_default?: boolean;
  /** Tri-state (#824): `undefined` follows the skin; a boolean holds across skins. */
  right_panel_open_by_default?: boolean;
  /** Linux/Windows: closing hides to the tray. Default true. */
  close_to_tray?: boolean;
  /** macOS menu-bar status item. Default false (opt-in). */
  menubar_icon?: boolean;
  /** Clear OS media permissions on quit (macOS `tccutil reset`). Default false. */
  revoke_media_on_exit?: boolean;
  /** Per-remote-user output volume, keyed by `user_id`; 0.0..=2.0, absent = 1.0. */
  user_volumes?: { [userId: string]: number };
  /** Shortcut overrides by `ShortcutCommandId`, as `keyboard/keyCombo.ts` combo strings. */
  shortcut_overrides?: { [commandId: string]: string };
  /** Mic gating (#849), pushed to the Rust gate on load and change. Absent → `voice_activity`. */
  voice_input_mode?: VoiceInputMode;
  /** Relay overlay mode (#455). Absent → `off`. */
  overlay_mode?: OverlayMode;
  /** Consent to relay for others (#813 §10.2); separate from `overlay_mode`, never defaulted on. */
  relay_serving?: boolean;
  /** Relay only on unmetered links. Absent → `true`. */
  relay_serving_wifi_only?: boolean;
  /** Relay only on external power. Absent → `true`. */
  relay_serving_power_only?: boolean;
}

/** Screen-share framerate presets, in fps. */
export const SCREEN_SHARE_FPS_OPTIONS = [15, 30, 60] as const;
export const SCREEN_SHARE_FPS_DEFAULT = 30;

/** Shared audio is opt-in. See `screen_share_audio`. */
export const SCREEN_SHARE_AUDIO_DEFAULT = false;

/** Snap an arbitrary value to the nearest allowed preset; default if invalid. */
export function clampScreenShareFps(v: number | undefined): number {
  if (v === undefined || !Number.isFinite(v)) {
    return SCREEN_SHARE_FPS_DEFAULT;
  }
  return SCREEN_SHARE_FPS_OPTIONS.reduce((best, opt) =>
    Math.abs(opt - v) < Math.abs(best - v) ? opt : best,
  );
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

/** `voice_apm::ApmConfig`; field names are wire-format. */
export interface ApmConfig {
  mic_boost_db: number;
  agc_enabled: boolean;
  agc_target_dbfs: number;
  ns_level: NoiseSuppressionLevel;
  aec_enabled: boolean;
  click_suppression: boolean;
}

/** Project voice prefs onto the APM config, defaulting undefined fields. */
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

/** Read a typed key from a preferences JSON string, else `defaultValue`. */
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
  }
  return defaultValue;
}

const prefsKey = (userId: string | null) => ["user", "preferences", userId] as const;

// Throttles only the remote `save_preferences`; leading and trailing edges always fire.
const SAVE_THROTTLE_MS = 500;

interface PendingSave {
  userId: string;
  merged: MergedSave;
}

// The raw stored blob per user, keys desktop does not parse included (#1650): every save
// merges into it, so mobile's keys survive a desktop save.
const rawBlobs = new Map<string, PreferencesBlob>();

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
      preferencesJson: preferencesSaveJson(job.merged),
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
  // Deletes from a save still waiting on the throttle ride along with this one.
  const carried = pendingSave?.userId === userId ? pendingSave.merged.deleted : undefined;
  const merged = mergePreferencesSave(rawBlobs.get(userId) ?? {}, { ...prefs }, carried);
  rawBlobs.set(userId, merged.blob);
  pendingSave = { userId, merged };

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
  const currentUser = useObserver(() => appStore.currentUser);
  const queryClient = useQueryClient();

  const query = useQuery({
    queryKey: prefsKey(currentUser?.id ?? null),
    queryFn: async (): Promise<PreferencesData> => {
      if (!currentUser) {
        return {};
      }
      const json = await invoke<string>("get_preferences", { userId: currentUser.id });
      rawBlobs.set(currentUser.id, parsePreferencesBlob(json));
      return {
        accent_color: getPreference<string | undefined>(json, "accent_color", undefined),
        background_color: getPreference<string | undefined>(json, "background_color", undefined),
        skin: normalizeSkin(getPreference<string | undefined>(json, "skin", undefined)),
        font_size: getPreference<string | undefined>(json, "font_size", undefined),
        allow_desktop_notifications: getPreference<boolean>(json, "allow_desktop_notifications", false),
        allow_sound_effects: getPreference<boolean>(json, "allow_sound_effects", true),
        incoming_call_takeover: getPreference<boolean>(
          json,
          INCOMING_CALL_TAKEOVER_PREF,
          INCOMING_CALL_TAKEOVER_DEFAULT,
        ),
        send_read_receipts: getPreference<boolean>(json, "send_read_receipts", true),
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
        screen_share_max_fps: clampScreenShareFps(
          getPreference<number>(json, "screen_share_max_fps", SCREEN_SHARE_FPS_DEFAULT),
        ),
        screen_share_audio: getPreference<boolean>(
          json,
          "screen_share_audio",
          SCREEN_SHARE_AUDIO_DEFAULT,
        ),
        auto_join_voice: getPreference<boolean>(json, "auto_join_voice", false),
        // Must be read back, or a refetch reverts the gate out of push-to-talk.
        voice_input_mode: normalizeVoiceInputMode(
          getPreference<string | undefined>(json, "voice_input_mode", undefined),
        ),
        sidebar_open_by_default: getPreference<boolean>(json, "sidebar_open_by_default", true),
        // `undefined` = "follow the skin".
        right_panel_open_by_default: getPreference<boolean | undefined>(
          json,
          "right_panel_open_by_default",
          undefined,
        ),
        close_to_tray: getPreference<boolean>(json, "close_to_tray", true),
        menubar_icon: getPreference<boolean>(json, "menubar_icon", false),
        revoke_media_on_exit: getPreference<boolean>(json, "revoke_media_on_exit", false),
        user_volumes: getPreference<{ [userId: string]: number } | undefined>(
          json,
          "user_volumes",
          undefined,
        ),
        shortcut_overrides: getPreference<{ [commandId: string]: string } | undefined>(
          json,
          "shortcut_overrides",
          undefined,
        ),
        overlay_mode: normalizeOverlayMode(
          getPreference<string | undefined>(json, "overlay_mode", undefined),
        ),
        relay_serving: getPreference<boolean>(json, "relay_serving", false),
        relay_serving_wifi_only: getPreference<boolean>(json, "relay_serving_wifi_only", true),
        relay_serving_power_only: getPreference<boolean>(json, "relay_serving_power_only", true),
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

/** Current skin, for structural branches; pure styling should use `:root[data-skin]` CSS. */
export function useSkin(): Skin {
  const { query } = usePreferences();
  return normalizeSkin(query.data?.skin);
}

/** Send-receipts preference (#857), default true; Rust enforces it. */
export function useSendReadReceipts(): boolean {
  const { query } = usePreferences();
  return query.data?.send_read_receipts ?? true;
}

/** Apply synced visual prefs to CSS vars; font size is device-local (`applyDeviceFontSize`). */
export function applyPreferences(prefs: PreferencesData): void {
  if (prefs.accent_color) {
    applyAccentColor(prefs.accent_color);
  }
  if (prefs.background_color) {
    applyBackgroundColor(prefs.background_color);
  }
  applySkin(normalizeSkin(prefs.skin));
  setShortcutOverrides(
    (prefs.shortcut_overrides ?? {}) as Partial<Record<ShortcutCommandId, string>>,
  );
  // On load too, so a synced push-to-talk applies on a fresh device.
  void voiceSession.setInputMode(prefs.voice_input_mode ?? VOICE_INPUT_MODE_DEFAULT);
}

/** Apply the device-local font size, seeding it once from legacy `prefs.font_size`. */
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

/** Re-apply visual prefs whenever the query data changes; mounted once at the root. */
export function useApplyPreferences(): void {
  const { query } = usePreferences();
  const currentUser = useObserver(() => appStore.currentUser);
  const data = query.data;
  const overridesKey = JSON.stringify(data?.shortcut_overrides ?? {});
  useEffect(() => {
    if (data) {
      applyPreferences(data);
      applyDeviceFontSize(currentUser?.id, data);
    }
  }, [data?.accent_color, data?.background_color, data?.skin, data?.font_size, overridesKey, currentUser?.id]);

  // Pushed so the close handler picks hide-vs-quit synchronously.
  const closeToTray = data?.close_to_tray ?? true;
  useEffect(() => {
    void setTrayCloseToTray(closeToTray).catch((err) => {
      console.warn("[tray] setTrayCloseToTray failed:", err);
    });
  }, [closeToTray]);

  const menubarIcon = data?.menubar_icon ?? false;
  useEffect(() => {
    void setTrayEnabled(menubarIcon).catch((err) => {
      console.warn("[tray] setTrayEnabled failed:", err);
    });
  }, [menubarIcon]);

  const revokeMediaOnExit = data?.revoke_media_on_exit ?? false;
  useEffect(() => {
    void invoke("set_revoke_media_on_exit", { enabled: revokeMediaOnExit }).catch(
      (err) => {
        console.warn("[media-permissions] set_revoke_media_on_exit failed:", err);
      },
    );
  }, [revokeMediaOnExit]);
}
