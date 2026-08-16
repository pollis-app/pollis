import React, { useEffect, useState } from "react";
import { Video, X } from "lucide-react";
import { observer } from "mobx-react-lite";
import { useTranslation } from "react-i18next";

import { appStore } from "../../stores/appStore";
import { cameraSession, friendlyCameraError } from "../../camera/cameraSession";
import type { CameraSource } from "../../camera/types";
import { Button } from "../ui/Button";
import { Card } from "../ui/Card";

/** Inline in-app picker for webcam devices. Replaces the voice participant
 *  grid when `camera.kind === 'picking'` — no modal, no overlay, just a
 *  full-pane takeover (CLAUDE.md rule), mirroring `ScreenSharePicker`. Only
 *  shown when there's more than one camera; the single-camera case starts
 *  directly without a picker (see `cameraActions`). */
export const CameraPicker: React.FC = observer(() => {
  const { t } = useTranslation("voice");
  const cameras =
    appStore.voiceState.kind === "joined" &&
    appStore.voiceState.camera.kind === "picking"
      ? appStore.voiceState.camera.cameras
      : null;
  const cameraCancelPicker = appStore.cameraCancelPicker;
  const cameraStartStarting = appStore.cameraStartStarting;
  const cameraFailed = appStore.cameraFailed;
  const [busy, setBusy] = useState(false);

  // Esc cancels — matches the screen-share picker + the app's other
  // modal-replacement flows.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !busy) {
        handleCancel();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [busy]);

  function handleCancel() {
    // The enumerate call parks a helper waiting for our pick; cancelling the
    // picker without starting leaves it to be reaped by the next
    // list/start/stop. Reset UI state immediately.
    cameraCancelPicker();
  }

  async function handlePick(camera: CameraSource) {
    setBusy(true);
    cameraStartStarting();
    try {
      await cameraSession.start(camera.id);
    } catch (e) {
      console.error("[camera] start:", e);
      cameraFailed(friendlyCameraError(String(e)));
    } finally {
      setBusy(false);
    }
  }

  if (cameras === null) {
    return null;
  }

  return (
    <div
      data-testid="camera-picker"
      className="flex-1 flex flex-col font-mono text-xs min-h-0 border-t border-b border-line bg-bg"
    >
      <header
        className="flex items-center justify-between px-3 py-2 border-b border-line text-fg"
      >
        <span className="flex items-center gap-2 text-accent">
          <Video size={13} /> {t("camera.pickerHeading")}
        </span>
        <Button
          variant="ghost"
          size="xs"
          onClick={handleCancel}
          disabled={busy}
          aria-label={t("camera.cancelLabel")}
          data-testid="camera-picker-cancel"
        >
          <X size={12} />
          {t("common:actions.cancel")}
        </Button>
      </header>

      <div className="flex-1 overflow-auto p-3">
        <div
          className="grid gap-2"
          style={{ gridTemplateColumns: "repeat(auto-fill, minmax(180px, 1fr))" }}
        >
          {cameras.map((c) => (
            <button
              key={c.id}
              type="button"
              onClick={() => handlePick(c)}
              disabled={busy}
              data-testid={`camera-picker-device-${c.id}`}
              className="text-start font-mono text-xs disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer focus:outline-none focus:ring-2 focus:ring-accent rounded-[6px]"
              style={{ minHeight: 100 }}
            >
              <Card padding="none" className="flex flex-col items-stretch h-full overflow-hidden">
                <div
                  className="flex-1 flex items-center justify-center overflow-hidden bg-bg text-muted"
                  style={{
                    aspectRatio: "16 / 10",
                  }}
                >
                  <Video size={32} />
                </div>
                <div className="p-2">
                  <div className="truncate text-fg" title={c.name}>
                    {c.name}
                  </div>
                </div>
              </Card>
            </button>
          ))}
        </div>
      </div>
    </div>
  );
});
