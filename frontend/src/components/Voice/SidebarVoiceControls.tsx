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

/**
 * How each mic state is *drawn* in refined's sidebar strip.
 *
 * The four states have to be told apart at a glance, so each gets its own
 * shape as well as its own colour — colour alone is not an affordance, and
 * `live` vs `ptt-idle` in particular must not read as the same thing:
 *
 *  - `live`     accent mic on a lit accent-tinted chip — transmitting
 *  - `ptt-idle` dim mic in a dashed outline — armed, waiting for the key
 *  - `muted`    red crossed mic — you muted yourself
 *  - `deafened` red crossed mic *and* the deafen control goes red + crossed
 *
 * Each restates its colour under `hover:` on purpose. `.icon-btn-sm:hover`
 * paints every icon button accent, which on a stateful control is a lie —
 * without this, pointing at a muted mic turns it the same colour as a live
 * one. Hover feedback still lands, via the background.
 */
const MIC_TONE: Record<MicIndicator, string> = {
  live: "bg-active text-accent hover:text-accent",
  "ptt-idle": "border border-dashed border-line-strong text-dim hover:text-dim",
  muted: "text-danger hover:text-danger",
  deafened: "text-danger hover:text-danger",
};

/**
 * Mic + deafen controls for refined's persistent sidebar voice strip
 * (`SidebarProfilePanel`).
 *
 * Parity with terminal's `VoiceBar`, not a copy of it: the same information
 * (the four-state mic indicator) and the same affordances (mute, deafen),
 * drawn in refined's own idiom — ghost `icon-btn-sm` buttons and semantic
 * colour utilities instead of terminal's filled accent pills. Every string
 * comes from the shared `voiceControlLabels`, and the state itself from the
 * shared `micIndicatorOf`, so no surface can describe a state differently
 * from the others.
 */
export const SidebarVoiceControls: React.FC = observer(() => {
  const { t } = useTranslation("voice");
  const { voiceState } = appStore;
  const gate = gateOf(voiceState);
  // Four states, not a bool: deafened and push-to-talk-idle both close the
  // mic without the user having muted themselves, and drawing either as a
  // plain mute is what makes push-to-talk look broken.
  const micIndicator = micIndicatorOf(gate);
  const pttCombo = useShortcutLabel("voice.pushToTalk");
  // Listen-only: joined without a working capture device. The mute toggle
  // becomes a non-interactive "listening only" indicator.
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

      {/* Deafen sits next to mute, as on Discord — deafen implies mute, and
          undeafening restores whatever mute state it displaced. Rendered even
          listen-only: with no capture device there is still incoming audio
          worth silencing, and without this control there is no way back. */}
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
