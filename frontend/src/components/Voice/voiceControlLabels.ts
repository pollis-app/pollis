// Shared mic/deafen title keys (#849), so every surface agrees. Surfaces call `t()` themselves.

import type { MicIndicator } from "../../types/voice-state";

/** `ptt-idle` isn't "muted". */
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
