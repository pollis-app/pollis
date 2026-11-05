// Optimistic gate transitions, pure so `tests/voice-gate-predict.test.ts` can pin them against `TransmitGate` (gate.rs, #849).

import type { VoiceGateState } from '../types/voice-state';

/** Gate commands whose optimistic result the renderer draws on the click frame; Rust's snapshot replaces it. */
export type GateCommand =
  | 'toggle_voice_mute'
  | 'toggle_voice_deafen'
  | 'set_voice_input_mode'
  | 'set_voice_ptt_held'
  | 'release_voice_ptt'
  | 'get_voice_gate_state';

/** Toggles coalesce: a second click while the first is in flight is dropped, so rapid clicks can't double-toggle. */
export const COALESCED_GATE_COMMANDS: ReadonlySet<GateCommand> = new Set<GateCommand>([
  'toggle_voice_mute',
  'toggle_voice_deafen',
]);

function deriveTransmitting(gate: Omit<VoiceGateState, 'transmitting'>): boolean {
  if (gate.self_muted) {
    return false;
  }
  return gate.mode === 'voice_activity' || gate.ptt_held;
}

/**
 * Predict the snapshot `command` yields. `mutedBeforeDeafen` is Rust's memory of the mute an
 * undeafen restores; null when unknown (reload mid-call), in which case the undeafen opens the mic
 * and Rust's reply corrects it a round-trip later.
 */
export function predictGate(
  gate: VoiceGateState,
  command: GateCommand,
  args: Record<string, unknown> | undefined,
  mutedBeforeDeafen: boolean | null,
): VoiceGateState | null {
  let next: Omit<VoiceGateState, 'transmitting'>;
  switch (command) {
    case 'toggle_voice_mute': {
      const muted = !gate.self_muted;
      next = {
        ...gate,
        self_muted: muted,
        // Unmuting while deafened undeafens too (`deafened ⇒ self_muted`).
        deafened: !muted && gate.deafened ? false : gate.deafened,
      };
      break;
    }
    case 'toggle_voice_deafen': {
      next = gate.deafened
        ? { ...gate, deafened: false, self_muted: mutedBeforeDeafen ?? false }
        : { ...gate, deafened: true, self_muted: true, ptt_held: false };
      break;
    }
    case 'set_voice_input_mode': {
      const mode = args?.mode;
      if (mode !== 'voice_activity' && mode !== 'push_to_talk') {
        return null;
      }
      next = { ...gate, mode, ptt_held: false };
      break;
    }
    case 'set_voice_ptt_held': {
      next = { ...gate, ptt_held: args?.held === true };
      break;
    }
    case 'release_voice_ptt': {
      next = { ...gate, ptt_held: false };
      break;
    }
    case 'get_voice_gate_state':
      return null;
  }
  return { ...next, transmitting: deriveTransmitting(next) };
}
