import React, { useEffect, useState } from "react";
import { Video, X } from "lucide-react";
import { observer } from "mobx-react-lite";
import { useTranslation } from "react-i18next";

import { appStore } from "../../stores/appStore";
import { cameraSession, friendlyCameraError } from "../../camera/cameraSession";
import type { CameraSource } from "../../camera/types";
import { Button } from "../ui/Button";
import { SourceCard } from "./SourceCard";

/** Inline webcam picker replacing the participant grid (no modal); only with more than one camera. */
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
    // The parked helper is reaped by the next list/start/stop.
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
      className="flex-1 flex flex-col font-mono text-secondary min-h-0 border-t border-b border-line bg-bg"
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
        <div className="grid gap-2 grid-cols-[repeat(auto-fill,minmax(11.25rem,1fr))]">
          {cameras.map((c) => (
            <SourceCard
              key={c.id}
              onPick={() => handlePick(c)}
              disabled={busy}
              testId={`camera-picker-device-${c.id}`}
              title={c.name}
              icon={<Video size={32} />}
            />
          ))}
        </div>
      </div>
    </div>
  );
});
