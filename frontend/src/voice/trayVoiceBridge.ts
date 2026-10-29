import { autorun } from "mobx";
import { setTrayVoiceState, onTrayRequestToggleMute } from "../bridge/tray";
import { appStore } from "../stores/appStore";
import { voiceSession } from "./VoiceSessionManager";

interface BridgeHandle {
  dispose: () => void;
}

/**
 * Mirrors call + mute state into the tray's "Mute mic" item and forwards its
 * clicks to the voice session. Pushing to an absent macOS tray is a no-op.
 * Installed once from App.tsx.
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

  // `autorun` pushes now and whenever the voice state it reads changes.
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
