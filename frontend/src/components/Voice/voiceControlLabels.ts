// Title / aria-label translation KEYS for the mic and deafen controls (#849).
//
// Shared by the surfaces that draw them — the global `VoiceBar`, the
// in-channel `VoiceStage` tray and refined's `SidebarVoiceControls` — so the
// three can never describe the same state differently. Only the key selection
// lives here; each surface keeps its own icon sizing and styling, and calls
// `t()` itself (a key resolved here would snapshot the language at import
// time). Keys live in the `voice` namespace.
//
// The push-to-talk keys interpolate `{{combo}}`; passing it unconditionally is
// harmless for the keys that don't use it.

import type { MicIndicator } from "../../types/voice-state";

/**
 * The four mic states, spelled out. `ptt-idle` gets its own wording on
 * purpose: the user has *not* muted themselves, so calling it "muted"
 * would make push-to-talk look broken.
 */
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
