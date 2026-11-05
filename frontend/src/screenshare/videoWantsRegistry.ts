// What the tiles on screen need from each video track, folded per track: the largest on-screen
// size and fastest paint rate across the tiles that show it, or null once none does. Pure, so
// it is unit-tested; `videoWants.ts` wires one instance to `set_video_wants`.

/** Mirrors `VideoWants` in `pollis-core/src/commands/screenshare/wants.rs`. */
export interface VideoWants {
  width: number;
  height: number;
  fps: number;
}

export type WantsReport = VideoWants | null;

/** A one-shot trailing delay so a burst of resize events folds into one send; never periodic. */
const COALESCE_MS = 50;

export class VideoWantsRegistry {
  private byTrack = new Map<string, Map<number, WantsReport>>();
  private lastSent = new Map<string, string>();
  private dirty = new Set<string>();
  private timer: ReturnType<typeof setTimeout> | null = null;
  private nextId = 1;
  private send: (trackKey: string, wants: WantsReport) => Promise<unknown>;

  constructor(send: (trackKey: string, wants: WantsReport) => Promise<unknown>) {
    this.send = send;
  }

  /** Register a tile for `trackKey`; it counts as hidden until it reports. */
  register(trackKey: string): number {
    const id = this.nextId++;
    let tiles = this.byTrack.get(trackKey);
    if (!tiles) {
      tiles = new Map();
      this.byTrack.set(trackKey, tiles);
    }
    tiles.set(id, null);
    return id;
  }

  report(trackKey: string, id: number, wants: WantsReport): void {
    const tiles = this.byTrack.get(trackKey);
    if (!tiles || !tiles.has(id)) {
      return;
    }
    tiles.set(id, wants);
    this.mark(trackKey);
  }

  unregister(trackKey: string, id: number): void {
    const tiles = this.byTrack.get(trackKey);
    if (!tiles) {
      return;
    }
    tiles.delete(id);
    if (tiles.size === 0) {
      this.byTrack.delete(trackKey);
    }
    this.mark(trackKey);
  }

  /** The folded demand: the max of every reporting tile, or null when none shows the track. */
  aggregate(trackKey: string): WantsReport {
    const tiles = this.byTrack.get(trackKey);
    if (!tiles) {
      return null;
    }
    let out: VideoWants | null = null;
    for (const w of tiles.values()) {
      if (!w) {
        continue;
      }
      out = out
        ? {
            width: Math.max(out.width, w.width),
            height: Math.max(out.height, w.height),
            fps: Math.max(out.fps, w.fps),
          }
        : { ...w };
    }
    return out;
  }

  private mark(trackKey: string): void {
    this.dirty.add(trackKey);
    if (this.timer !== null) {
      return;
    }
    this.timer = setTimeout(() => {
      this.timer = null;
      this.flush();
    }, COALESCE_MS);
  }

  /** Send every changed aggregate now (the coalescing timer calls this). */
  flush(): void {
    if (this.timer !== null) {
      clearTimeout(this.timer);
      this.timer = null;
    }
    const keys = [...this.dirty];
    this.dirty.clear();
    for (const trackKey of keys) {
      const wants = this.aggregate(trackKey);
      const encoded = wants ? `${wants.width}x${wants.height}@${wants.fps}` : "hidden";
      if (this.lastSent.get(trackKey) === encoded) {
        continue;
      }
      if (wants || this.byTrack.has(trackKey)) {
        this.lastSent.set(trackKey, encoded);
      } else {
        // The last tile left: say "hidden" once, then forget the key so a tile that
        // reappears for it (the same track key after a remount) reports afresh.
        this.lastSent.delete(trackKey);
      }
      this.send(trackKey, wants).catch((e) => {
        console.warn("[video] set_video_wants", trackKey, e);
      });
    }
  }
}
