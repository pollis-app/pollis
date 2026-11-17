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
import { PageColumn } from "../components/ui/PageColumn";
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
import { HexColorField } from "../components/Preferences/HexColorField";
import {
  applyAccentColor,
  applyBackgroundColor,
  applyFontSize,
  applySkin,
  normalizeSkin,
  DEFAULT_SKIN,
  loadDeviceFontSize,
  saveDeviceFontSize,
  type Skin,
} from "../utils/colorUtils";
import {
  ACCENT_PRESETS,
  BACKGROUND_PRESETS,
  DEFAULT_ACCENT_HEX,
  DEFAULT_BACKGROUND_HEX,
  accentFromPrefs,
  backgroundFromPrefs,
  colorPreferencesPatch,
} from "../../../design/theme";
import { RangeSlider } from "../components/ui/RangeSlider";
import { Switch } from "../components/ui/Switch";
import { Button } from "../components/ui/Button";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";
import { loadDeviceCallRingtone, saveDeviceCallRingtone } from "../utils/notify";
import { INCOMING_CALL_TAKEOVER_DEFAULT } from "../utils/incomingCallTakeover";
import { isMac } from "../utils/platform";
import { useShortcutLabel } from "../keyboard";
import { useRightPanel } from "../components/Layout/RightPanel/useRightPanel";
import { SkinPicker } from "../components/Preferences/SkinPicker";
import { Chip } from "../components/ui/Chip";

function getRootVar(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

// Absent `relay_serving*` fields fall back to RELAY_SERVING_DEFAULTS.
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
  // Live panel state, which the switch below reports.
  const { isOpen: isRightPanelOpen, setPanel: setRightPanel } = useRightPanel();
  const [skin, setSkin] = useState<Skin>(DEFAULT_SKIN);
  // Hex, exactly as synced (design/theme.ts): no lightness is forced on a pick.
  const [accentHex, setAccentHex] = useState<string>(DEFAULT_ACCENT_HEX);
  const [bgHex, setBgHex] = useState<string>(DEFAULT_BACKGROUND_HEX);
  const [fontSize, setFontSize] = useState<number>(15);
  const [allowDesktopNotifications, setAllowDesktopNotifications] = useState<boolean>(true);
  const [allowSoundEffects, setAllowSoundEffects] = useState<boolean>(true);
  const [incomingCallTakeover, setIncomingCallTakeover] = useState<boolean>(INCOMING_CALL_TAKEOVER_DEFAULT);
  const [sendReadReceipts, setSendReadReceipts] = useState<boolean>(true);
  const [allowCallRingtone, setAllowCallRingtone] = useState<boolean>(true);
  const [sidebarOpenByDefault, setSidebarOpenByDefault] = useState<boolean>(true);
  // `undefined` until touched, so a new device seeds from the skin (#904).
  const [rightPanelOpenByDefault, setRightPanelOpenByDefault] = useState<
    boolean | undefined
  >(undefined);
  const [closeToTray, setCloseToTray] = useState<boolean>(true);
  const [menubarIcon, setMenubarIcon] = useState<boolean>(false);
  const [overlayMode, setOverlayMode] = useState<OverlayMode>("off");
  // Apply errors (e.g. Strict with no relay) show here.
  const [overlayStatus, setOverlayStatus] = useState<string | null>(null);
  // "Be a relay" consent (#813 §10.2), separate from `overlayMode`.
  const [relayServing, setRelayServing] = useState<RelayServingConfig>(RELAY_SERVING_DEFAULTS);
  const [relayServingError, setRelayServingError] = useState<string | null>(null);
  const [accentHexInput, setAccentHexInput] = useState<string>(DEFAULT_ACCENT_HEX);
  const [bgHexInput, setBgHexInput] = useState<string>(DEFAULT_BACKGROUND_HEX);

  const { query, save: savePrefs } = usePreferences();

  const relayStatusQuery = useRelayServingStatus();
  const applyRelayServing = useApplyRelayServing();

  const retentionQuery = useMessageRetention();
  const rebuildSearchIndex = useRebuildSearchIndex();
  const setRetention = useSetMessageRetention();
  const retentionDays = retentionQuery.data ?? MESSAGE_RETENTION_OPTIONS[0].days;

  useEffect(() => {
    if (query.data) {
      applyPreferences(query.data);
      applyDeviceFontSize(currentUser?.id, query.data);
      setSkin(normalizeSkin(query.data.skin));
      const accent = accentFromPrefs(query.data) ?? DEFAULT_ACCENT_HEX;
      setAccentHex(accent);
      setAccentHexInput(accent);
      const background = backgroundFromPrefs(query.data) ?? DEFAULT_BACKGROUND_HEX;
      setBgHex(background);
      setBgHexInput(background);
      if (query.data.allow_desktop_notifications !== undefined) {
        setAllowDesktopNotifications(query.data.allow_desktop_notifications);
      }
      if (query.data.allow_sound_effects !== undefined) {
        setAllowSoundEffects(query.data.allow_sound_effects);
      }
      if (query.data.incoming_call_takeover !== undefined) {
        setIncomingCallTakeover(query.data.incoming_call_takeover);
      }
      if (query.data.send_read_receipts !== undefined) {
        setSendReadReceipts(query.data.send_read_receipts);
      }
      if (query.data.sidebar_open_by_default !== undefined) {
        setSidebarOpenByDefault(query.data.sidebar_open_by_default);
      }
      // No `!== undefined` guard: absent is meaningful here.
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

  useEffect(() => {
    // Device-local font size, else the CSS var's current value.
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
    accent?: string;
    background?: string;
    skin?: Skin;
    notifications?: boolean; soundEffects?: boolean;
    incomingCallTakeover?: boolean;
    sendReadReceipts?: boolean;
    sidebarOpenByDefault?: boolean;
    rightPanelOpenByDefault?: boolean;
    closeToTray?: boolean;
    menubarIcon?: boolean;
    overlayMode?: OverlayMode;
    relayServing?: RelayServingConfig;
  }) => {
    const accent = opts.accent ?? accentHex;
    const background = opts.background ?? bgHex;
    const notif = opts.notifications ?? allowDesktopNotifications;
    const sfx = opts.soundEffects ?? allowSoundEffects;
    const takeover = opts.incomingCallTakeover ?? incomingCallTakeover;
    const receipts = opts.sendReadReceipts ?? sendReadReceipts;
    const sidebar = opts.sidebarOpenByDefault ?? sidebarOpenByDefault;
    const rightPanel = opts.rightPanelOpenByDefault ?? rightPanelOpenByDefault;
    const tray = opts.closeToTray ?? closeToTray;
    const menubar = opts.menubarIcon ?? menubarIcon;
    const overlay = opts.overlayMode ?? overlayMode;
    const relay = opts.relayServing ?? relayServing;
    const skinVal = opts.skin ?? skin;
    // `font_size` is device-local; delete the legacy synced field so saves don't overwrite it.
    savePrefs({
      ...query.data,
      font_size: undefined,
      // `accent_color` + `background_color`, and the legacy `accent_hex` older mobile builds read.
      ...colorPreferencesPatch({ accent, background }),
      skin: skinVal,
      allow_desktop_notifications: notif,
      allow_sound_effects: sfx,
      incoming_call_takeover: takeover,
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
  }, [savePrefs, query.data, accentHex, bgHex, skin, allowDesktopNotifications, allowSoundEffects, incomingCallTakeover, sendReadReceipts, sidebarOpenByDefault, closeToTray, menubarIcon, overlayMode, relayServing]);

  // Apply `val` live, never throwing: errors show inline and the control snaps to the real mode.
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
      }
      setOverlayMode(actual);
      setOverlayStatus(msg);
      if (persist) {
        save({ overlayMode: actual });
      }
    }
  }, [save]);

  // Re-apply the saved mode on login, only if it differs from the running one.
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
      }
      if (!cancelled) {
        await applyOverlayMode(desired, false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [query.data?.overlay_mode, applyOverlayMode]);

  // Translated labels for the wire values.
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

  // A failed apply keeps the toggle: consent is the user's, and the status line says so.
  const applyRelayServingConfig = useCallback(async (next: RelayServingConfig, persist: boolean) => {
    try {
      const status = await applyRelayServing(next);
      setRelayServingError(null);
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

  // Re-apply saved consent on login, guarded on the live config.
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
    const normalized = hex.toLowerCase();
    setAccentHex(normalized);
    setAccentHexInput(normalized);
    applyAccentColor(normalized);
    save({ accent: normalized });
  };

  const handleBgColor = (hex: string) => {
    const normalized = hex.toLowerCase();
    setBgHex(normalized);
    setBgHexInput(normalized);
    applyBackgroundColor(normalized);
    save({ background: normalized });
  };

  // One literal call per key, so the i18n check sees every one.
  const accentPresetLabel = (n: (typeof ACCENT_PRESETS)[number]["n"]): string => {
    switch (n) {
      case "Amber":
        return t("accent.presetAmber");
      case "Citron":
        return t("accent.presetCitron");
      case "Mint":
        return t("accent.presetMint");
      case "Glass":
        return t("accent.presetGlass");
      case "Lilac":
        return t("accent.presetLilac");
      case "Rust":
        return t("accent.presetRust");
    }
  };

  const backgroundPresetLabel = (n: (typeof BACKGROUND_PRESETS)[number]["n"]): string => {
    switch (n) {
      case "Warm":
        return t("background.presetWarm");
      case "Neutral":
        return t("background.presetNeutral");
      case "Cool":
        return t("background.presetCool");
      case "Green":
        return t("background.presetGreen");
      case "Purple":
        return t("background.presetPurple");
    }
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

  const handleIncomingCallTakeover = (val: boolean) => {
    setIncomingCallTakeover(val);
    save({ incomingCallTakeover: val });
  };

  const handleSendReadReceipts = (val: boolean) => {
    setSendReadReceipts(val);
    save({ sendReadReceipts: val });
  };

  const handleSidebarOpenByDefault = (val: boolean) => {
    setSidebarOpenByDefault(val);
    save({ sidebarOpenByDefault: val });
  };

  // Device-local opens the panel now; the synced value seeds new devices.
  const handleRightPanelOpenByDefault = (val: boolean) => {
    setRightPanelOpenByDefault(val);
    setRightPanel(val ? "members" : "none");
    save({ rightPanelOpenByDefault: val });
  };

  const handleCloseToTray = (val: boolean) => {
    setCloseToTray(val);
    save({ closeToTray: val });
    // Apply now rather than after the throttled save.
    void setTrayCloseToTray(val).catch((err) => {
      console.warn("[tray] setTrayCloseToTray failed:", err);
    });
  };

  const handleMenubarIcon = (val: boolean) => {
    setMenubarIcon(val);
    save({ menubarIcon: val });
    // Apply now rather than after the throttled save.
    void setTrayEnabled(val).catch((err) => {
      console.warn("[tray] setTrayEnabled failed:", err);
    });
  };

  const handleAllowCallRingtone = (val: boolean) => {
    setAllowCallRingtone(val);
    saveDeviceCallRingtone(currentUser?.id, val);
  };

  // Translated here: the options are module-level data.
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
    if (val) {
      try {
        const granted = await isPermissionGranted();
        if (!granted) {
          await requestPermission();
        }
      } catch {
      }
    }
  };

  return (
    <PageShell title={t("preferences.title")} scrollable>
      <PageColumn spacing="sections" paneTestId="preferences-page">
        {/* Language (this device). Deliberately first: someone who cannot
            read the UI needs the way to fix that at the top. One row; the
            list is its own page (#1944). */}
        <LanguageSection />

        {/* Appearance — UI skin (synced across devices) */}
        <section className="flex flex-col gap-4 mb-12">
          <h2
            className="text-secondary font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line"
          >
            {t("appearance.heading")}
          </h2>
          <SkinPicker
            data-testid="pref-skin"
            value={skin}
            onChange={(val) => {
              if (skin !== val) {
                handleSkin(val);
              }
            }}
          />
          <p className="text-secondary font-mono text-muted">
            {t("appearance.description")}
          </p>
        </section>

        {/* Accent Color */}
        <section className="flex flex-col gap-4 mb-12">
          <h2
            className="text-secondary font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line"
          >
            {t("accent.heading")}
          </h2>

          <HexColorField
            color={accentHex}
            hexInput={accentHexInput}
            onHexInputChange={setAccentHexInput}
            onColor={handleAccentColor}
            title={t("accent.pickTitle")}
          />

          {/* Quick presets: the same list mobile offers (design/theme.ts). */}
          <div className="flex gap-2 flex-wrap">
            {ACCENT_PRESETS.map((preset) => (
              <Chip
                key={preset.n}
                data-testid={`pref-accent-${preset.n.toLowerCase()}`}
                selected={accentHex === preset.c}
                aria-pressed={accentHex === preset.c}
                onClick={() => handleAccentColor(preset.c)}
                // Each preset previews its own colour, a runtime value.
                style={{
                  background: `${preset.c}26`,
                  borderColor: accentHex === preset.c ? preset.c : `${preset.c}66`,
                  color: preset.c,
                }}
              >
                {accentPresetLabel(preset.n)}
              </Chip>
            ))}
          </div>
        </section>

        {/* Background Color */}
        <section className="flex flex-col gap-4 mb-12">
          <h2
            className="text-secondary font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line"
          >
            {t("background.heading")}
          </h2>

          <HexColorField
            color={bgHex}
            hexInput={bgHexInput}
            onHexInputChange={setBgHexInput}
            onColor={handleBgColor}
            title={t("background.pickTitle")}
            outlined
          />

          {/* Quick presets (design/theme.ts); every one passes the contrast test. */}
          <div className="flex gap-2 flex-wrap">
            {BACKGROUND_PRESETS.map((preset) => (
              <Chip
                key={preset.n}
                data-testid={`pref-background-${preset.n.toLowerCase()}`}
                selected={bgHex === preset.c}
                aria-pressed={bgHex === preset.c}
                onClick={() => handleBgColor(preset.c)}
                // Each preset previews its own colour, a runtime value.
                style={{ background: preset.c }}
              >
                {backgroundPresetLabel(preset.n)}
              </Chip>
            ))}
          </div>
        </section>


        {/* Display (this device) — settings here are stored on this device only,
            not synced across the user's account. Future device-specific items
            should slot in here. */}
        <section className="flex flex-col gap-4 mb-12">
          <h2
            className="text-secondary font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line"
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
            <div className="flex justify-between text-secondary font-mono text-muted">
              <span>{t("display.fontSizeSmall")}</span>
              <span>{t("display.fontSizeNormal")}</span>
              <span>{t("display.fontSizeLarge")}</span>
            </div>
            <p className="text-secondary font-mono mt-1 text-muted">
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
            <p className="text-secondary font-mono text-muted">
              {t("display.callRingtoneDescription")}
            </p>
          </div>
        </section>

        {/* Layout */}
        <section className="flex flex-col gap-4 mb-12">
          <h2
            className="text-secondary font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line"
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
            <p className="text-secondary font-mono text-muted">
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
            <p className="text-secondary font-mono text-muted">
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
              <p className="text-secondary font-mono text-muted">
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
              <p className="text-secondary font-mono text-muted">
                {t("layout.menubarIconDescription")}
              </p>
            </div>
          )}
        </section>

        {/* Notifications */}
        <section className="flex flex-col gap-4 mb-12">
          <h2
            className="text-secondary font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line"
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
          <div className="flex flex-col gap-1.5">
            <Switch
              id="pref-incoming-call-takeover"
              label={t("notifications.callTakeoverLabel")}
              checked={incomingCallTakeover}
              onChange={handleIncomingCallTakeover}
            />
            <p className="text-secondary font-mono text-muted">
              {t("notifications.callTakeoverDescription")}
            </p>
          </div>
        </section>

        {/* Read receipts (#857) — DMs only. */}
        <section className="flex flex-col gap-4 mb-12">
          <h2
            className="text-secondary font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line"
          >
            {t("readReceipts.heading")}
          </h2>
          <Switch
            id="pref-read-receipts"
            label={t("readReceipts.toggleLabel")}
            checked={sendReadReceipts}
            onChange={handleSendReadReceipts}
          />
          <p className="text-secondary font-mono text-muted">
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
            className="text-secondary font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line"
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
          <p className="text-secondary font-mono text-muted">
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
          <h2 className="text-secondary font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line">
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
          <p className="text-secondary font-mono text-muted">
            {rebuildSearchIndex.isSuccess
              ? t("searchIndex.rebuilt")
              : t("searchIndex.description")}
          </p>
        </section>

        {/* Network privacy — relay overlay (#455). Synced across devices and
            applied live via set_overlay_mode. */}
        <section className="flex flex-col gap-4 mb-12">
          <h2
            className="text-secondary font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line"
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
          <p className="text-secondary font-mono text-muted">
            {t("overlay.description")}
          </p>
          <ul className="flex flex-col gap-1 text-secondary font-mono text-muted">
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
              className="text-secondary font-mono text-danger"
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
            className="text-secondary font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line"
          >
            {t("voice.heading")}
          </h2>
          <div className="self-start">
            <Button variant="secondary" size="sm" activateOn="press" onClick={() => navigate({ to: "/voice-settings" })}>
              {t("voice.openButton")}
            </Button>
          </div>
        </section>
      </PageColumn>
    </PageShell>
  );
});
