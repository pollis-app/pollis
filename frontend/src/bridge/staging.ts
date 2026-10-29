/** Attachment staging (`src-tauri/src/commands/staging.rs`): pasted bytes live in backend memory, never a temp file. */

import { invoke } from "./invoke";

export interface StagedAttachment {
  /** Opaque handle; pass to `upload_media_staged` or `discardStagedAttachment`. */
  id: string;
  size_bytes: number;
}

/** Stage bytes, sent as the raw IPC body (not a JSON array). */
export async function stageAttachment(
  bytes: Uint8Array,
): Promise<StagedAttachment> {
  return invoke<StagedAttachment>("stage_attachment", bytes);
}

/** Release staged bytes the composer won't send; uploads, lock, logout and wipe also release. */
export async function discardStagedAttachment(id: string): Promise<boolean> {
  return invoke<boolean>("discard_staged_attachment", { stagedId: id });
}
