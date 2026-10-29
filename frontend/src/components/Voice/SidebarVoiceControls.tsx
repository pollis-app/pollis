import React from "react";
import { observer } from "mobx-react-lite";
import { useTranslation } from "react-i18next";
import { Mic, MicOff, Volume2, VolumeX } from "lucide-react";
import { appStore } from "../../stores/appStore";
import { voiceSession } from "../../voice";
import { gateOf, micIndicatorOf, type MicIndicator } from "../../types/voice-state";
import { useShortcutLabel } from "../../keyboard";
import {
  micControlTitleKey,
  micControlAriaLabelKey,
  deafenControlTitleKey,
  deafenControlAriaLabelKey,
} from "./voiceControlLabels";

/** Each mic state gets its own shape and colour; colours are restated under `hover:` since `.icon-btn-sm:hover` paints accent. */
const MIC_TONE: Record<MicIndicator, string> = {
  live: "bg-active text-accent hover:text-accent",
  "ptt-idle": "border border-dashed border-line-strong text-dim hover:text-dim",
  muted: "text-danger hover:text-danger",
  deafened: "text-danger hover:text-danger",
};

/** Mic + deafen for refined's sidebar strip; labels and state shared with `VoiceBar`. */
export const SidebarVoiceControls: React.FC = observer(() => {
  const { t } = useTranslation("voice");
  const { voiceState } = appStore;
  const gate = gateOf(voiceState);
  // Four states: PTT-idle and deafened close the mic too.
  const micIndicator = micIndicatorOf(gate);
  const pttCombo = useShortcutLabel("voice.pushToTalk");
  // Listen-only: joined with no capture device.
  const micAvailable = voiceState.kind === "joined" ? voiceState.micAvailable : true;

  return (
    <>
      {micAvailable ? (
        <button
          type="button"
          data-testid="sidebar-voice-mute"
          data-mic-state={micIndicator}
          onClick={() => voiceSession.toggleMute()}
          aria-label={t(micControlAriaLabelKey(micIndicator), { combo: pttCombo })}
          title={t(micControlTitleKey(micIndicator), { combo: pttCombo })}
          className={`icon-btn-sm ${MIC_TONE[micIndicator]}`}
        >
          {micIndicator === "live" || micIndicator === "ptt-idle" ? (
            <Mic size={15} className="size-[0.933rem] shrink-0" />
          ) : (
            <MicOff size={15} className="size-[0.933rem] shrink-0" />
          )}
        </button>
      ) : (
        <button
          type="button"
          data-testid="sidebar-voice-listen-only"
          data-mic-state="listen-only"
          aria-label={t("controls.listenOnly")}
          title={t("controls.listenOnly")}
          className="icon-btn-sm text-dim hover:text-dim disabled:cursor-default"
          disabled
        >
          <MicOff size={15} className="size-[0.933rem] shrink-0" />
        </button>
      )}

      {/* Deafen, shown even listen-only. */}
      <button
        type="button"
        data-testid="sidebar-voice-deafen"
        data-deafened={gate.deafened}
        onClick={() => voiceSession.toggleDeafen()}
        aria-label={t(deafenControlAriaLabelKey(gate.deafened))}
        title={t(deafenControlTitleKey(gate.deafened))}
        className={`icon-btn-sm ${gate.deafened ? "text-danger hover:text-danger" : ""}`}
      >
        {gate.deafened ? (
          <VolumeX size={15} className="size-[0.933rem] shrink-0" />
        ) : (
          <Volume2 size={15} className="size-[0.933rem] shrink-0" />
        )}
      </button>
    </>
  );
});
