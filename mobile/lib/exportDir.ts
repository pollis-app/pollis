// Where the on-device export (#856) writes its archives. Everything
// `export_archive`, `fetch_export_attachments` and `bundle_export` produce
// lands under here: the archive, its sibling `-files/` dir and the zip the
// share sheet is handed — all plaintext.
//
// Under `cacheDirectory` on purpose: the OS may evict it, and the share
// sheet has already copied the zip wherever the user sent it.

import * as FileSystem from "expo-file-system/legacy";

export const EXPORT_DIR = `${FileSystem.cacheDirectory ?? ""}pollis-export/`;

// Delete every export archive. Called when a session ends and at startup
// (#1256): an archive is decrypted message history, so it must not outlive
// the session that made it.
export async function clearExportArchives(): Promise<void> {
  await FileSystem.deleteAsync(EXPORT_DIR, { idempotent: true });
}
