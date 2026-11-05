// Per-identity pub/sub for `audio_bands` levels, kept out of observable state; tiles write CSS vars via refs.
// The backend batches every participant into one event per 50 ms tick (`levels::LevelsHub`).

import { userIdFromVoiceIdentity } from "./identity";

/** Band count — MUST match `levels::BAND_COUNT` in pollis-core. */
export const BAND_COUNT = 3;

/** One participant's entry in an `audio_bands` event (`AudioBandsEntry` in pollis-core). */
export interface AudioBandsEntry {
  identity: string;
  bands: number[];
}

type Listener = (bands: number[]) => void;

class AudioLevels {
  private listeners = new Map<string, Set<Listener>>();
  private latest = new Map<string, number[]>();

  // Keyed by user id to match user-scoped tiles (and the synthesized local identity).
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

  /** Latest snapshot, to seed a new subscriber. */
  get(identity: string): number[] | undefined {
    return this.latest.get(this.key(identity));
  }

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

  /** One batched tick: every entry the backend currently has levels for. */
  pushBatch(levels: AudioBandsEntry[]): void {
    for (const entry of levels) {
      this.push(entry.identity, entry.bands);
    }
  }

  /** Drop a participant's cached level (e.g. when they leave). */
  clear(identity: string): void {
    this.latest.delete(this.key(identity));
  }
}

export const audioLevels = new AudioLevels();
