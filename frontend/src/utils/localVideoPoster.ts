// A video from a sender older than #1837 carries no `pv` poster, so this device decodes a frame
// itself. #1894: the core bounds that frame and saves it in the local encrypted DB, keyed by the
// attachment's content hash, so the video is fetched and decoded once, not on every render.
// Pure, with its effects passed in, so a test can count decodes.

/** Mirrors the Rust `SavedVideoPoster` in `pollis-core/src/commands/poster.rs`. */
export type SavedVideoPoster = {
  /** The bounded poster JPEG as base64, or null when none fit; the blurhash stands in. */
  jpeg: string | null;
  blurhash: string;
  width: number;
  height: number;
};

/**
 * What a tile draws for a video with no `pv`: a JPEG, else a blurhash. `from` says whether it
 * was read from the saved row ("saved") or decoded for this lookup ("decoded").
 */
export type LocalVideoPoster = { jpeg: string | null; blurhash: string | null; from: "saved" | "decoded" };

export interface LocalVideoPosterDeps {
  /** `get_local_video_poster`. */
  get(contentHash: string): Promise<SavedVideoPoster | null>;
  /** Fetch and decode the video; a frame as base64 JPEG/PNG, or null when there is none. */
  decode(): Promise<string | null>;
  /** `save_local_video_poster`. */
  save(contentHash: string, frame: string): Promise<SavedVideoPoster>;
}

// One lookup per video at a time: tiles of the same video mounting together (a remount, the
// message and the Vault grid) share it, so they decode once between them.
const inFlight = new Map<string, Promise<LocalVideoPoster | null>>();

/**
 * The poster for a video with no `pv`: the saved one, else a frame decoded now and saved.
 * A video that has `pv` resolves to null without touching the store, so it never gets a row.
 */
export function localVideoPoster(
  attachment: { poster?: string; content_hash: string },
  deps: LocalVideoPosterDeps,
): Promise<LocalVideoPoster | null> {
  const hash = attachment.content_hash;
  if (attachment.poster || !hash) {
    return Promise.resolve(null);
  }
  const running = inFlight.get(hash);
  if (running) {
    return running;
  }
  const lookup = savedOrDecoded(hash, deps).finally(() => {
    inFlight.delete(hash);
  });
  inFlight.set(hash, lookup);
  return lookup;
}

async function savedOrDecoded(hash: string, deps: LocalVideoPosterDeps): Promise<LocalVideoPoster | null> {
  const saved = await deps.get(hash).catch(() => null);
  if (saved) {
    return { jpeg: saved.jpeg, blurhash: saved.blurhash, from: "saved" };
  }
  const frame = await deps.decode().catch(() => null);
  if (!frame) {
    return null;
  }
  try {
    const made = await deps.save(hash, frame);
    return { jpeg: made.jpeg, blurhash: made.blurhash, from: "decoded" };
  } catch {
    // Not saved (the core refused the frame, or the DB is closed): still draw this frame.
    return { jpeg: frame, blurhash: null, from: "decoded" };
  }
}
