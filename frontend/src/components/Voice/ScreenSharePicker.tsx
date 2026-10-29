import React, { useEffect, useRef, useState } from "react";
import { Monitor, Square, X } from "lucide-react";
import { observer } from "mobx-react-lite";
import { useTranslation } from "react-i18next";
import { appStore } from "../../stores/appStore";
import {
  friendlyScreenShareError,
  screenShareSession,
  type DisplaySource,
  type Selection,
  type WindowSource,
} from "../../screenshare/screenShareSession";
import { Button } from "../ui/Button";
import { Card } from "../ui/Card";
import { Switch } from "../ui/Switch";

/** In-app screen-share source picker: a full-pane takeover of the participant
 *  grid when `share.kind === 'picking'`, listing displays + windows from the
 *  platform enumerator (Slack/Discord/Zoom's pattern). */
export const ScreenSharePicker: React.FC = observer(() => {
  const { t } = useTranslation("voice");
  // Narrowed via the union; null for the brief frame after state moves on.
  const sources =
    appStore.voiceState.kind === 'joined' && appStore.voiceState.share.kind === 'picking'
      ? appStore.voiceState.share.sources
      : null;
  const shareCancelPicker = appStore.shareCancelPicker;
  const shareStartStarting = appStore.shareStartStarting;
  const shareFailed = appStore.shareFailed;
  const [busy, setBusy] = useState(false);

  // Default Displays: most shares are whole-monitor.
  const [tab, setTab] = useState<"displays" | "windows">("displays");

  // Off by default (unintended sound is a privacy surprise), seeded from the
  // stored preference the Linux path also reads.
  const [withAudio, setWithAudio] = useState(false);
  const audioScope = screenShareSession.audioScope();
  // A flip before the async preference lands wins over it (#1040).
  const audioTouched = useRef(false);

  useEffect(() => {
    let cancelled = false;
    void screenShareSession.resolveWithAudio().then((v) => {
      if (!cancelled && !audioTouched.current) {
        setWithAudio(v);
      }
    });
    return () => {
      cancelled = true;
    };
  }, []);

  function handleAudioToggle(next: boolean) {
    audioTouched.current = true;
    setWithAudio(next);
  }

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !busy) {
        void handleCancel();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [busy]);

  async function handleCancel() {
    setBusy(true);
    try {
      await screenShareSession.cancelPicker();
    } catch (e) {
      console.warn("[screenshare] cancel picker:", e);
    } finally {
      shareCancelPicker();
      setBusy(false);
    }
  }

  async function handlePick(selection: Selection) {
    setBusy(true);
    shareStartStarting(withAudio);
    try {
      await screenShareSession.start(selection, withAudio);
    } catch (e) {
      console.error("[screenshare] start:", e);
      shareFailed(friendlyScreenShareError(String(e)));
    } finally {
      setBusy(false);
    }
  }

  if (sources === null) {
    return null;
  }

  const displays = sources.displays;
  const windows = sources.windows;
  const items = tab === "displays" ? displays : windows;

  return (
    <div
      data-testid="screen-share-picker"
      className="flex-1 flex flex-col font-mono text-xs min-h-0 border-t border-b border-line bg-bg"
    >
      <header
        className="flex items-center justify-between px-3 py-2 border-b border-line text-fg"
      >
        <div className="flex items-center gap-3">
          <span className="text-accent">{t("share.heading")}</span>
          <div className="flex items-center gap-1">
            <Button
              variant={tab === "displays" ? "primary" : "secondary"}
              size="sm"
              onClick={() => setTab("displays")}
            >
              {t("share.displaysTab")}
              <span className="opacity-70">[{displays.length}]</span>
            </Button>
            <Button
              variant={tab === "windows" ? "primary" : "secondary"}
              size="sm"
              onClick={() => setTab("windows")}
            >
              {t("share.windowsTab")}
              <span className="opacity-70">[{windows.length}]</span>
            </Button>
          </div>
        </div>
        <div className="flex items-center gap-3">
          <Switch
            label={t("share.audioToggle")}
            checked={withAudio}
            onChange={handleAudioToggle}
            disabled={busy}
            data-testid="screen-share-audio-toggle"
          />
          <Button
            variant="ghost"
            size="xs"
            onClick={handleCancel}
            disabled={busy}
            aria-label={t("share.cancelLabel")}
            data-testid="screen-share-picker-cancel"
          >
            <X size={12} />
            {t("common:actions.cancel")}
          </Button>
        </div>
      </header>

      {/* What "share sound" captures differs by OS (macOS: the source; Linux
          and Windows: the system mix), so the picker says so. */}
      {withAudio ? (
        <div
          className="px-3 py-2 border-b border-line text-muted"
          data-testid="screen-share-audio-scope"
        >
          {audioScope === "source"
            ? t("share.audioScopeSource")
            : t("share.audioScopeSystem")}{" "}
          {t("share.audioExcludesCall")}
        </div>
      ) : null}

      <div className="flex-1 overflow-auto p-3">
        {items.length === 0 ? (
          <div className="h-full flex items-center justify-center text-muted">
            {tab === "displays" ? t("share.noDisplays") : t("share.noWindows")}
          </div>
        ) : (
          <div
            className="grid gap-2"
            style={{
              gridTemplateColumns:
                "repeat(auto-fill, minmax(180px, 1fr))",
            }}
          >
            {tab === "displays"
              ? displays.map((d) => (
                  <DisplayCard
                    key={d.id}
                    display={d}
                    disabled={busy}
                    onPick={() => handlePick({ kind: "display", id: d.id })}
                  />
                ))
              : windows.map((w) => (
                  <WindowCard
                    key={w.id}
                    window={w}
                    disabled={busy}
                    onPick={() => handlePick({ kind: "window", id: w.id })}
                  />
                ))}
          </div>
        )}
      </div>
    </div>
  );
});

interface SourceCardProps {
  disabled: boolean;
  onPick: () => void;
  testId: string;
  title: string;
  subtitle?: string;
  /** PNG data URL preview; absent on the capture-helper path, which shows `icon`. */
  thumbnail?: string;
  icon: React.ReactNode;
}

const SourceCardShell: React.FC<SourceCardProps> = ({
  disabled,
  onPick,
  testId,
  title,
  subtitle,
  thumbnail,
  icon,
}) => (
  <button
    type="button"
    onClick={onPick}
    disabled={disabled}
    data-testid={testId}
    className="text-start font-mono text-xs disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer focus:outline-none focus:ring-2 focus:ring-accent rounded-[6px]"
    style={{ minHeight: 100 }}
  >
    <Card padding="none" className="flex flex-col items-stretch h-full overflow-hidden">
      <div
        className="flex-1 flex items-center justify-center overflow-hidden bg-bg text-muted"
        style={{
          // Fixed 16:10 (the requested 320×200) keeps the grid even.
          aspectRatio: "16 / 10",
        }}
      >
        {thumbnail ? (
          // alt="": the title carries the label. object-contain avoids cropping.
          <img
            src={thumbnail}
            alt=""
            className="w-full h-full object-contain"
            draggable={false}
          />
        ) : (
          icon
        )}
      </div>
      <div className="p-2">
        <div className="truncate text-fg">
          {title}
        </div>
        {subtitle ? (
          <div
            className="truncate text-muted"
            style={{ fontSize: 10 }}
          >
            {subtitle}
          </div>
        ) : null}
      </div>
    </Card>
  </button>
);

const DisplayCard: React.FC<{
  display: DisplaySource;
  disabled: boolean;
  onPick: () => void;
}> = ({ display, disabled, onPick }) => (
  <SourceCardShell
    disabled={disabled}
    onPick={onPick}
    testId={`screen-share-source-display-${display.id}`}
    title={display.name}
    // No subtitle without real dimensions (0×0 looked broken).
    subtitle={
      display.width > 0 && display.height > 0
        ? `${display.width} × ${display.height}`
        : undefined
    }
    thumbnail={display.thumbnail_data_url}
    icon={<Monitor size={32} />}
  />
);

const WindowCard: React.FC<{
  window: WindowSource;
  disabled: boolean;
  onPick: () => void;
}> = ({ window, disabled, onPick }) => {
  const { t } = useTranslation("voice");
  // No OS title: fall back to the app name.
  const primary = window.title || window.app_name || t("share.untitledWindow");
  const secondary =
    window.title && window.app_name && window.title !== window.app_name
      ? window.app_name
      : undefined;
  return (
    <SourceCardShell
      disabled={disabled}
      onPick={onPick}
      testId={`screen-share-source-window-${window.id}`}
      title={primary}
      // Window size is only knowable after capture starts; never "0 × 0".
      subtitle={secondary}
      thumbnail={window.thumbnail_data_url}
      icon={<Square size={32} />}
    />
  );
};
