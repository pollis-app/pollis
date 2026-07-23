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

/**
 * Mirrors `voice_apm::NsLevel` in src-tauri.
 */
export type NoiseSuppressionLevel = "off" | "low" | "moderate" | "high";

/**
 * Network-privacy relay overlay mode. Mirrors the Rust `pollis_relay::OverlayMode`
 * that `get_overlay_mode` / `set_overlay_mode` (see `commands/overlay.rs`) parse
 * and apply. `off` is the default direct path; `prefer` routes through the relay
 * when one is reachable and falls back to direct; `strict` routes only through the
 * relay and pauses sending rather than revealing the client IP when none is up.
 */
export type OverlayMode = "off" | "prefer" | "strict";

/** Coerce an arbitrary string to a known `OverlayMode`; unknown/absent → `off`. */
export function normalizeOverlayMode(v: string | undefined | null): OverlayMode {
  return v === "prefer" || v === "strict" ? v : "off";
}

export interface PreferencesData {
  accent_color?: string;
  background_color?: string;
  /**
   * UI skin — `terminal` (default IRC/monospace look) or `refined` (friendlier
   * proportional-sans, Slack/Discord-shaped alternate). Synced across devices,
   * like `accent_color`/`background_color`. Absent → terminal.
   */
  skin?: Skin;
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
  /**
   * Screen-share capture/encode framerate ceiling, in fps. One of
   * `SCREEN_SHARE_FPS_OPTIONS` (15 / 30 / 60). Read at share-start and passed
   * to the publish path (Rust `start_screen_share` under Tauri, `getUserMedia`
   * constraints under Electron). Absent → `SCREEN_SHARE_FPS_DEFAULT`.
   */
  screen_share_max_fps?: number;
  auto_join_voice?: boolean;
  /** Whether the left sidebar is open by default at app start. */
  sidebar_open_by_default?: boolean;
  /**
   * Linux/Windows only: when true, closing the window hides the app to the
   * system tray instead of fully exiting. macOS already hides via the Dock
   * regardless. Default true.
   */
  close_to_tray?: boolean;
  /**
   * macOS only: when true, Pollis shows a status item in the menu bar
   * (top-right) with quick controls (open, mute toggle, quit). Default
   * false — the menu bar is prime real estate, so we wait for the user
   * to opt in. Linux/Windows ignore this; their tray is always set up.
   */
  menubar_icon?: boolean;
  /**
   * When true, Pollis clears the OS media permissions (camera / microphone /
   * screen share) as it quits. macOS runs `tccutil reset` per kind so the OS
   * re-prompts next use; Linux/Windows have no standing grant to clear. Pushed
   * to the host via `set_revoke_media_on_exit`. Default false.
   */
  revoke_media_on_exit?: boolean;
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
  /**
   * Per-command keyboard shortcut overrides, keyed by `ShortcutCommandId`.
   * Values are canonical combo strings (e.g. `"mod+shift+k"`) understood by
   * `keyboard/keyCombo.ts`. Missing entries fall back to the built-in
   * `defaultCombo` in `keyboard/commands.ts`.
   */
  shortcut_overrides?: { [commandId: string]: string };
  /**
   * Network-privacy relay overlay mode (#455). Persisted through the synced
   * preferences blob like every other field so the choice survives restart and
   * syncs across devices; applied live via `set_overlay_mode` on change and on
   * load. Absent → `off` (the direct path). See `OverlayMode`.
   */
  overlay_mode?: OverlayMode;
}

// Voice-identity parsing (`userIdFromVoiceIdentity`) lives in
// `voice/identity.ts` — the canonical home shared with the rest of the voice
// layer. Import it from there.

/**
 * Screen-share framerate presets, in fps. 15 = documents/browsing (cheapest,
 * good for constrained machines/networks), 30 = standard, 60 = motion/gameplay.
 */
export const SCREEN_SHARE_FPS_OPTIONS = [15, 30, 60] as const;
export const SCREEN_SHARE_FPS_DEFAULT = 30;

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
        auto_join_voice: getPreference<boolean>(json, "auto_join_voice", false),
        sidebar_open_by_default: getPreference<boolean>(json, "sidebar_open_by_default", true),
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
 * Reactive current UI skin, for components that must branch their *structure*
 * (not just color) on the skin — e.g. the message row renders IRC-style in
 * terminal and Slack-style in refined. Backed by the synced preferences query,
 * so it updates the instant the toggle flips (the toggle writes the query
 * cache). Defaults to terminal before prefs load. Pure color/spacing
 * differences should use CSS (`:root[data-skin="refined"]`) instead of this.
 */
export function useSkin(): Skin {
  const { query } = usePreferences();
  return normalizeSkin(query.data?.skin);
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
  applySkin(normalizeSkin(prefs.skin));
  // Shortcut overrides flow through the same bindings module that
  // `useGlobalShortcut` resolves against on every keydown — no callsite
  // changes needed. An undefined map clears any previous override.
  setShortcutOverrides(
    (prefs.shortcut_overrides ?? {}) as Partial<Record<ShortcutCommandId, string>>,
  );
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
  const currentUser = useObserver(() => appStore.currentUser);
  const data = query.data;
  // Stringify the override map so the effect re-runs when any override
  // changes (a React Query refetch returns a new object reference, and we
  // don't want to re-fire on unrelated pref edits either).
  const overridesKey = JSON.stringify(data?.shortcut_overrides ?? {});
  useEffect(() => {
    if (data) {
      applyPreferences(data);
      applyDeviceFontSize(currentUser?.id, data);
    }
  }, [data?.accent_color, data?.background_color, data?.skin, data?.font_size, overridesKey, currentUser?.id]);

  // Push close-to-tray to the host so the close handler can pick
  // hide-vs-quit synchronously. macOS ignores this (close already hides via
  // the Dock path), but pushing is harmless and keeps the Linux/Windows
  // path live.
  const closeToTray = data?.close_to_tray ?? true;
  useEffect(() => {
    void setTrayCloseToTray(closeToTray).catch((err) => {
      console.warn("[tray] setTrayCloseToTray failed:", err);
    });
  }, [closeToTray]);

  // macOS menu-bar icon. Linux/Windows ignore this on the main side;
  // they have a tray unconditionally once setup succeeds. Default
  // is off, so the very first load on a fresh macOS install does NOT
  // claim a menu-bar slot until the user opts in.
  const menubarIcon = data?.menubar_icon ?? false;
  useEffect(() => {
    void setTrayEnabled(menubarIcon).catch((err) => {
      console.warn("[tray] setTrayEnabled failed:", err);
    });
  }, [menubarIcon]);

  // Push the "revoke media permissions on quit" pref to the host so the
  // ExitRequested hook can read it synchronously at shutdown. Same reasoning
  // as close-to-tray above.
  const revokeMediaOnExit = data?.revoke_media_on_exit ?? false;
  useEffect(() => {
    void invoke("set_revoke_media_on_exit", { enabled: revokeMediaOnExit }).catch(
      (err) => {
        console.warn("[media-permissions] set_revoke_media_on_exit failed:", err);
      },
    );
  }, [revokeMediaOnExit]);
}
