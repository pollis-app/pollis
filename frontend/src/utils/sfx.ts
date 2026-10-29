import { invoke } from '../bridge';
import { logIgnored } from './log';

/** Play a sound via Rust (rodio; WebKit audio is unreliable on Linux). Errors are swallowed. */
export function playSfx(sound: string): void {
  invoke('play_sfx', { sound }).catch(logIgnored);
}

export const SFX = {
  /** Incoming DM or channel message when window is unfocused. */
  ping: 'ping',
  /** Another user joined a voice channel. */
  join: 'join',
  /** Another user left a voice channel. */
  leave: 'leave',
} as const;
