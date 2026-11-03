// Profile pictures on disk (lib/avatarStore.ts wired to core, the file system and expo-image).
// Public, hash-checked objects like custom emoji, so they stay cached across launches; the
// directory is wiped when the session ends.

import { AppState } from "react-native";
import * as FileSystem from "expo-file-system/legacy";
import { Image } from "expo-image";
import { invoke } from "./native";
import { createAvatarStore } from "./avatarStore";

export const AVATAR_DIR = `${FileSystem.cacheDirectory ?? ""}pollis-avatars/`;

export const avatarStore = createAvatarStore({
  listFiles: () => FileSystem.readDirectoryAsync(AVATAR_DIR).catch(() => []),
  fileUri: (name) => `${AVATAR_DIR}${name}`,
  resolve: (userId, key, px) =>
    invoke<string>("get_avatar_path", { userId, key, px, destDir: AVATAR_DIR }).then((uri) => {
      if (!uri) {
        throw new Error("get_avatar_path returned no path");
      }
      return uri;
    }),
  fetchKeys: (userIds) =>
    invoke<Array<{ id: string; avatar_url?: string | null }>>("get_user_profiles", { userIds }).then(
      (rows) => rows ?? [],
    ),
  deleteFiles: (uris) => {
    for (const uri of uris) {
      FileSystem.deleteAsync(uri, { idempotent: true }).catch(() => {});
    }
  },
  // One tick: every row rendered in this pass joins the same profile read.
  schedule: (fn) => {
    setTimeout(fn, 0);
  },
  warm: (uris) => {
    Image.prefetch(uris, "memory").catch(() => {});
  },
});

let foregroundHooked = false;

/** Read the directory once per launch and retry failed fetches on each return to the app. */
export function startAvatarCache(): void {
  void avatarStore.hydrate();
  if (!foregroundHooked) {
    foregroundHooked = true;
    AppState.addEventListener("change", (s) => {
      if (s === "active") {
        avatarStore.retryFailed();
      }
    });
  }
}

export async function clearAvatarCache(): Promise<void> {
  avatarStore.clear();
  try {
    await FileSystem.deleteAsync(AVATAR_DIR, { idempotent: true });
  } catch {
    // Best-effort.
  }
}
