// Where the on-device export (#856) writes the archive, its `-files/` dir and
// the shared zip, all plaintext. Under `cacheDirectory` on purpose: the share
// sheet has already copied the zip wherever it went.

import * as FileSystem from "expo-file-system/legacy";

export const EXPORT_DIR = `${FileSystem.cacheDirectory ?? ""}pollis-export/`;

// Called when a session ends and at startup (#1256): an archive is decrypted
// history and must not outlive its session.
export async function clearExportArchives(): Promise<void> {
  await FileSystem.deleteAsync(EXPORT_DIR, { idempotent: true });
}
