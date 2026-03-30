import React, { useState, useEffect, useCallback } from "react";
import { usePreferences, applyPreferences } from "../hooks/queries/usePreferences";
import { hslToHex, hexToHsl, applyAccentColor, applyBackgroundColor } from "../utils/colorUtils";
import { RangeSlider } from "../components/ui/RangeSlider";
import { Switch } from "../components/ui/Switch";

function getRootVar(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

function isValidHex(val: string): boolean {
  return /^#[0-9a-fA-F]{6}$/.test(val);
}

export const Preferences: React.FC = () => {
  const [hue, setHue] = useState<number>(38);
  const [saturation, setSaturation] = useState<number>(90);
  const [bgHue, setBgHue] = useState<number>(38);
  const [bgSaturation, setBgSaturation] = useState<number>(20);
  const [bgLightness, setBgLightness] = useState<number>(4);
  const [fontSize, setFontSize] = useState<number>(15);
  const [allowDesktopNotifications, setAllowDesktopNotifications] = useState<boolean>(true);
  const [accentHexInput, setAccentHexInput] = useState<string>(() => hslToHex(38, 90, 62));
  const [bgHexInput, setBgHexInput] = useState<string>(() => hslToHex(38, 20, 4));

  const { query, mutation } = usePreferences();

  // Apply saved preferences on first load
  useEffect(() => {
    if (query.data) {
      applyPreferences(query.data);
      if (query.data.allow_desktop_notifications !== undefined) {
        setAllowDesktopNotifications(query.data.allow_desktop_notifications);
      }
    }
  }, [query.data]);

  // Read current CSS var values on mount and sync all state + hex inputs
  useEffect(() => {
    const h = parseInt(getRootVar("--accent-h"));
    const s = parseInt(getRootVar("--accent-s"));
    const bh = parseInt(getRootVar("--bg-h"));
    const bs = parseInt(getRootVar("--bg-s"));
    const bl = parseInt(getRootVar("--bg-l"));
    const fs = parseInt(getRootVar("--font-size-base"));
    if (!isNaN(h)) { setHue(h); }
    if (!isNaN(s)) { setSaturation(s); }
    if (!isNaN(h) && !isNaN(s)) { setAccentHexInput(hslToHex(h, s, 62)); }
    if (!isNaN(bh)) { setBgHue(bh); }
    if (!isNaN(bs)) { setBgSaturation(bs); }
    if (!isNaN(bl)) { setBgLightness(bl); }
    if (!isNaN(bh) && !isNaN(bs) && !isNaN(bl)) { setBgHexInput(hslToHex(bh, bs, bl)); }
    if (!isNaN(fs)) { setFontSize(fs); }
  }, []);

  const save = useCallback((opts: {
    accentH?: number; accentS?: number;
    bgH?: number; bgS?: number; bgL?: number;
    fs?: number; notifications?: boolean;
  }) => {
    const ah = opts.accentH ?? hue;
    const as_ = opts.accentS ?? saturation;
    const bh = opts.bgH ?? bgHue;
    const bs = opts.bgS ?? bgSaturation;
    const bl = opts.bgL ?? bgLightness;
    const fs = opts.fs ?? fontSize;
    const notif = opts.notifications ?? allowDesktopNotifications;
    const accentHex = hslToHex(ah, as_, 62);
    const bgHex = hslToHex(bh, bs, bl);
    mutation.mutate({
      accent_color: accentHex,
      background_color: bgHex,
      font_size: String(fs),
      allow_desktop_notifications: notif,
    });
  }, [mutation, hue, saturation, bgHue, bgSaturation, bgLightness, fontSize, allowDesktopNotifications]);

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

  const handleFontSize = (val: number) => {
    setFontSize(val);
    document.documentElement.style.setProperty("--font-size-base", `${val}px`);
    save({ fs: val });
  };

  const handleAllowDesktopNotifications = (val: boolean) => {
    setAllowDesktopNotifications(val);
    save({ notifications: val });
  };

  return (
    <div
      data-testid="preferences-page"
      className="flex-1 flex flex-col overflow-auto"
      style={{ background: "var(--c-bg)" }}
    >
      <div className="flex-1 flex justify-center overflow-auto px-6 py-8">
        <div className="w-full max-w-md flex flex-col gap-8">

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
                className="text-xs font-mono px-2 py-1 focus:outline-none focus:ring-4 focus:ring-[var(--c-accent)] focus:ring-offset-2 focus:ring-offset-black"
                style={{
                  width: 90,
                  background: "var(--c-surface)",
                  color: isValidHex(accentHexInput) ? "var(--c-text)" : "#ff6b6b",
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
                className="text-xs font-mono px-2 py-1 focus:outline-none focus:ring-4 focus:ring-[var(--c-accent)] focus:ring-offset-2 focus:ring-offset-black"
                style={{
                  width: 90,
                  background: "var(--c-surface)",
                  color: isValidHex(bgHexInput) ? "var(--c-text)" : "#ff6b6b",
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


          {/* Font size */}
          <section className="flex flex-col gap-4 mb-12">
            <h2
              className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b"
              style={{ color: "var(--c-text)", borderColor: "var(--c-border)" }}
            >
              Font Size
            </h2>
            <div className="flex flex-col gap-1.5">
              <RangeSlider
                id="pref-font-size"
                label="Base size — px"
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
            </div>
            <p
              className="font-mono"
              style={{ fontSize, color: "var(--c-text-dim)" }}
            >
              The quick brown fox jumps over the lazy dog.
            </p>
          </section>

        </div>
      </div>
    </div>
  );
};
