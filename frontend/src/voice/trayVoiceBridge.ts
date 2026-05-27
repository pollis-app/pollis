import { electron, hasElectron } from "../bridge/runtime";
import { useAppStore } from "../stores/appStore";
import { voiceSession } from "./VoiceSessionManager";

interface BridgeHandle {
  dispose: () => void;
}

/**
 * Mirrors voice-call + mute state into the menu-bar / system tray so its
 * "Mute mic" item reflects the real call, and forwards tray mute clicks
 * back to the voice session manager.
 *
 * No-op outside Electron. On Linux/Windows the tray is always present so
 * the voice-state push runs there too; on macOS the tray only exists when
 * the "Show menu bar icon" preference is on, but pushing voice state to a
 * non-existent tray is a harmless no-op in main.ts.
 *
 * Installed once at app boot from App.tsx alongside `installVoiceBridge`.
 */
export function installTrayVoiceBridge(): BridgeHandle {
  if (!hasElectron()) {
    return { dispose: () => {} };
  }

  const api = electron();

  const pushState = (): void => {
    const vs = useAppStore.getState().voiceState;
    const inCall = vs.kind === "joined";
    const muted = inCall ? vs.micMuted : false;
    void api.traySetVoiceState(inCall, muted).catch((err) => {
      console.warn("[tray] traySetVoiceState failed:", err);
    });
  };

  // Push initial state, then on every store change. The selector returns a
  // fresh object reference on every set; we re-derive inCall+muted inside
  // pushState so subscription churn doesn't matter for correctness.
  pushState();
  const unsubStore = useAppStore.subscribe(pushState);

  const unsubToggle = api.trayOnRequestToggleMute(() => {
    void voiceSession.toggleMute();
  });

  return {
    dispose: () => {
      unsubStore();
      unsubToggle();
    },
  };
}
