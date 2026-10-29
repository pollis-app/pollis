/**
 * Attachment staging (`src-tauri/src/commands/staging.rs`): pasted/dropped
 * bytes go into backend memory, never a temp file, so nothing here returns a
 * path.
 */

import { invoke } from "./invoke";

export interface StagedAttachment {
  /** Opaque handle; pass to `upload_media_staged` or `discardStagedAttachment`. */
  id: string;
  size_bytes: number;
}

/**
 * Hand the backend an attachment's bytes, as the raw IPC body (a JSON number
 * array stalls on multi-megabyte screenshots).
 */
export async function stageAttachment(
  bytes: Uint8Array,
): Promise<StagedAttachment> {
  return invoke<StagedAttachment>("stage_attachment", bytes);
}

/**
 * Release staged bytes the composer won't send. Uploads release their own
 * entry; `lock`, `logout` and `wipe_local_data` release everything.
 */
export async function discardStagedAttachment(id: string): Promise<boolean> {
  return invoke<boolean>("discard_staged_attachment", { stagedId: id });
}
