// Per-source multi-band audio levels, pushed from Rust at ~20 Hz via the
// `audio_bands` voice event. Kept OUT of the MobX/React-Query state on
// purpose: this is high-frequency cosmetic data (N participants × ~20 Hz)
// and routing it through observable state would thrash every tile 20×/sec.
//
// Instead it's a tiny per-identity pub/sub. The participant tile's live
// waveform subscribes and writes CSS variables straight onto its bar
// element via a ref — so the meter animates with zero React re-renders.
// Mirrors the `livekitView.onStats` / `useScreenShareStats` pattern.

import { userIdFromVoiceIdentity } from "./identity";

/** Band count — MUST match `levels::BAND_COUNT` in pollis-core. */
export const BAND_COUNT = 3;

type Listener = (bands: number[]) => void;

class AudioLevels {
  private listeners = new Map<string, Set<Listener>>();
  private latest = new Map<string, number[]>();

  // Levels are keyed by *user id*, not the full device identity, to match the
  // user-scoped tile model (VoiceStage merges a user's devices into one tile;
  // the speaking indicator is likewise user-scoped). This is also what fixes
  // the local meter: the local tile's identity is synthesized on the frontend
  // and may not byte-match the device-suffixed identity the backend emits
  // `audio_bands` under, but the user id always matches. Remote bands collapse
  // per user (latest device wins), consistent with the merged tile.
  private key(identity: string): string {
    return userIdFromVoiceIdentity(identity);
  }

  subscribe(identity: string, cb: Listener): () => void {
    const k = this.key(identity);
    let set = this.listeners.get(k);
    if (!set) {
      set = new Set();
      this.listeners.set(k, set);
    }
    set.add(cb);
    return () => {
      const s = this.listeners.get(k);
      if (!s) {
        return;
      }
      s.delete(cb);
      if (s.size === 0) {
        this.listeners.delete(k);
      }
    };
  }

  /** Latest snapshot for an identity, if one has arrived (for seeding a
   *  freshly-mounted subscriber before the next push). */
  get(identity: string): number[] | undefined {
    return this.latest.get(this.key(identity));
  }

  /** Called from the voice event handler when an `audio_bands` event lands. */
  push(identity: string, bands: number[]): void {
    const k = this.key(identity);
    this.latest.set(k, bands);
    const set = this.listeners.get(k);
    if (!set) {
      return;
    }
    for (const cb of set) {
      cb(bands);
    }
  }

  /** Drop a participant's cached level (e.g. when they leave). */
  clear(identity: string): void {
    this.latest.delete(this.key(identity));
  }
}

export const audioLevels = new AudioLevels();
