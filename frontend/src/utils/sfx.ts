import { invoke } from '../bridge';
import { logIgnored } from './log';

/**
 * Play a named sound effect in Rust (rodio), bypassing WebKit's unreliable
 * Linux GStreamer path. Errors are swallowed.
 */
export function playSfx(sound: string): void {
  invoke('play_sfx', { sound }).catch(logIgnored);
}

export const SFX = {
  /** Incoming message while the window is unfocused. */
  ping: 'ping',
  join: 'join',
  leave: 'leave',
} as const;
