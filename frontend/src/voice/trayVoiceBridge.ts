import { autorun } from "mobx";
import { setTrayVoiceState, onTrayRequestToggleMute } from "../bridge/tray";
import { appStore } from "../stores/appStore";
import { voiceSession } from "./VoiceSessionManager";

interface BridgeHandle {
  dispose: () => void;
}

/** Mirror call + mute state into the tray and forward its mute clicks; install once at boot. */
export function installTrayVoiceBridge(): BridgeHandle {
  const pushState = (): void => {
    const vs = appStore.voiceState;
    const inCall = vs.kind === "joined";
    const muted = inCall ? vs.micMuted : false;
    void setTrayVoiceState(inCall, muted).catch((err) => {
      console.warn("[tray] setTrayVoiceState failed:", err);
    });
  };

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
