import React, { useState, useEffect, useCallback } from "react";
import { useNavigate } from "@tanstack/react-router";
import {
  invoke,
  isPermissionGranted,
  requestPermission,
  setTrayCloseToTray,
  setTrayEnabled,
} from "../bridge";
import { PageShell } from "../components/Layout/PageShell";
import { usePreferences, applyPreferences, applyDeviceFontSize } from "../hooks/queries/usePreferences";
import {
  useMessageRetention,
  useSetMessageRetention,
  MESSAGE_RETENTION_OPTIONS,
} from "../hooks/queries/useMessageRetention";
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

function getRootVar(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

function isValidHex(val: string): boolean {
  return /^#[0-9a-fA-F]{6}$/.test(val);
}

export const PreferencesPage: React.FC = observer(() => {
  const navigate = useNavigate();
  const currentUser = appStore.currentUser;
  const toggleSidebarLabel = useShortcutLabel("app.toggleSidebar");
  const [skin, setSkin] = useState<Skin>("terminal");
  const [hue, setHue] = useState<number>(38);
  const [saturation, setSaturation] = useState<number>(90);
  const [bgHue, setBgHue] = useState<number>(38);
  const [bgSaturation, setBgSaturation] = useState<number>(20);
  const [bgLightness, setBgLightness] = useState<number>(4);
  const [fontSize, setFontSize] = useState<number>(15);
  const [allowDesktopNotifications, setAllowDesktopNotifications] = useState<boolean>(true);
  const [allowSoundEffects, setAllowSoundEffects] = useState<boolean>(true);
  const [allowCallRingtone, setAllowCallRingtone] = useState<boolean>(true);
  const [sidebarOpenByDefault, setSidebarOpenByDefault] = useState<boolean>(true);
  const [closeToTray, setCloseToTray] = useState<boolean>(true);
  const [menubarIcon, setMenubarIcon] = useState<boolean>(false);
  const [accentHexInput, setAccentHexInput] = useState<string>(() => hslToHex(38, 90, 62));
  const [bgHexInput, setBgHexInput] = useState<string>(() => hslToHex(38, 20, 4));

  const { query, save: savePrefs } = usePreferences();

  // Device-local message retention window (see useMessageRetention). Selecting
  // an option fires the mutation immediately — the backend sweep is immediate.
  const retentionQuery = useMessageRetention();
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
      if (query.data.sidebar_open_by_default !== undefined) {
        setSidebarOpenByDefault(query.data.sidebar_open_by_default);
      }
      if (query.data.close_to_tray !== undefined) {
        setCloseToTray(query.data.close_to_tray);
      }
      if (query.data.menubar_icon !== undefined) {
        setMenubarIcon(query.data.menubar_icon);
      }
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
    sidebarOpenByDefault?: boolean;
    closeToTray?: boolean;
    menubarIcon?: boolean;
  }) => {
    const ah = opts.accentH ?? hue;
    const as_ = opts.accentS ?? saturation;
    const bh = opts.bgH ?? bgHue;
    const bs = opts.bgS ?? bgSaturation;
    const bl = opts.bgL ?? bgLightness;
    const notif = opts.notifications ?? allowDesktopNotifications;
    const sfx = opts.soundEffects ?? allowSoundEffects;
    const sidebar = opts.sidebarOpenByDefault ?? sidebarOpenByDefault;
    const tray = opts.closeToTray ?? closeToTray;
    const menubar = opts.menubarIcon ?? menubarIcon;
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
      sidebar_open_by_default: sidebar,
      close_to_tray: tray,
      menubar_icon: menubar,
    });
  }, [savePrefs, query.data, hue, saturation, bgHue, bgSaturation, bgLightness, skin, allowDesktopNotifications, allowSoundEffects, sidebarOpenByDefault, closeToTray, menubarIcon]);

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

  const handleSidebarOpenByDefault = (val: boolean) => {
    setSidebarOpenByDefault(val);
    save({ sidebarOpenByDefault: val });
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
    <PageShell title="Preferences" scrollable>
      <div
        data-testid="preferences-page"
        className="flex-1 flex flex-col overflow-auto"
        style={{ background: "var(--c-bg)" }}
      >
        <div className="flex-1 flex justify-center overflow-auto px-6 py-8">
          <div className="w-full max-w-md flex flex-col gap-8">

            {/* Appearance — UI skin (synced across devices) */}
            <section className="flex flex-col gap-4 mb-12">
              <h2
                className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b"
                style={{ color: "var(--c-text)", borderColor: "var(--c-border)" }}
              >
                Appearance
              </h2>
              <div
                role="radiogroup"
                aria-label="UI skin"
                className="flex gap-2 flex-wrap"
              >
                {([
                  { value: "terminal", label: "Terminal" },
                  { value: "refined", label: "Refined" },
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
              <p className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
                Terminal is the default IRC/monospace look. Refined is a
                friendlier, proportional-sans layout for people who prefer a
                more conventional chat app. Syncs across your devices.
              </p>
            </section>

            {/* Accent Color */}
            <section className="flex flex-col gap-4 mb-12">
              <h2
                className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b"
                style={{ color: "var(--c-text)", borderColor: "var(--c-border)" }}
              >
                Accent Color
              </h2>

              <div className="flex items-center gap-2">
                <label
                  className="flex-shrink-0 cursor-pointer overflow-hidden focus-within:ring-4 focus-within:ring-[var(--c-accent)] focus-within:ring-offset-2 focus-within:ring-offset-black"
                  style={{ width: 40, height: 40, borderRadius: 8, padding: 0 }}
                  title="Pick accent color"
                >
                  <input
                    type="color"
                    value={hslToHex(hue, saturation, 62)}
                    onChange={(e) => handleAccentColor(e.target.value)}
                    style={{ width: "150%", height: "150%", margin: "-25%", border: "none", padding: 0, cursor: "pointer" }}
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
                  className="text-xs font-mono font-machine px-2 py-1 focus:outline-none focus:ring-4 focus:ring-[var(--c-accent)] focus:ring-offset-2 focus:ring-offset-black"
                  style={{
                    width: 90,
                    background: "var(--c-surface)",
                    color: isValidHex(accentHexInput) ? "var(--c-text)" : "var(--c-danger)",
                    border: "1px solid var(--c-border)",
                    borderRadius: 6,
                  }}
                />
              </div>

              {/* Quick presets */}
              <div className="flex gap-2 flex-wrap">
                {[
                  { label: "Orange", h: 38, s: 90 },
                  { label: "Green", h: 150, s: 62 },
                  { label: "Blue", h: 210, s: 80 },
                  { label: "Purple", h: 270, s: 70 },
                  { label: "Red", h: 0, s: 85 },
                  { label: "Cyan", h: 185, s: 75 },
                ].map((preset) => (
                  <button
                    key={preset.label}
                    onClick={() => {
                      setHue(preset.h);
                      setSaturation(preset.s);
                      const hex = hslToHex(preset.h, preset.s, 62);
                      setAccentHexInput(hex);
                      applyAccentColor(hex);
                      save({ accentH: preset.h, accentS: preset.s });
                    }}
                    className="px-2 py-0.5 text-xs font-mono transition-colors focus:outline-none focus:ring-4 focus:ring-[var(--c-accent)] focus:ring-offset-2 focus:ring-offset-black"
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
                className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b"
                style={{ color: "var(--c-text)", borderColor: "var(--c-border)" }}
              >
                Background Color
              </h2>

              <div className="flex items-center gap-2">
                <label
                  className="flex-shrink-0 cursor-pointer overflow-hidden focus-within:ring-4 focus-within:ring-[var(--c-accent)] focus-within:ring-offset-2 focus-within:ring-offset-black"
                  style={{ width: 40, height: 40, padding: 0, borderRadius: "0.5rem", outline: "2px solid var(--c-accent)", outlineOffset: "-1px" }}
                  title="Pick background color"
                >
                  <input
                    type="color"
                    value={hslToHex(bgHue, bgSaturation, bgLightness)}
                    onChange={(e) => handleBgColor(e.target.value)}
                    style={{ width: "150%", height: "150%", margin: "-25%", border: "none", padding: 0, cursor: "pointer" }}
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
                  className="text-xs font-mono font-machine px-2 py-1 focus:outline-none focus:ring-4 focus:ring-[var(--c-accent)] focus:ring-offset-2 focus:ring-offset-black"
                  style={{
                    width: 90,
                    background: "var(--c-surface)",
                    color: isValidHex(bgHexInput) ? "var(--c-text)" : "var(--c-danger)",
                    border: "1px solid var(--c-border)",
                    borderRadius: 6,
                  }}
                />
              </div>

              {/* Quick presets */}
              <div className="flex gap-2 flex-wrap">
                {[
                  { label: "Match accent", h: hue, s: 20 },
                  { label: "Neutral", h: 0, s: 0 },
                  { label: "Warm", h: 30, s: 15 },
                  { label: "Cool", h: 220, s: 15 },
                  { label: "Green", h: 150, s: 12 },
                  { label: "Purple", h: 270, s: 12 },
                ].map((preset) => (
                  <button
                    key={preset.label}
                    onClick={() => {
                      setBgHue(preset.h);
                      setBgSaturation(preset.s);
                      setBgLightness(7);
                      const hex = hslToHex(preset.h, preset.s, 7);
                      setBgHexInput(hex);
                      applyBackgroundColor(hex);
                      save({ bgH: preset.h, bgS: preset.s, bgL: 7 });
                    }}
                    className="px-2 py-0.5 text-xs font-mono transition-colors focus:outline-none focus:ring-4 focus:ring-[var(--c-accent)] focus:ring-offset-2 focus:ring-offset-black"
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
                className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b"
                style={{ color: "var(--c-text)", borderColor: "var(--c-border)" }}
              >
                Display (this device)
              </h2>
              <div className="flex flex-col gap-1.5">
                <RangeSlider
                  id="pref-font-size"
                  label="Font size — px"
                  value={fontSize}
                  min={12}
                  max={20}
                  step={1}
                  onChange={handleFontSize}
                />
                <div className="flex justify-between text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
                  <span>12px small</span>
                  <span>16px normal</span>
                  <span>20px large</span>
                </div>
                <p className="text-xs font-mono mt-1" style={{ color: "var(--c-text-muted)" }}>
                  Font size is per-device — it won't sync to your other devices.
                </p>
              </div>
              <p
                className="font-mono"
                style={{ fontSize, color: "var(--c-text-dim)" }}
              >
                The quick brown fox jumps over the lazy dog.
              </p>
              <div className="flex flex-col gap-1.5 mt-4">
                <Switch
                  id="pref-call-ringtone"
                  label="Incoming call ringtone"
                  checked={allowCallRingtone}
                  onChange={handleAllowCallRingtone}
                />
                <p className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
                  Plays a looping ring on this device when someone calls. Off here doesn't mute the alert badge or your other devices.
                </p>
              </div>
            </section>

            {/* Layout */}
            <section className="flex flex-col gap-4 mb-12">
              <h2
                className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b"
                style={{ color: "var(--c-text)", borderColor: "var(--c-border)" }}
              >
                Layout
              </h2>
              <div className="flex flex-col gap-1.5">
                <Switch
                  id="pref-sidebar-default"
                  label="Show sidebar by default"
                  checked={sidebarOpenByDefault}
                  onChange={handleSidebarOpenByDefault}
                />
                <p className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
                  Controls whether the left sidebar is open when the app starts. Toggle ad-hoc with {toggleSidebarLabel}.
                </p>
              </div>
              {!isMac && (
                <div className="flex flex-col gap-1.5">
                  <Switch
                    id="pref-close-to-tray"
                    label="Close to tray"
                    checked={closeToTray}
                    onChange={handleCloseToTray}
                  />
                  <p className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
                    When on, closing the window hides Pollis to the system tray instead of quitting. Right-click the tray icon to quit. If your desktop environment doesn't show tray icons (bare GNOME without the AppIndicator extension), turn this off.
                  </p>
                </div>
              )}
              {isMac && (
                <div className="flex flex-col gap-1.5">
                  <Switch
                    id="pref-menubar-icon"
                    label="Show menu bar icon"
                    checked={menubarIcon}
                    onChange={handleMenubarIcon}
                  />
                  <p className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
                    Adds a Pollis icon to the macOS menu bar (top right). Click it to open the window, mute the mic while in a call, or quit the app. Off by default — the dock icon already keeps Pollis reachable when the window is closed.
                  </p>
                </div>
              )}
            </section>

            {/* Notifications */}
            <section className="flex flex-col gap-4 mb-12">
              <h2
                className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b"
                style={{ color: "var(--c-text)", borderColor: "var(--c-border)" }}
              >
                Notifications
              </h2>
              <Switch
                id="pref-notifications"
                label="Desktop notifications"
                checked={allowDesktopNotifications}
                onChange={handleAllowDesktopNotifications}
              />
              <Switch
                id="pref-sound-effects"
                label="Sound effects"
                checked={allowSoundEffects}
                onChange={handleAllowSoundEffects}
              />
            </section>

            {/* Local message history (this device) — device-local retention
                window stored in the local DB, not synced across the account. */}
            <section className="flex flex-col gap-4 mb-12">
              <h2
                className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b"
                style={{ color: "var(--c-text)", borderColor: "var(--c-border)" }}
              >
                Local message history
              </h2>
              <div
                role="radiogroup"
                aria-label="Local message history retention"
                className="flex gap-2 flex-wrap"
              >
                {MESSAGE_RETENTION_OPTIONS.map((option) => {
                  const selected = retentionDays === option.days;
                  return (
                    <Button
                      key={option.days}
                      variant={selected ? "primary" : "secondary"}
                      size="sm"
                      aria-label={option.label}
                      data-testid={`pref-retention-${option.days}`}
                      onClick={() => {
                        if (selected) {
                          return;
                        }
                        setRetention.mutate(option.days);
                      }}
                    >
                      {option.label}
                    </Button>
                  );
                })}
              </div>
              <p className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
                Controls how much message history is kept on this device. Older
                messages are deleted from local storage to save space. This does
                not affect your other devices or the people you're talking to, and
                you'll still receive new messages normally.
              </p>
            </section>

            {/* Voice */}
            <section className="flex flex-col gap-4 mb-12">
              <h2
                className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b"
                style={{ color: "var(--c-text)", borderColor: "var(--c-border)" }}
              >
                Voice
              </h2>
              <div className="self-start">
                <Button variant="secondary" size="sm" onClick={() => navigate({ to: "/voice-settings" })}>
                  Voice & Video
                </Button>
              </div>
            </section>

          </div>
        </div>
      </div>
    </PageShell>
  );
});
