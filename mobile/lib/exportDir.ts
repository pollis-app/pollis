// Where the on-device export (#856) writes its plaintext archives; OS-evictable cache on purpose.

import * as FileSystem from "expo-file-system/legacy";

export const EXPORT_DIR = `${FileSystem.cacheDirectory ?? ""}pollis-export/`;

// Decrypted history must not outlive the session that made it (#1256).
export async function clearExportArchives(): Promise<void> {
  await FileSystem.deleteAsync(EXPORT_DIR, { idempotent: true });
}
