// Titles / aria-labels for the mic and deafen controls (#849).
//
// Shared by the two surfaces that draw them — the global `VoiceBar` and
// the in-channel `VoiceStage` tray — so the two can never describe the
// same state differently. Only the strings live here; each surface keeps
// its own icon sizing and styling.

import type { MicIndicator } from "../../types/voice-state";

/**
 * The four mic states, spelled out. `ptt-idle` gets its own wording on
 * purpose: the user has *not* muted themselves, so calling it "muted"
 * would make push-to-talk look broken.
 */
export function micControlTitle(indicator: MicIndicator, pttCombo: string): string {
  switch (indicator) {
    case "live":
      return "Mute microphone";
    case "muted":
      return "Unmute microphone";
    case "deafened":
      return "Deafened — undeafen to unmute";
    case "ptt-idle":
      return `Push to talk — hold ${pttCombo}`;
  }
}

export function micControlAriaLabel(
  indicator: MicIndicator,
  pttCombo: string,
): string {
  switch (indicator) {
    case "live":
      return "Microphone live. Mute microphone";
    case "muted":
      return "Microphone muted. Unmute microphone";
    case "deafened":
      return "Deafened. Microphone muted";
    case "ptt-idle":
      return `Push to talk armed. Hold ${pttCombo} to transmit`;
  }
}

export function deafenControlTitle(deafened: boolean): string {
  return deafened ? "Undeafen" : "Deafen (mutes everyone you hear)";
}

export function deafenControlAriaLabel(deafened: boolean): string {
  return deafened ? "Undeafen" : "Deafen";
}
