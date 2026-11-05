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
  /** Self mute / unmute / deafen / undeafen cues (Discord's clicks). `play_sfx` ignores a name it has no asset for. */
  mute: 'mute',
  unmute: 'unmute',
  deafen: 'deafen',
  undeafen: 'undeafen',
} as const;

export type SfxName = (typeof SFX)[keyof typeof SFX];
