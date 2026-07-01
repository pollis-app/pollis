import React, { useEffect, useState } from "react";
import { Video, X } from "lucide-react";
import { observer } from "mobx-react-lite";

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
      className="flex-1 flex flex-col font-mono text-xs min-h-0"
      style={{
        borderTop: "1px solid var(--c-border)",
        borderBottom: "1px solid var(--c-border)",
        background: "var(--c-bg)",
      }}
    >
      <header
        className="flex items-center justify-between px-3 py-2"
        style={{ borderBottom: "1px solid var(--c-border)", color: "var(--c-text)" }}
      >
        <span style={{ color: "var(--c-accent)" }} className="flex items-center gap-2">
          <Video size={13} /> Choose a camera
        </span>
        <Button
          variant="ghost"
          size="xs"
          onClick={handleCancel}
          disabled={busy}
          aria-label="Cancel camera selection"
          data-testid="camera-picker-cancel"
        >
          <X size={12} />
          Cancel
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
              className="text-left font-mono text-xs disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer focus:outline-none focus:ring-2 focus:ring-[var(--c-accent)] rounded-[6px]"
              style={{ minHeight: 100 }}
            >
              <Card padding="none" className="flex flex-col items-stretch h-full overflow-hidden">
                <div
                  className="flex-1 flex items-center justify-center overflow-hidden"
                  style={{
                    aspectRatio: "16 / 10",
                    background: "var(--c-bg)",
                    color: "var(--c-text-muted)",
                  }}
                >
                  <Video size={32} />
                </div>
                <div className="p-2">
                  <div className="truncate" style={{ color: "var(--c-text)" }} title={c.name}>
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
