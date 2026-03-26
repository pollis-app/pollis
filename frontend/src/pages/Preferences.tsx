import React, { useState, useEffect, useCallback } from "react";
import { usePreferences, applyPreferences } from "../hooks/queries/usePreferences";
import { hslToHex } from "../utils/colorUtils";
import { RangeSlider } from "../components/ui/RangeSlider";
import { Switch } from "../components/ui/Switch";

function getRootVar(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

function setRootVar(name: string, value: string) {
  document.documentElement.style.setProperty(name, value);
}

export const Preferences: React.FC = () => {
  const [hue, setHue] = useState<number>(38);
  const [saturation, setSaturation] = useState<number>(90);
  const [bgHue, setBgHue] = useState<number>(38);
  const [bgSaturation, setBgSaturation] = useState<number>(20);
  const [fontSize, setFontSize] = useState<number>(15);
  const [allowDesktopNotifications, setAllowDesktopNotifications] = useState<boolean>(true);

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

  // Read current CSS var values on mount
  useEffect(() => {
    const h = parseInt(getRootVar("--accent-h"));
    const s = parseInt(getRootVar("--accent-s"));
    const bh = parseInt(getRootVar("--bg-h"));
    const bs = parseInt(getRootVar("--bg-s"));
    const fs = parseInt(getRootVar("--font-size-base"));
    if (!isNaN(h)) { setHue(h); }
    if (!isNaN(s)) { setSaturation(s); }
    if (!isNaN(bh)) { setBgHue(bh); }
    if (!isNaN(bs)) { setBgSaturation(bs); }
    if (!isNaN(fs)) { setFontSize(fs); }
  }, []);

  const save = useCallback((opts: {
    accentH?: number; accentS?: number;
    bgH?: number; bgS?: number;
    fs?: number; notifications?: boolean;
  }) => {
    const ah = opts.accentH ?? hue;
    const as_ = opts.accentS ?? saturation;
    const bh = opts.bgH ?? bgHue;
    const bs = opts.bgS ?? bgSaturation;
    const fs = opts.fs ?? fontSize;
    const notif = opts.notifications ?? allowDesktopNotifications;
    const accentHex = hslToHex(ah, as_, 62);
    const bgHex = hslToHex(bh, bs, 20);
    mutation.mutate({
      accent_color: accentHex,
      background_color: bgHex,
      font_size: String(fs),
      allow_desktop_notifications: notif,
    });
  }, [mutation, hue, saturation, bgHue, bgSaturation, fontSize, allowDesktopNotifications]);

  const handleHue = (val: number) => {
    setHue(val);
    setRootVar("--accent-h", String(val));
    save({ accentH: val });
  };

  const handleSaturation = (val: number) => {
    setSaturation(val);
    setRootVar("--accent-s", `${val}%`);
    save({ accentS: val });
  };

  const handleBgHue = (val: number) => {
    setBgHue(val);
    setRootVar("--bg-h", String(val));
    save({ bgH: val });
  };

  const handleBgSaturation = (val: number) => {
    setBgSaturation(val);
    setRootVar("--bg-s", `${val}%`);
    save({ bgS: val });
  };

  const handleFontSize = (val: number) => {
    setFontSize(val);
    setRootVar("--font-size-base", `${val}px`);
    save({ fs: val });
  };

  const handleAllowDesktopNotifications = (val: boolean) => {
    setAllowDesktopNotifications(val);
    save({ notifications: val });
  };

  const previewColor = `hsl(${hue} ${saturation}% 62%)`;
  const previewBgColor = `hsl(${bgHue} ${bgSaturation}% 7%)`;

  return (
    <div
      data-testid="preferences-page"
      className="flex-1 flex flex-col overflow-auto"
      style={{ background: "var(--c-bg)" }}
    >
      <div className="flex-1 flex justify-center overflow-auto px-6 py-8">
        <div className="w-full max-w-md flex flex-col gap-8">

          {/* Color */}
          <section className="flex flex-col gap-4 mb-12">
            <h2
              className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b"
              style={{ color: "var(--c-text)", borderColor: "var(--c-border)" }}
            >
              Accent Color
            </h2>

            <div className="flex items-center gap-3">
              <div
                className="w-8 h-8 rounded-sm flex-shrink-0"
                style={{ background: previewColor, border: "1px solid var(--c-border)" }}
              />
              <span className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
                hsl({hue} {saturation}% 62%)
              </span>
            </div>

            <div className="flex flex-col gap-3">
              <div className="flex flex-col gap-1.5">
                <RangeSlider
                  id="pref-hue"
                  label={`Hue — °`}
                  value={hue}
                  min={0}
                  max={360}
                  onChange={handleHue}
                />
                {/* Quick presets */}
                <div className="flex gap-2 flex-wrap mt-1">
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
                      onClick={() => { handleHue(preset.h); handleSaturation(preset.s); }}
                      className="px-2 py-0.5 text-xs font-mono transition-colors"
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
              </div>

              <RangeSlider
                id="pref-saturation"
                label="Saturation — %"
                value={saturation}
                min={20}
                max={100}
                onChange={handleSaturation}
              />
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

            <div className="flex items-center gap-3">
              <div
                className="w-8 h-8 rounded-sm flex-shrink-0"
                style={{ background: previewBgColor, border: "1px solid var(--c-border)" }}
              />
              <span className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
                hsl({bgHue} {bgSaturation}% 7%)
              </span>
            </div>

            <div className="flex flex-col gap-3">
              <div className="flex flex-col gap-1.5">
                <RangeSlider
                  id="pref-bg-hue"
                  label={`Hue — °`}
                  value={bgHue}
                  min={0}
                  max={360}
                  onChange={handleBgHue}
                />
                {/* Quick presets */}
                <div className="flex gap-2 flex-wrap mt-1">
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
                      onClick={() => { handleBgHue(preset.h); handleBgSaturation(preset.s); }}
                      className="px-2 py-0.5 text-xs font-mono transition-colors"
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
              </div>

              <RangeSlider
                id="pref-bg-saturation"
                label="Saturation — %"
                value={bgSaturation}
                min={0}
                max={40}
                onChange={handleBgSaturation}
              />
            </div>
          </section>

          {/* Notifications */}
          {/* <section className="flex flex-col gap-4 mb-12">
            <h2
              className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b"
              style={{ color: "var(--c-text)", borderColor: "var(--c-border)" }}
            >
              Notifications
            </h2>
            <Switch
              id="pref-desktop-notifications"
              label="Desktop notifications"
              checked={allowDesktopNotifications}
              onChange={handleAllowDesktopNotifications}
              description="Show a system notification when a message arrives in an unfocused window"
            />
          </section> */}

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
