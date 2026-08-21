/**
 * Attachment staging bridge — `stage_attachment` and
 * `discard_staged_attachment` (`src-tauri/src/commands/staging.rs`).
 *
 * A pasted or dragged-in file arrives in the webview as a `File` with no
 * filesystem path, and the upload path needs somewhere to read it from. That
 * used to be a copy written into the OS temp directory under the file's own
 * name, which nothing ever deleted — so the plaintext of every file a user had
 * ever pasted was still there. These two calls replace it: the bytes go
 * straight into the backend's memory and come back out only as an upload.
 *
 * The whole point is that no path exists, so nothing here returns one.
 */

import { invoke } from "./invoke";

export interface StagedAttachment {
  /** Opaque handle; pass to `upload_media_staged` or `discardStagedAttachment`. */
  id: string;
  size_bytes: number;
}

/**
 * Hand the backend an attachment's bytes. Invoked with the `Uint8Array` as the
 * raw IPC body — not a JSON number array, which for a multi-megabyte
 * screenshot is the difference between a paste and a stall.
 */
export async function stageAttachment(
  bytes: Uint8Array,
): Promise<StagedAttachment> {
  return invoke<StagedAttachment>("stage_attachment", bytes);
}

/**
 * Release staged bytes the composer is no longer going to send — the user
 * removed the attachment card, or the composer unmounted still holding it.
 *
 * A successful upload releases its own entry, so this is not the only thing
 * standing between a paste and a leak; `lock`, `logout` and `wipe_local_data`
 * release everything, and process exit frees the lot with nothing on disk.
 */
export async function discardStagedAttachment(id: string): Promise<boolean> {
  return invoke<boolean>("discard_staged_attachment", { stagedId: id });
}
