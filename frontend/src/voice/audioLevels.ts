// Per-source multi-band audio levels, pushed from Rust at ~20 Hz via the
// `audio_bands` voice event. Kept OUT of the MobX/React-Query state on
// purpose: this is high-frequency cosmetic data (N participants × ~20 Hz)
// and routing it through observable state would thrash every tile 20×/sec.
//
// Instead it's a tiny per-identity pub/sub. The participant tile's live
// waveform subscribes and writes CSS variables straight onto its bar
// element via a ref — so the meter animates with zero React re-renders.
// Mirrors the `livekitView.onStats` / `useScreenShareStats` pattern.

/** Band count — MUST match `levels::BAND_COUNT` in pollis-core. */
export const BAND_COUNT = 3;

type Listener = (bands: number[]) => void;

class AudioLevels {
  private listeners = new Map<string, Set<Listener>>();
  private latest = new Map<string, number[]>();

  subscribe(identity: string, cb: Listener): () => void {
    let set = this.listeners.get(identity);
    if (!set) {
      set = new Set();
      this.listeners.set(identity, set);
    }
    set.add(cb);
    return () => {
      const s = this.listeners.get(identity);
      if (!s) {
        return;
      }
      s.delete(cb);
      if (s.size === 0) {
        this.listeners.delete(identity);
      }
    };
  }

  /** Latest snapshot for an identity, if one has arrived (for seeding a
   *  freshly-mounted subscriber before the next push). */
  get(identity: string): number[] | undefined {
    return this.latest.get(identity);
  }

  /** Called from the voice event handler when an `audio_bands` event lands. */
  push(identity: string, bands: number[]): void {
    this.latest.set(identity, bands);
    const set = this.listeners.get(identity);
    if (!set) {
      return;
    }
    for (const cb of set) {
      cb(bands);
    }
  }

  /** Drop a participant's cached level (e.g. when they leave). */
  clear(identity: string): void {
    this.latest.delete(identity);
  }
}

export const audioLevels = new AudioLevels();
