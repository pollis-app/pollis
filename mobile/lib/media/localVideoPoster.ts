// A video from a sender older than #1837 carries no `pv` poster, so this device decodes a frame
// itself. #1894: the core bounds that frame and saves it in the local encrypted DB, keyed by the
// attachment's content hash, so the video is fetched and decoded once, not on every render.
// Pure, with its effects passed in, so a test can count decodes. Desktop's twin is
// `frontend/src/utils/localVideoPoster.ts` (a separate pnpm project, so not shared).

/** Mirrors the Rust `SavedVideoPoster` in `pollis-core/src/commands/poster.rs`. */
export type SavedVideoPoster = {
  /** The bounded poster JPEG as base64, or null when none fit; the blurhash stands in. */
  jpeg: string | null;
  blurhash: string;
  width: number;
  height: number;
};

/** What a tile draws for a video with no `pv`: a JPEG, else a blurhash. */
export type LocalVideoPoster = { jpeg: string | null; blurhash: string | null };

export interface LocalVideoPosterDeps {
  /** `get_local_video_poster`. */
  get(contentHash: string): Promise<SavedVideoPoster | null>;
  /** Fetch and decode the video; a frame as base64 JPEG/PNG, or null when there is none. */
  decode(): Promise<string | null>;
  /** `save_local_video_poster`. */
  save(contentHash: string, frame: string): Promise<SavedVideoPoster>;
}

/**
 * The poster for a video with no `pv`: the saved one, else a frame decoded now and saved.
 * A video that has `pv` resolves to null without touching the store, so it never gets a row.
 */
export async function localVideoPoster(
  attachment: { poster?: string; content_hash: string },
  deps: LocalVideoPosterDeps,
): Promise<LocalVideoPoster | null> {
  if (attachment.poster || !attachment.content_hash) {
    return null;
  }
  const saved = await deps.get(attachment.content_hash).catch(() => null);
  if (saved) {
    return { jpeg: saved.jpeg, blurhash: saved.blurhash };
  }
  const frame = await deps.decode();
  if (!frame) {
    return null;
  }
  try {
    const made = await deps.save(attachment.content_hash, frame);
    return { jpeg: made.jpeg, blurhash: made.blurhash };
  } catch {
    // Not saved (the core refused the frame, or the DB is closed): still draw this frame.
    return { jpeg: frame, blurhash: null };
  }
}
