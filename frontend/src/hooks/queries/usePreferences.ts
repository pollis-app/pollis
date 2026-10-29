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

/** Mirrors `voice_apm::NsLevel` in src-tauri. */
export type NoiseSuppressionLevel = "off" | "low" | "moderate" | "high";

/**
 * Relay overlay mode, mirroring `pollis_relay::OverlayMode`. `prefer` falls
 * back to direct; `strict` pauses sending rather than reveal the client IP.
 */
export type OverlayMode = "off" | "prefer" | "strict";

export function normalizeOverlayMode(v: string | undefined | null): OverlayMode {
  return v === "prefer" || v === "strict" ? v : "off";
}

export function normalizeVoiceInputMode(v: string | undefined | null): VoiceInputMode {
  return v === "push_to_talk" ? v : VOICE_INPUT_MODE_DEFAULT;
}

export interface PreferencesData {
  accent_color?: string;
  background_color?: string;
  /** `terminal` (default) or `refined`; synced across devices. */
  skin?: Skin;
  /** Legacy, read only: seeds the device-local font size once (`loadDeviceFontSize`). */
  font_size?: string;
  allow_desktop_notifications?: boolean;
  allow_sound_effects?: boolean;
  /**
   * DM delivery + read receipts (#857), default true. Reciprocal: while off the
   * device emits AND records none, delivery included (it still reveals "online
   * at T"). Enforced in Rust (`receipts.rs`); synced so it applies on every
   * device.
   */
  send_read_receipts?: boolean;
  /** Pre-AGC mic boost in dB. 0..=20; 0 = off. */
  mic_boost_db?: number;
  auto_gain_control?: boolean;
  /** AGC target loudness (dB below full scale); smaller = louder; 3..=15. */
  agc_target_dbfs?: number;
  noise_suppression_level?: NoiseSuppressionLevel;
  echo_cancellation?: boolean;
  /** RNNoise click/keystroke suppression (separate from APM's NS). */
  click_suppression?: boolean;
  /** Screen-share fps ceiling, one of `SCREEN_SHARE_FPS_OPTIONS`, read at share start. */
  screen_share_max_fps?: number;
  /**
   * Whether a screen share carries sound. Stored because Linux has no in-app
   * picker (the portal dialog is the picker); elsewhere it seeds the toggle.
   * Absent → off: sharing sound is always explicit.
   */
  screen_share_audio?: boolean;
  auto_join_voice?: boolean;
  sidebar_open_by_default?: boolean;
  /**
   * Right panel open by default (#824). Tri-state: `undefined` follows the skin;
   * a boolean holds across skins.
   */
  right_panel_open_by_default?: boolean;
  /** Linux/Windows: closing hides to the tray instead of exiting. Default true. */
  close_to_tray?: boolean;
  /** macOS: show a menu-bar status item. Default false (opt-in). */
  menubar_icon?: boolean;
  /**
   * Clear the OS media permissions on quit (macOS `tccutil reset`; nothing to
   * clear elsewhere), via `set_revoke_media_on_exit`. Default false.
   */
  revoke_media_on_exit?: boolean;
  /**
   * Per-remote-user output volume by `user_id`, 0.0..=2.0 (1.0 unity). User-
   * scoped, not per device (#140).
   */
  user_volumes?: { [userId: string]: number };
  /** Shortcut overrides by `ShortcutCommandId`, as `keyboard/keyCombo.ts` combos. */
  shortcut_overrides?: { [commandId: string]: string };
  /**
   * Mic gating (#849): `voice_activity` (default) or `push_to_talk`. Synced;
   * pushed to the Rust voice gate, which owns the state machine.
   */
  voice_input_mode?: VoiceInputMode;
  /** Relay overlay mode (#455), synced and applied live via `set_overlay_mode`. */
  overlay_mode?: OverlayMode;
  /**
   * Consent to relay FOR OTHER PEOPLE (#813 §10.2), independent of
   * `overlay_mode`. Opt-in only; never default it on.
   */
  relay_serving?: boolean;
  /** Relay only on unmetered (Wi-Fi) links. Absent → true (§10.2 / §11.6). */
  relay_serving_wifi_only?: boolean;
  /** Relay only on external power. Absent → true. */
  relay_serving_power_only?: boolean;
}

/** Screen-share fps presets: 15 documents, 30 standard, 60 motion. */
export const SCREEN_SHARE_FPS_OPTIONS = [15, 30, 60] as const;
export const SCREEN_SHARE_FPS_DEFAULT = 30;

export const SCREEN_SHARE_AUDIO_DEFAULT = false;

export function clampScreenShareFps(v: number | undefined): number {
  if (v === undefined || !Number.isFinite(v)) {
    return SCREEN_SHARE_FPS_DEFAULT;
  }
  return SCREEN_SHARE_FPS_OPTIONS.reduce((best, opt) =>
    Math.abs(opt - v) < Math.abs(best - v) ? opt : best,
  );
}

export const REMOTE_USER_VOLUME_MIN = 0.0;
export const REMOTE_USER_VOLUME_MAX = 2.0;
export const REMOTE_USER_VOLUME_DEFAULT = 1.0;

export function clampRemoteUserVolume(v: number): number {
  if (!Number.isFinite(v)) {
    return REMOTE_USER_VOLUME_DEFAULT;
  }
  return Math.max(REMOTE_USER_VOLUME_MIN, Math.min(REMOTE_USER_VOLUME_MAX, v));
}

export const APM_DEFAULTS = {
  mic_boost_db: 0,
  auto_gain_control: true,
  agc_target_dbfs: 6,
  noise_suppression_level: "high" as NoiseSuppressionLevel,
  echo_cancellation: true,
  click_suppression: false,
} as const;

/**
 * The Rust `voice_apm::ApmConfig`, for `join_voice_channel` and
 * `set_voice_audio_processing`. Field names are wire-format.
 */
export interface ApmConfig {
  mic_boost_db: number;
  agc_enabled: boolean;
  agc_target_dbfs: number;
  ns_level: NoiseSuppressionLevel;
  aec_enabled: boolean;
  click_suppression: boolean;
}

/** Voice prefs → APM config, defaulting undefined fields so a partial row is safe. */
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

/** Read a typed value from a preferences JSON string; default on bad JSON or absent key. */
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
      // Malformed JSON: fall through to default
  }
  return defaultValue;
}

const prefsKey = (userId: string | null) => ["user", "preferences", userId] as const;

// Throttle for the remote `save_preferences` only; the cache update and live
// effects fire every call. Leading and trailing edges always fire.
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
  const currentUser = useObserver(() => appStore.currentUser);
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
        skin: normalizeSkin(getPreference<string | undefined>(json, "skin", undefined)),
        font_size: getPreference<string | undefined>(json, "font_size", undefined),
        allow_desktop_notifications: getPreference<boolean>(json, "allow_desktop_notifications", false),
        allow_sound_effects: getPreference<boolean>(json, "allow_sound_effects", true),
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
        // Read back explicitly: a field missing here silently reverts to
        // voice-activity on refetch and drops a live call out of PTT.
        voice_input_mode: normalizeVoiceInputMode(
          getPreference<string | undefined>(json, "voice_input_mode", undefined),
        ),
        sidebar_open_by_default: getPreference<boolean>(json, "sidebar_open_by_default", true),
        // Stays `undefined`: absent means "follow the skin".
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

/**
 * Reactive UI skin, for components whose STRUCTURE differs by skin. Defaults to
 * terminal before prefs load; colour-only differences belong in CSS.
 */
export function useSkin(): Skin {
  const { query } = usePreferences();
  return normalizeSkin(query.data?.skin);
}

/**
 * Reactive "send read receipts" (#857), true until prefs load. Renderer
 * convenience only; Rust enforces it.
 */
export function useSendReadReceipts(): boolean {
  const { query } = usePreferences();
  return query.data?.send_read_receipts ?? true;
}

/**
 * Apply loaded preferences to CSS vars and the shortcut / voice modules. Font
 * size is device-local (`applyDeviceFontSize`).
 */
export function applyPreferences(prefs: PreferencesData): void {
  if (prefs.accent_color) {
    applyAccentColor(prefs.accent_color);
  }
  if (prefs.background_color) {
    applyBackgroundColor(prefs.background_color);
  }
  applySkin(normalizeSkin(prefs.skin));
  // An undefined map clears any previous override.
  setShortcutOverrides(
    (prefs.shortcut_overrides ?? {}) as Partial<Record<ShortcutCommandId, string>>,
  );
  // On load too, so a synced PTT preference applies on a fresh device.
  void voiceSession.setInputMode(prefs.voice_input_mode ?? VOICE_INPUT_MODE_DEFAULT);
}

/**
 * Apply the device-local font size for `userId`, seeding it once from the
 * legacy remote `font_size` if localStorage has none.
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
 * Re-apply visual prefs whenever the query data changes. Mounted once near the
 * root so login and stored-session reopen share one path.
 */
export function useApplyPreferences(): void {
  const { query } = usePreferences();
  const currentUser = useObserver(() => appStore.currentUser);
  const data = query.data;
  // Stringified so only an override change re-runs the effect.
  const overridesKey = JSON.stringify(data?.shortcut_overrides ?? {});
  useEffect(() => {
    if (data) {
      applyPreferences(data);
      applyDeviceFontSize(currentUser?.id, data);
    }
  }, [data?.accent_color, data?.background_color, data?.skin, data?.font_size, overridesKey, currentUser?.id]);

  // Pushed to the host so the close handler can decide synchronously.
  const closeToTray = data?.close_to_tray ?? true;
  useEffect(() => {
    void setTrayCloseToTray(closeToTray).catch((err) => {
      console.warn("[tray] setTrayCloseToTray failed:", err);
    });
  }, [closeToTray]);

  // macOS menu-bar icon; off by default, so a fresh install claims no slot.
  const menubarIcon = data?.menubar_icon ?? false;
  useEffect(() => {
    void setTrayEnabled(menubarIcon).catch((err) => {
      console.warn("[tray] setTrayEnabled failed:", err);
    });
  }, [menubarIcon]);

  // Pushed so the ExitRequested hook can read it synchronously.
  const revokeMediaOnExit = data?.revoke_media_on_exit ?? false;
  useEffect(() => {
    void invoke("set_revoke_media_on_exit", { enabled: revokeMediaOnExit }).catch(
      (err) => {
        console.warn("[media-permissions] set_revoke_media_on_exit failed:", err);
      },
    );
  }, [revokeMediaOnExit]);
}
