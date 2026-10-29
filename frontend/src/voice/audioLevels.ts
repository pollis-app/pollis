// Per-source audio band levels (~20 Hz from Rust), kept OUT of observable
// state so tiles don't re-render 20×/sec: a per-identity pub/sub whose
// subscribers write CSS variables via a ref.

import { userIdFromVoiceIdentity } from "./identity";

/** Band count — MUST match `levels::BAND_COUNT` in pollis-core. */
export const BAND_COUNT = 3;

type Listener = (bands: number[]) => void;

class AudioLevels {
  private listeners = new Map<string, Set<Listener>>();
  private latest = new Map<string, number[]>();

  // Keyed by user id, matching the user-scoped tiles; the local tile's
  // synthesized identity may not match the device-suffixed one the backend
  // emits, but the user id does.
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

  /** Latest snapshot, for seeding a fresh subscriber. */
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

  clear(identity: string): void {
    this.latest.delete(this.key(identity));
  }
}

export const audioLevels = new AudioLevels();
