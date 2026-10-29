// Translation keys for the mic and deafen controls (#849), shared by VoiceBar,
// the VoiceStage tray and SidebarVoiceControls. Callers run `t()` themselves.
// PTT keys interpolate `{{combo}}`.

import type { MicIndicator } from "../../types/voice-state";

/** `ptt-idle` has its own wording: calling it "muted" makes PTT look broken. */
export function micControlTitleKey(indicator: MicIndicator): string {
  switch (indicator) {
    case "live":
      return "controls.micTitleLive";
    case "muted":
      return "controls.micTitleMuted";
    case "deafened":
      return "controls.micTitleDeafened";
    case "ptt-idle":
      return "controls.micTitlePushToTalk";
  }
}

export function micControlAriaLabelKey(indicator: MicIndicator): string {
  switch (indicator) {
    case "live":
      return "controls.micLabelLive";
    case "muted":
      return "controls.micLabelMuted";
    case "deafened":
      return "controls.micLabelDeafened";
    case "ptt-idle":
      return "controls.micLabelPushToTalk";
  }
}

export function deafenControlTitleKey(deafened: boolean): string {
  return deafened ? "controls.undeafenTitle" : "controls.deafenTitle";
}

export function deafenControlAriaLabelKey(deafened: boolean): string {
  return deafened ? "controls.undeafenLabel" : "controls.deafenLabel";
}
