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
    const fs = parseInt(getRootVar("--font-size-base"));
    if (!isNaN(h)) { setHue(h); }
    if (!isNaN(s)) { setSaturation(s); }
    if (!isNaN(fs)) { setFontSize(fs); }
  }, []);

  const save = useCallback((newHue: number, newSat: number, newFs: number, newNotifications: boolean) => {
    const accentHex = hslToHex(newHue, newSat, 62);
    mutation.mutate({ accent_color: accentHex, font_size: String(newFs), allow_desktop_notifications: newNotifications });
  }, [mutation]);

  const handleHue = (val: number) => {
    setHue(val);
    setRootVar("--accent-h", String(val));
    save(val, saturation, fontSize, allowDesktopNotifications);
  };

  const handleSaturation = (val: number) => {
    setSaturation(val);
    setRootVar("--accent-s", `${val}%`);
    save(hue, val, fontSize, allowDesktopNotifications);
  };

  const handleFontSize = (val: number) => {
    setFontSize(val);
    setRootVar("--font-size-base", `${val}px`);
    save(hue, saturation, val, allowDesktopNotifications);
  };

  const handleAllowDesktopNotifications = (val: boolean) => {
    setAllowDesktopNotifications(val);
    save(hue, saturation, fontSize, val);
  };

  const previewColor = `hsl(${hue} ${saturation}% 62%)`;

  return (
    <div
      data-testid="preferences-page"
      className="flex-1 flex flex-col overflow-auto"
      style={{ background: "var(--c-bg)" }}
    >
      <div className="flex-1 flex justify-center overflow-auto px-6 py-8">
        <div className="w-full max-w-md flex flex-col gap-8">

          {/* Color */}
          <section className="flex flex-col gap-4">
            <h2
              className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b"
              style={{ color: "var(--c-text-dim)", borderColor: "var(--c-border)" }}
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

          {/* Notifications */}
          <section className="flex flex-col gap-4">
            <h2
              className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b"
              style={{ color: "var(--c-text-dim)", borderColor: "var(--c-border)" }}
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
          </section>

          {/* Font size */}
          <section className="flex flex-col gap-4">
            <h2
              className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b"
              style={{ color: "var(--c-text-dim)", borderColor: "var(--c-border)" }}
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
