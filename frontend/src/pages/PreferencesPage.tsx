import React, { useState, useEffect, useCallback } from "react";
import { Trans, useTranslation } from "react-i18next";
import { useNavigate } from "@tanstack/react-router";
import {
  invoke,
  isPermissionGranted,
  requestPermission,
  setTrayCloseToTray,
  setTrayEnabled,
} from "../bridge";
import { PageShell } from "../components/Layout/PageShell";
import {
  usePreferences,
  applyPreferences,
  applyDeviceFontSize,
  normalizeOverlayMode,
  type OverlayMode,
  type PreferencesData,
} from "../hooks/queries/usePreferences";
import {
  useMessageRetention,
  useSetMessageRetention,
  MESSAGE_RETENTION_OPTIONS,
  RETENTION_FOREVER,
} from "../hooks/queries/useMessageRetention";
import { useRebuildSearchIndex } from "../hooks/queries/useSearchMessages";
import {
  useRelayServingStatus,
  useApplyRelayServing,
  relayServingConfigEquals,
  RELAY_SERVING_DEFAULTS,
  type RelayServingConfig,
} from "../hooks/queries/useRelayServing";
import { RelayServingSection } from "../components/Preferences/RelayServingSection";
import { LanguageSection } from "../components/Preferences/LanguageSection";
import {
  hslToHex,
  hexToHsl,
  applyAccentColor,
  applyBackgroundColor,
  applyFontSize,
  applySkin,
  normalizeSkin,
  loadDeviceFontSize,
  saveDeviceFontSize,
  type Skin,
} from "../utils/colorUtils";
import { RangeSlider } from "../components/ui/RangeSlider";
import { Switch } from "../components/ui/Switch";
import { Button } from "../components/ui/Button";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";
import { loadDeviceCallRingtone, saveDeviceCallRingtone } from "../utils/notify";
import { isMac } from "../utils/platform";
import { useShortcutLabel } from "../keyboard";
import { useRightPanel } from "../components/Layout/RightPanel/useRightPanel";

function getRootVar(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

function isValidHex(val: string): boolean {
  return /^#[0-9a-fA-F]{6}$/.test(val);
}

// Project the three synced `relay_serving*` fields onto the config shape the
// backend and the section component both speak. Absent fields fall back to
// RELAY_SERVING_DEFAULTS (consent off, both conditions on).
function relayServingFromPrefs(prefs: PreferencesData): RelayServingConfig {
  return {
    enabled: prefs.relay_serving ?? RELAY_SERVING_DEFAULTS.enabled,
    wifi_only: prefs.relay_serving_wifi_only ?? RELAY_SERVING_DEFAULTS.wifi_only,
    power_only: prefs.relay_serving_power_only ?? RELAY_SERVING_DEFAULTS.power_only,
  };
}

export const PreferencesPage: React.FC = observer(() => {
  const { t } = useTranslation("settings");
  const navigate = useNavigate();
  const currentUser = appStore.currentUser;
  const toggleSidebarLabel = useShortcutLabel("app.toggleSidebar");
  const toggleRightPanelLabel = useShortcutLabel("app.toggleRightPanel");
  // The panel's live state, so the switch below reports what is actually on
  // screen rather than a synced default the device may have moved past.
  const { isOpen: isRightPanelOpen, setPanel: setRightPanel } = useRightPanel();
  const [skin, setSkin] = useState<Skin>("terminal");
  const [hue, setHue] = useState<number>(38);
  const [saturation, setSaturation] = useState<number>(90);
  const [bgHue, setBgHue] = useState<number>(38);
  const [bgSaturation, setBgSaturation] = useState<number>(20);
  const [bgLightness, setBgLightness] = useState<number>(4);
  const [fontSize, setFontSize] = useState<number>(15);
  const [allowDesktopNotifications, setAllowDesktopNotifications] = useState<boolean>(true);
  const [allowSoundEffects, setAllowSoundEffects] = useState<boolean>(true);
  const [sendReadReceipts, setSendReadReceipts] = useState<boolean>(true);
  const [allowCallRingtone, setAllowCallRingtone] = useState<boolean>(true);
  const [sidebarOpenByDefault, setSidebarOpenByDefault] = useState<boolean>(true);
  // `undefined` until the user touches it — that state means "this account has
  // never expressed a preference", which is what lets a brand-new device seed
  // itself from the skin instead of from a boolean nobody chose (#904). The
  // SWITCH does not render this; it renders the panel's live state, because
  // that is the thing the user is looking at while they flip it.
  const [rightPanelOpenByDefault, setRightPanelOpenByDefault] = useState<
    boolean | undefined
  >(undefined);
  const [closeToTray, setCloseToTray] = useState<boolean>(true);
  const [menubarIcon, setMenubarIcon] = useState<boolean>(false);
  const [overlayMode, setOverlayMode] = useState<OverlayMode>("off");
  // Inline status line under the relay control: an apply error (e.g. Strict
  // with no relay reachable) surfaces here rather than throwing.
  const [overlayStatus, setOverlayStatus] = useState<string | null>(null);
  // "Be a relay" consent + its conditions (#813 §10.2). Entirely separate
  // from `overlayMode` above — that routes this user's traffic, this carries
  // other people's. Defaults to consent off, both conditions on.
  const [relayServing, setRelayServing] = useState<RelayServingConfig>(RELAY_SERVING_DEFAULTS);
  const [relayServingError, setRelayServingError] = useState<string | null>(null);
  const [accentHexInput, setAccentHexInput] = useState<string>(() => hslToHex(38, 90, 62));
  const [bgHexInput, setBgHexInput] = useState<string>(() => hslToHex(38, 20, 4));

  const { query, save: savePrefs } = usePreferences();

  // Live relay-serving status. `null` data means the host can't report it —
  // the section says so plainly rather than implying the device is relaying.
  const relayStatusQuery = useRelayServingStatus();
  const applyRelayServing = useApplyRelayServing();

  // Device-local message retention window (see useMessageRetention). Selecting
  // an option fires the mutation immediately — the backend sweep is immediate.
  const retentionQuery = useMessageRetention();
  // Manual repair for the local full-text search index (#850).
  const rebuildSearchIndex = useRebuildSearchIndex();
  const setRetention = useSetMessageRetention();
  const retentionDays = retentionQuery.data ?? MESSAGE_RETENTION_OPTIONS[0].days;

  // Apply saved preferences on first load
  useEffect(() => {
    if (query.data) {
      applyPreferences(query.data);
      // Font size is device-local; seed once from any legacy remote value.
      applyDeviceFontSize(currentUser?.id, query.data);
      setSkin(normalizeSkin(query.data.skin));
      if (query.data.allow_desktop_notifications !== undefined) {
        setAllowDesktopNotifications(query.data.allow_desktop_notifications);
      }
      if (query.data.allow_sound_effects !== undefined) {
        setAllowSoundEffects(query.data.allow_sound_effects);
      }
      if (query.data.send_read_receipts !== undefined) {
        setSendReadReceipts(query.data.send_read_receipts);
      }
      if (query.data.sidebar_open_by_default !== undefined) {
        setSidebarOpenByDefault(query.data.sidebar_open_by_default);
      }
      // No `!== undefined` guard: absent is a meaningful value here, and
      // skipping it would strand the local state after a reset elsewhere.
      setRightPanelOpenByDefault(query.data.right_panel_open_by_default);
      if (query.data.close_to_tray !== undefined) {
        setCloseToTray(query.data.close_to_tray);
      }
      if (query.data.menubar_icon !== undefined) {
        setMenubarIcon(query.data.menubar_icon);
      }
      setOverlayMode(normalizeOverlayMode(query.data.overlay_mode));
      setRelayServing(relayServingFromPrefs(query.data));
    }
  }, [query.data, currentUser?.id]);

  // Read current CSS var values on mount and sync all state + hex inputs
  useEffect(() => {
    const h = parseInt(getRootVar("--accent-h"));
    const s = parseInt(getRootVar("--accent-s"));
    const bh = parseInt(getRootVar("--bg-h"));
    const bs = parseInt(getRootVar("--bg-s"));
    const bl = parseInt(getRootVar("--bg-l"));
    if (!isNaN(h)) { setHue(h); }
    if (!isNaN(s)) { setSaturation(s); }
    if (!isNaN(h) && !isNaN(s)) { setAccentHexInput(hslToHex(h, s, 62)); }
    if (!isNaN(bh)) { setBgHue(bh); }
    if (!isNaN(bs)) { setBgSaturation(bs); }
    if (!isNaN(bl)) { setBgLightness(bl); }
    if (!isNaN(bh) && !isNaN(bs) && !isNaN(bl)) { setBgHexInput(hslToHex(bh, bs, bl)); }
    // Font size: prefer the device-local store; fall back to whatever the
    // CSS var currently resolves to (default 15px) so a fresh device
    // shows the slider in a sane position before the user touches it.
    const localFs = loadDeviceFontSize(currentUser?.id);
    if (localFs !== null) {
      setFontSize(localFs);
    } else {
      const fs = parseInt(getRootVar("--font-size-base"));
      if (!isNaN(fs)) { setFontSize(fs); }
    }
    setAllowCallRingtone(loadDeviceCallRingtone(currentUser?.id));
  }, [currentUser?.id]);

  const save = useCallback((opts: {
    accentH?: number; accentS?: number;
    bgH?: number; bgS?: number; bgL?: number;
    skin?: Skin;
    notifications?: boolean; soundEffects?: boolean;
    sendReadReceipts?: boolean;
    sidebarOpenByDefault?: boolean;
    rightPanelOpenByDefault?: boolean;
    closeToTray?: boolean;
    menubarIcon?: boolean;
    overlayMode?: OverlayMode;
    relayServing?: RelayServingConfig;
  }) => {
    const ah = opts.accentH ?? hue;
    const as_ = opts.accentS ?? saturation;
    const bh = opts.bgH ?? bgHue;
    const bs = opts.bgS ?? bgSaturation;
    const bl = opts.bgL ?? bgLightness;
    const notif = opts.notifications ?? allowDesktopNotifications;
    const sfx = opts.soundEffects ?? allowSoundEffects;
    const receipts = opts.sendReadReceipts ?? sendReadReceipts;
    const sidebar = opts.sidebarOpenByDefault ?? sidebarOpenByDefault;
    const rightPanel = opts.rightPanelOpenByDefault ?? rightPanelOpenByDefault;
    const tray = opts.closeToTray ?? closeToTray;
    const menubar = opts.menubarIcon ?? menubarIcon;
    const overlay = opts.overlayMode ?? overlayMode;
    const relay = opts.relayServing ?? relayServing;
    const skinVal = opts.skin ?? skin;
    const accentHex = hslToHex(ah, as_, 62);
    const bgHex = hslToHex(bh, bs, bl);
    // font_size is intentionally NOT included — it's device-local now,
    // persisted via saveDeviceFontSize. We also strip any legacy
    // `font_size` field from query.data so we stop overwriting our own
    // local value back to the remote on every save.
    const { font_size: _legacyFontSize, ...rest } = query.data ?? {};
    void _legacyFontSize;
    savePrefs({
      ...rest,
      accent_color: accentHex,
      background_color: bgHex,
      skin: skinVal,
      allow_desktop_notifications: notif,
      allow_sound_effects: sfx,
      send_read_receipts: receipts,
      sidebar_open_by_default: sidebar,
      right_panel_open_by_default: rightPanel,
      close_to_tray: tray,
      menubar_icon: menubar,
      overlay_mode: overlay,
      relay_serving: relay.enabled,
      relay_serving_wifi_only: relay.wifi_only,
      relay_serving_power_only: relay.power_only,
    });
  }, [savePrefs, query.data, hue, saturation, bgHue, bgSaturation, bgLightness, skin, allowDesktopNotifications, allowSoundEffects, sendReadReceipts, sidebarOpenByDefault, closeToTray, menubarIcon, overlayMode, relayServing]);

  // Drive the merged overlay engine (`set_overlay_mode`) to `val`, live. Never
  // throws: a rejected apply (e.g. Strict with no relay reachable — the engine
  // rolls back rather than silently going direct) is surfaced inline and the
  // control snaps to whatever mode actually took effect (`get_overlay_mode`).
  // `persist` writes the result through the synced preferences blob; the
  // apply-on-load path passes false so it never re-saves.
  const applyOverlayMode = useCallback(async (val: OverlayMode, persist: boolean) => {
    try {
      await invoke("set_overlay_mode", { mode: val });
      setOverlayStatus(null);
      if (persist) {
        save({ overlayMode: val });
      }
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      let actual: OverlayMode = val;
      try {
        actual = normalizeOverlayMode(await invoke<string>("get_overlay_mode"));
      } catch {
        // `get_overlay_mode` itself failed — keep the best guess (`val`) and
        // still surface the original apply error below.
      }
      setOverlayMode(actual);
      setOverlayStatus(msg);
      if (persist) {
        save({ overlayMode: actual });
      }
    }
  }, [save]);

  // Apply the saved relay preference after login/restart so the synced choice
  // takes effect. Guarded: only invoke when the desired mode differs from the
  // currently-running one, so a redundant re-apply never reconnects the DBs.
  useEffect(() => {
    const desired = query.data?.overlay_mode;
    if (desired === undefined) {
      return;
    }
    let cancelled = false;
    void (async () => {
      try {
        const current = normalizeOverlayMode(await invoke<string>("get_overlay_mode"));
        if (cancelled || current === desired) {
          return;
        }
      } catch {
        // Couldn't read the live mode — fall through and apply anyway;
        // `set_overlay_mode` is itself idempotent.
      }
      if (!cancelled) {
        await applyOverlayMode(desired, false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [query.data?.overlay_mode, applyOverlayMode]);

  // One label per overlay mode, resolved once. `off` / `prefer` / `strict` are
  // wire values, and the error line below used to interpolate the raw token
  // into a translated sentence ("currently strict") while the buttons two
  // sections down translated the very same three words.
  const overlayModeLabels: Record<OverlayMode, string> = {
    off: t("overlay.modeOff"),
    prefer: t("overlay.modePrefer"),
    strict: t("overlay.modeStrict"),
  };

  const handleOverlayMode = (val: OverlayMode) => {
    if (val === overlayMode) {
      return;
    }
    setOverlayMode(val);
    setOverlayStatus(null);
    void applyOverlayMode(val, true);
  };

  // Drive the running relay to `next`, live. Unlike the overlay control, a
  // failed apply does NOT snap the toggle back: the consent is the user's
  // choice, not the backend's, so we keep it, persist it, and say plainly in
  // the status line that we can't confirm what the device is doing.
  const applyRelayServingConfig = useCallback(async (next: RelayServingConfig, persist: boolean) => {
    try {
      const status = await applyRelayServing(next);
      setRelayServingError(null);
      // The backend is the authority on what actually took effect.
      setRelayServing(status.config);
      if (persist) {
        save({ relayServing: status.config });
      }
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setRelayServingError(msg);
      if (persist) {
        save({ relayServing: next });
      }
    }
  }, [applyRelayServing, save]);

  // Re-apply the saved relay-serving choice after login/restart, so the
  // synced consent takes effect. Guarded on the live config the backend
  // reports, so a redundant apply never restarts the relay.
  useEffect(() => {
    if (!query.data) {
      return;
    }
    const desired = relayServingFromPrefs(query.data);
    const live = relayStatusQuery.data?.config;
    if (live !== undefined && relayServingConfigEquals(live, desired)) {
      return;
    }
    void applyRelayServingConfig(desired, false);
  }, [
    query.data?.relay_serving,
    query.data?.relay_serving_wifi_only,
    query.data?.relay_serving_power_only,
    relayStatusQuery.data?.config,
    applyRelayServingConfig,
  ]);

  const handleRelayServing = (next: RelayServingConfig) => {
    setRelayServing(next);
    setRelayServingError(null);
    void applyRelayServingConfig(next, true);
  };

  const handleAccentColor = (hex: string) => {
    const [h, s] = hexToHsl(hex);
    setHue(h);
    setSaturation(s);
    const normalized = hslToHex(h, s, 62);
    setAccentHexInput(normalized);
    applyAccentColor(normalized);
    save({ accentH: h, accentS: s });
  };

  const handleBgColor = (hex: string) => {
    const [h, s, l] = hexToHsl(hex);
    setBgHue(h);
    setBgSaturation(s);
    setBgLightness(l);
    setBgHexInput(hex);
    applyBackgroundColor(hex);
    save({ bgH: h, bgS: s, bgL: l });
  };

  const handleSkin = (val: Skin) => {
    setSkin(val);
    applySkin(val);
    save({ skin: val });
  };

  const handleFontSize = (val: number) => {
    setFontSize(val);
    applyFontSize(val);
    saveDeviceFontSize(currentUser?.id, val);
  };

  const handleAllowSoundEffects = (val: boolean) => {
    setAllowSoundEffects(val);
    save({ soundEffects: val });
  };

  const handleSendReadReceipts = (val: boolean) => {
    setSendReadReceipts(val);
    save({ sendReadReceipts: val });
  };

  const handleSidebarOpenByDefault = (val: boolean) => {
    setSidebarOpenByDefault(val);
    save({ sidebarOpenByDefault: val });
  };

  // Two writes, deliberately. The device-local one is what actually opens or
  // shuts the panel here and now; the synced one is the seed a NEW device
  // picks up. Without the first the switch would be inert on any device that
  // has already made up its mind, which is every device after its first launch.
  const handleRightPanelOpenByDefault = (val: boolean) => {
    setRightPanelOpenByDefault(val);
    setRightPanel(val ? "members" : "none");
    save({ rightPanelOpenByDefault: val });
  };

  const handleCloseToTray = (val: boolean) => {
    setCloseToTray(val);
    save({ closeToTray: val });
    // Push immediately so the very next window close picks up the new
    // value (useApplyPreferences would also re-fire, but only after the
    // throttled save round-trips through the remote prefs query).
    void setTrayCloseToTray(val).catch((err) => {
      console.warn("[tray] setTrayCloseToTray failed:", err);
    });
  };

  const handleMenubarIcon = (val: boolean) => {
    setMenubarIcon(val);
    save({ menubarIcon: val });
    // Same reasoning as handleCloseToTray: apply right away so the icon
    // appears/disappears the moment the toggle flips, without waiting
    // for the throttled prefs round-trip.
    void setTrayEnabled(val).catch((err) => {
      console.warn("[tray] setTrayEnabled failed:", err);
    });
  };

  const handleAllowCallRingtone = (val: boolean) => {
    setAllowCallRingtone(val);
    saveDeviceCallRingtone(currentUser?.id, val);
  };

  // `MESSAGE_RETENTION_OPTIONS` lives in `hooks/queries/useMessageRetention.ts`
  // — module-level data, so its `label` can't be translated there without
  // freezing the language at import time. Key off the stable `days` value and
  // translate here, at the render site, instead.
  const retentionLabel = (days: number): string => {
    if (days === RETENTION_FOREVER) {
      return t("retention.optionForever");
    }
    if (days % 365 === 0) {
      return t("retention.optionYears", { count: days / 365 });
    }
    return t("retention.optionDays", { count: days });
  };

  const handleAllowDesktopNotifications = async (val: boolean) => {
    setAllowDesktopNotifications(val);
    save({ notifications: val });
    // When enabling, ensure we have OS-level permission (prompts on macOS)
    if (val) {
      try {
        const granted = await isPermissionGranted();
        if (!granted) {
          await requestPermission();
        }
      } catch {
        // Notification host unavailable — ignore
      }
    }
  };

  return (
    <PageShell title={t("preferences.title")} scrollable>
      <div
        data-testid="preferences-page"
        className="flex-1 flex flex-col overflow-auto bg-bg"
      >
        <div className="flex-1 flex justify-center overflow-auto px-6 py-8">
          <div className="w-full max-w-md flex flex-col gap-8">

            {/* Language (this device). Deliberately first: someone who cannot
                read the UI needs the control that fixes that at the top, not
                buried under colour pickers. */}
            <LanguageSection userId={currentUser?.id ?? null} />

            {/* Appearance — UI skin (synced across devices) */}
            <section className="flex flex-col gap-4 mb-12">
              <h2
                className="text-xs font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line"
              >
                {t("appearance.heading")}
              </h2>
              <div
                role="radiogroup"
                aria-label={t("appearance.ariaLabel")}
                className="flex gap-2 flex-wrap"
              >
                {([
                  { value: "terminal", label: t("appearance.skinTerminal") },
                  { value: "refined", label: t("appearance.skinRefined") },
                ] as const).map((opt) => (
                  <Button
                    key={opt.value}
                    variant={skin === opt.value ? "primary" : "secondary"}
                    size="sm"
                    aria-label={opt.label}
                    data-testid={`pref-skin-${opt.value}`}
                    onClick={() => {
                      if (skin !== opt.value) {
                        handleSkin(opt.value);
                      }
                    }}
                  >
                    {opt.label}
                  </Button>
                ))}
              </div>
              <p className="text-xs font-mono text-muted">
                {t("appearance.description")}
              </p>
            </section>

            {/* Accent Color */}
            <section className="flex flex-col gap-4 mb-12">
              <h2
                className="text-xs font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line"
              >
                {t("accent.heading")}
              </h2>

              <div className="flex items-center gap-2">
                <label
                  className="flex-shrink-0 cursor-pointer overflow-hidden focus-within:ring-4 focus-within:ring-accent focus-within:ring-offset-2 focus-within:ring-offset-black"
                  style={{ width: 40, height: 40, borderRadius: 8, padding: 0 }}
                  title={t("accent.pickTitle")}
                >
                  <input
                    type="color"
                    value={hslToHex(hue, saturation, 62)}
                    onChange={(e) => handleAccentColor(e.target.value)}
                    className="border-0"
                    style={{ width: "150%", height: "150%", margin: "-25%", padding: 0, cursor: "pointer" }}
                  />
                </label>
                <input
                  type="text"
                  value={accentHexInput}
                  onChange={(e) => {
                    const val = e.target.value;
                    setAccentHexInput(val);
                    if (isValidHex(val)) {
                      handleAccentColor(val);
                    }
                  }}
                  onBlur={() => {
                    if (!isValidHex(accentHexInput)) {
                      setAccentHexInput(hslToHex(hue, saturation, 62));
                    }
                  }}
                  maxLength={7}
                  spellCheck={false}
                  className={`text-xs font-mono font-machine px-2 py-1 bg-surface border border-line ${isValidHex(accentHexInput) ? "text-fg" : "text-danger"} focus:outline-none focus:ring-4 focus:ring-accent focus:ring-offset-2 focus:ring-offset-black`}
                  style={{
                    width: 90,
                    borderRadius: 6,
                  }}
                />
              </div>

              {/* Quick presets */}
              <div className="flex gap-2 flex-wrap">
                {[
                  { id: "orange", label: t("accent.presetOrange"), h: 38, s: 90 },
                  { id: "green", label: t("accent.presetGreen"), h: 150, s: 62 },
                  { id: "blue", label: t("accent.presetBlue"), h: 210, s: 80 },
                  { id: "purple", label: t("accent.presetPurple"), h: 270, s: 70 },
                  { id: "red", label: t("accent.presetRed"), h: 0, s: 85 },
                  { id: "cyan", label: t("accent.presetCyan"), h: 185, s: 75 },
                ].map((preset) => (
                  <button
                    key={preset.id}
                    onClick={() => {
                      setHue(preset.h);
                      setSaturation(preset.s);
                      const hex = hslToHex(preset.h, preset.s, 62);
                      setAccentHexInput(hex);
                      applyAccentColor(hex);
                      save({ accentH: preset.h, accentS: preset.s });
                    }}
                    className="px-2 py-0.5 text-xs font-mono transition-colors focus:outline-none focus:ring-4 focus:ring-accent focus:ring-offset-2 focus:ring-offset-black"
                    style={{
                      background: `hsl(${preset.h} ${preset.s}% 62% / 15%)`,
                      border: `1px solid hsl(${preset.h} ${preset.s}% 62% / 40%)`,
                      color: `hsl(${preset.h} ${preset.s}% 65%)`,
                      borderRadius: 4,
                    }}
                  >
                    {preset.label}
                  </button>
                ))}
              </div>
            </section>

            {/* Background Color */}
            <section className="flex flex-col gap-4 mb-12">
              <h2
                className="text-xs font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line"
              >
                {t("background.heading")}
              </h2>

              <div className="flex items-center gap-2">
                <label
                  className="flex-shrink-0 cursor-pointer overflow-hidden focus-within:ring-4 focus-within:ring-accent focus-within:ring-offset-2 focus-within:ring-offset-black"
                  style={{ width: 40, height: 40, padding: 0, borderRadius: "0.5rem", outline: "2px solid var(--c-accent)", outlineOffset: "-1px" }}
                  title={t("background.pickTitle")}
                >
                  <input
                    type="color"
                    value={hslToHex(bgHue, bgSaturation, bgLightness)}
                    onChange={(e) => handleBgColor(e.target.value)}
                    className="border-0"
                    style={{ width: "150%", height: "150%", margin: "-25%", padding: 0, cursor: "pointer" }}
                  />
                </label>
                <input
                  type="text"
                  value={bgHexInput}
                  onChange={(e) => {
                    const val = e.target.value;
                    setBgHexInput(val);
                    if (isValidHex(val)) {
                      handleBgColor(val);
                    }
                  }}
                  onBlur={() => {
                    if (!isValidHex(bgHexInput)) {
                      setBgHexInput(hslToHex(bgHue, bgSaturation, bgLightness));
                    }
                  }}
                  maxLength={7}
                  spellCheck={false}
                  className={`text-xs font-mono font-machine px-2 py-1 bg-surface border border-line ${isValidHex(bgHexInput) ? "text-fg" : "text-danger"} focus:outline-none focus:ring-4 focus:ring-accent focus:ring-offset-2 focus:ring-offset-black`}
                  style={{
                    width: 90,
                    borderRadius: 6,
                  }}
                />
              </div>

              {/* Quick presets */}
              <div className="flex gap-2 flex-wrap">
                {[
                  { id: "match-accent", label: t("background.presetMatchAccent"), h: hue, s: 20 },
                  { id: "neutral", label: t("background.presetNeutral"), h: 0, s: 0 },
                  { id: "warm", label: t("background.presetWarm"), h: 30, s: 15 },
                  { id: "cool", label: t("background.presetCool"), h: 220, s: 15 },
                  { id: "green", label: t("background.presetGreen"), h: 150, s: 12 },
                  { id: "purple", label: t("background.presetPurple"), h: 270, s: 12 },
                ].map((preset) => (
                  <button
                    key={preset.id}
                    onClick={() => {
                      setBgHue(preset.h);
                      setBgSaturation(preset.s);
                      setBgLightness(7);
                      const hex = hslToHex(preset.h, preset.s, 7);
                      setBgHexInput(hex);
                      applyBackgroundColor(hex);
                      save({ bgH: preset.h, bgS: preset.s, bgL: 7 });
                    }}
                    className="px-2 py-0.5 text-xs font-mono transition-colors focus:outline-none focus:ring-4 focus:ring-accent focus:ring-offset-2 focus:ring-offset-black"
                    style={{
                      background: `hsl(${preset.h} ${preset.s}% 20% / 40%)`,
                      border: `1px solid hsl(${preset.h} ${preset.s}% 40% / 40%)`,
                      color: `hsl(${preset.h} ${Math.max(preset.s, 30)}% 65%)`,
                      borderRadius: 4,
                    }}
                  >
                    {preset.label}
                  </button>
                ))}
              </div>
            </section>


            {/* Display (this device) — settings here are stored on this device only,
                not synced across the user's account. Future device-specific items
                should slot in here. */}
            <section className="flex flex-col gap-4 mb-12">
              <h2
                className="text-xs font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line"
              >
                {t("display.heading")}
              </h2>
              <div className="flex flex-col gap-1.5">
                <RangeSlider
                  id="pref-font-size"
                  label={t("display.fontSizeLabel")}
                  value={fontSize}
                  min={12}
                  max={20}
                  step={1}
                  onChange={handleFontSize}
                />
                <div className="flex justify-between text-xs font-mono text-muted">
                  <span>{t("display.fontSizeSmall")}</span>
                  <span>{t("display.fontSizeNormal")}</span>
                  <span>{t("display.fontSizeLarge")}</span>
                </div>
                <p className="text-xs font-mono mt-1 text-muted">
                  {t("display.fontSizeNote")}
                </p>
              </div>
              <p
                className="font-mono text-dim"
                style={{ fontSize }}
              >
                {t("display.fontSizeSample")}
              </p>
              <div className="flex flex-col gap-1.5 mt-4">
                <Switch
                  id="pref-call-ringtone"
                  label={t("display.callRingtoneLabel")}
                  checked={allowCallRingtone}
                  onChange={handleAllowCallRingtone}
                />
                <p className="text-xs font-mono text-muted">
                  {t("display.callRingtoneDescription")}
                </p>
              </div>
            </section>

            {/* Layout */}
            <section className="flex flex-col gap-4 mb-12">
              <h2
                className="text-xs font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line"
              >
                {t("layout.heading")}
              </h2>
              <div className="flex flex-col gap-1.5">
                <Switch
                  id="pref-sidebar-default"
                  label={t("layout.sidebarLabel")}
                  checked={sidebarOpenByDefault}
                  onChange={handleSidebarOpenByDefault}
                />
                <p className="text-xs font-mono text-muted">
                  {t("layout.sidebarDescription", { shortcut: toggleSidebarLabel })}
                </p>
              </div>
              <div className="flex flex-col gap-1.5">
                <Switch
                  id="pref-right-panel-default"
                  label={t("layout.rightPanelLabel")}
                  checked={isRightPanelOpen}
                  onChange={handleRightPanelOpenByDefault}
                />
                <p className="text-xs font-mono text-muted">
                  {t("layout.rightPanelDescription", { shortcut: toggleRightPanelLabel })}
                </p>
              </div>
              {!isMac && (
                <div className="flex flex-col gap-1.5">
                  <Switch
                    id="pref-close-to-tray"
                    label={t("layout.closeToTrayLabel")}
                    checked={closeToTray}
                    onChange={handleCloseToTray}
                  />
                  <p className="text-xs font-mono text-muted">
                    {t("layout.closeToTrayDescription")}
                  </p>
                </div>
              )}
              {isMac && (
                <div className="flex flex-col gap-1.5">
                  <Switch
                    id="pref-menubar-icon"
                    label={t("layout.menubarIconLabel")}
                    checked={menubarIcon}
                    onChange={handleMenubarIcon}
                  />
                  <p className="text-xs font-mono text-muted">
                    {t("layout.menubarIconDescription")}
                  </p>
                </div>
              )}
            </section>

            {/* Notifications */}
            <section className="flex flex-col gap-4 mb-12">
              <h2
                className="text-xs font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line"
              >
                {t("notifications.heading")}
              </h2>
              <Switch
                id="pref-notifications"
                label={t("notifications.desktopLabel")}
                checked={allowDesktopNotifications}
                onChange={handleAllowDesktopNotifications}
              />
              <Switch
                id="pref-sound-effects"
                label={t("notifications.soundEffectsLabel")}
                checked={allowSoundEffects}
                onChange={handleAllowSoundEffects}
              />
            </section>

            {/* Read receipts (#857) — DMs only. */}
            <section className="flex flex-col gap-4 mb-12">
              <h2
                className="text-xs font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line"
              >
                {t("readReceipts.heading")}
              </h2>
              <Switch
                id="pref-read-receipts"
                label={t("readReceipts.toggleLabel")}
                checked={sendReadReceipts}
                onChange={handleSendReadReceipts}
              />
              <p className="text-xs font-mono text-muted">
                <Trans
                  t={t}
                  i18nKey="readReceipts.description"
                  components={{ em: <em /> }}
                />
              </p>
            </section>

            {/* Local message history (this device) — device-local retention
                window stored in the local DB, not synced across the account. */}
            <section className="flex flex-col gap-4 mb-12">
              <h2
                className="text-xs font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line"
              >
                {t("retention.heading")}
              </h2>
              <div
                role="radiogroup"
                aria-label={t("retention.ariaLabel")}
                className="flex gap-2 flex-wrap"
              >
                {MESSAGE_RETENTION_OPTIONS.map((option) => {
                  const selected = retentionDays === option.days;
                  const label = retentionLabel(option.days);
                  return (
                    <Button
                      key={option.days}
                      variant={selected ? "primary" : "secondary"}
                      size="sm"
                      aria-label={label}
                      data-testid={`pref-retention-${option.days}`}
                      onClick={() => {
                        if (selected) {
                          return;
                        }
                        setRetention.mutate(option.days);
                      }}
                    >
                      {label}
                    </Button>
                  );
                })}
              </div>
              <p className="text-xs font-mono text-muted">
                {t("retention.description")}
              </p>
            </section>

            {/* Message search index (#850) — device-local, inside the encrypted
                database. The button is the escape hatch for the one failure a
                contentless FTS5 index can have that nothing else repairs;
                startup already rebuilds silently when `integrity-check` catches
                it, so nobody should ever need this. It exists for when the
                automatic repair is the thing that is wrong. */}
            <section className="flex flex-col gap-4 mb-12">
              <h2 className="text-xs font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line">
                {t("searchIndex.heading")}
              </h2>
              <div>
                <Button
                  variant="secondary"
                  size="sm"
                  data-testid="pref-rebuild-search-index"
                  disabled={rebuildSearchIndex.isPending}
                  onClick={() => rebuildSearchIndex.mutate()}
                >
                  {rebuildSearchIndex.isPending
                    ? t("searchIndex.rebuilding")
                    : t("searchIndex.rebuild")}
                </Button>
              </div>
              <p className="text-xs font-mono text-muted">
                {rebuildSearchIndex.isSuccess
                  ? t("searchIndex.rebuilt")
                  : t("searchIndex.description")}
              </p>
            </section>

            {/* Network privacy — relay overlay (#455). Synced across devices and
                applied live via set_overlay_mode. */}
            <section className="flex flex-col gap-4 mb-12">
              <h2
                className="text-xs font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line"
              >
                {t("overlay.heading")}
              </h2>
              <div
                role="radiogroup"
                aria-label={t("overlay.ariaLabel")}
                className="flex gap-2 flex-wrap"
              >
                {(["off", "prefer", "strict"] as const).map((value) => (
                  <Button
                    key={value}
                    variant={overlayMode === value ? "primary" : "secondary"}
                    size="sm"
                    aria-label={overlayModeLabels[value]}
                    data-testid={`pref-overlay-mode-${value}`}
                    onClick={() => handleOverlayMode(value)}
                  >
                    {overlayModeLabels[value]}
                  </Button>
                ))}
              </div>
              <p className="text-xs font-mono text-muted">
                {t("overlay.description")}
              </p>
              <ul className="flex flex-col gap-1 text-xs font-mono text-muted">
                <li>
                  <Trans
                    t={t}
                    i18nKey="overlay.itemOff"
                    components={{ mode: <span className="text-dim" /> }}
                  />
                </li>
                <li>
                  <Trans
                    t={t}
                    i18nKey="overlay.itemPrefer"
                    components={{ mode: <span className="text-dim" /> }}
                  />
                </li>
                <li>
                  <Trans
                    t={t}
                    i18nKey="overlay.itemStrict"
                    components={{ mode: <span className="text-dim" /> }}
                  />
                </li>
              </ul>
              {overlayStatus !== null && (
                <p
                  data-testid="pref-overlay-status"
                  className="text-xs font-mono text-danger"
                >
                  {t("overlay.applyError", {
                    error: overlayStatus,
                    mode: overlayModeLabels[overlayMode],
                  })}
                </p>
              )}
            </section>

            {/* Run a relay for others (#813 §10.2) — a SEPARATE consent from
                the mode control above. Never implied by turning the overlay
                on; off by default; conditions default to Wi-Fi + power. */}
            <RelayServingSection
              config={relayServing}
              status={relayStatusQuery.data ?? null}
              applyError={relayServingError}
              onChange={handleRelayServing}
            />

            {/* Voice */}
            <section className="flex flex-col gap-4 mb-12">
              <h2
                className="text-xs font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line"
              >
                {t("voice.heading")}
              </h2>
              <div className="self-start">
                <Button variant="secondary" size="sm" onClick={() => navigate({ to: "/voice-settings" })}>
                  {t("voice.openButton")}
                </Button>
              </div>
            </section>

          </div>
        </div>
      </div>
    </PageShell>
  );
});
