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

/** Inline in-app picker for screen-share sources. Replaces the voice
 *  participant grid when `share.kind === 'picking'` — no modal, no
 *  overlay, just a full-pane takeover that gives the user a grid of
 *  displays + windows. The source list comes from each platform's
 *  enumerator (macOS: SCShareableContent in the helper subprocess;
 *  Windows: windows-rs Monitor/Window enumeration + GDI thumbnails).
 *  Industry-standard pattern — what Slack/Discord/Zoom do. */
export const ScreenSharePicker: React.FC = observer(() => {
  const { t } = useTranslation("voice");
  // Picker only renders when shareState.kind === 'picking', so sources are
  // guaranteed present. Narrowed via the union; bail to null defensively
  // for the brief frame where state may have transitioned away.
  const sources =
    appStore.voiceState.kind === 'joined' && appStore.voiceState.share.kind === 'picking'
      ? appStore.voiceState.share.sources
      : null;
  const shareCancelPicker = appStore.shareCancelPicker;
  const shareStartStarting = appStore.shareStartStarting;
  const shareFailed = appStore.shareFailed;
  const [busy, setBusy] = useState(false);

  // Tab between Displays and Windows. Default to Displays — most
  // screen shares are whole-monitor.
  const [tab, setTab] = useState<"displays" | "windows">("displays");

  // Off by default, matching Slack/Discord/Zoom. Sharing sound you did not
  // mean to share is a privacy surprise, so it is always an explicit act.
  // Seeded from the stored preference so the choice sticks between shares
  // — the same preference the Linux path reads, since it has no picker.
  const [withAudio, setWithAudio] = useState(false);
  const audioScope = screenShareSession.audioScope();
  // The stored preference arrives asynchronously. If the user flips the
  // switch before it lands, their explicit choice wins over the remembered
  // default (#1040) — the preference only seeds an untouched switch.
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

  // Esc cancels (matches the rest of the app's modal-replacement flows).
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

      {/* What "share sound" captures genuinely differs by OS — macOS
          follows the picked source, Linux and Windows take the system
          mix — so the picker states it rather than letting the user
          discover it after the fact. The second sentence is the one
          people actually worry about. */}
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
  /** PNG data URL — when present, renders as the tile preview. When
   *  absent (Tauri capture helper path, which doesn't ship preview
   *  frames), the `icon` is shown instead. */
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
          // Fixed aspect for thumbnails so the grid stays even when
          // sources have wildly different aspect ratios (an ultra-wide
          // monitor next to a portrait phone screen sharer, etc).
          // 16:10 matches the 320×200 thumbnail size we request in main.
          aspectRatio: "16 / 10",
        }}
      >
        {thumbnail ? (
          // alt="" because the title below carries the accessible label.
          // object-contain rather than cover so we don't crop windows
          // whose aspect ratio differs from the thumbnail frame.
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
    // Suppress the dim subtitle when the backend didn't supply real
    // dimensions (0×0 looked broken; "—" would be noise) — i.e. under
    // capture-helper paths that don't enumerate display sizes.
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
  // Title fallback: most chat apps name a window after their conversation;
  // if the OS gave us no title, use the app name.
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
      // Never show "0 × 0" for window sources; the thumbnail is the
      // primary visual identifier, and per-window size is only knowable
      // after capture starts.
      subtitle={secondary}
      thumbnail={window.thumbnail_data_url}
      icon={<Square size={32} />}
    />
  );
};
