// Lightweight local-tone player for short UI cues. Uses HTMLAudioElement
// against files served from `frontend/public/sounds/`, so dropping a real
// `.mp3` (or any web-audio format) into that directory enables sound with
// no code change.
//
// This is intentionally separate from the rodio-backed `sfx.ts` system:
// those sounds are pre-embedded into the Rust binary via `include_bytes!`
// (compile-time), and adding a new sound there requires editing Rust. Short
// transient UI tones (screenshare on/off, etc.) can use the webview's
// audio path safely — they're small, single-shot, and have no GC pressure
// concerns.
//
// All playback is wrapped in try/catch and `.play()` rejections are
// swallowed: a missing file, blocked autoplay, or absent audio device must
// never throw into the caller or surface to the user.

const SOUND_PATHS = {
  screenshare_start: "/sounds/screenshare-start.mp3",
  screenshare_stop: "/sounds/screenshare-stop.mp3",
} as const;

export type SoundName = keyof typeof SOUND_PATHS;

export function playSound(name: SoundName): void {
  try {
    const audio = new Audio(SOUND_PATHS[name]);
    audio.volume = 0.5;
    const result = audio.play();
    if (result && typeof result.catch === "function") {
      result.catch(() => {
        // Missing file, blocked autoplay, or no audio device — silent no-op.
      });
    }
  } catch {
    // HTMLAudioElement unavailable (unlikely in webview) — silent no-op.
  }
}
