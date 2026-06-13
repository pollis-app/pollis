import { autorun } from "mobx";
import { setTrayVoiceState, onTrayRequestToggleMute } from "../bridge/tray";
import { appStore } from "../stores/appStore";
import { voiceSession } from "./VoiceSessionManager";

interface BridgeHandle {
  dispose: () => void;
}

/**
 * Mirrors voice-call + mute state into the menu-bar / system tray so its
 * "Mute mic" item reflects the real call, and forwards tray mute clicks
 * back to the voice session manager.
 *
 * Works under both runtimes via the tray bridge. On Linux/Windows the tray
 * is always present so the voice-state push runs there too; on macOS the
 * tray only exists when the "Show menu bar icon" preference is on, but
 * pushing voice state to a non-existent tray is a harmless no-op.
 *
 * Installed once at app boot from App.tsx alongside `installVoiceBridge`.
 */
export function installTrayVoiceBridge(): BridgeHandle {
  const pushState = (): void => {
    const vs = appStore.voiceState;
    const inCall = vs.kind === "joined";
    const muted = inCall ? vs.micMuted : false;
    void setTrayVoiceState(inCall, muted).catch((err) => {
      console.warn("[tray] setTrayVoiceState failed:", err);
    });
  };

  // `autorun` pushes once immediately and re-runs whenever the voice state it
  // reads changes. We re-derive inCall+muted inside pushState each run so
  // reactivity churn doesn't matter for correctness.
  const unsubStore = autorun(pushState);

  const unsubToggle = onTrayRequestToggleMute(() => {
    void voiceSession.toggleMute();
  });

  return {
    dispose: () => {
      unsubStore();
      unsubToggle();
    },
  };
}
