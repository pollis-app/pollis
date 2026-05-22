import { invoke } from '@tauri-apps/api/core';

/**
 * Play a named sound effect via the Rust backend.
 *
 * Audio is played by rodio on the host audio device, bypassing WebKit's
 * GStreamer pipeline entirely (which is unreliable on Linux).
 * Errors are silently swallowed — a missing audio device should never
 * surface to the user.
 */
export function playSfx(sound: string): void {
  invoke('play_sfx', { sound }).catch(() => {});
}
