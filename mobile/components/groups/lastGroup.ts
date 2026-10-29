// The Groups tab's last group, remembered per user; best-effort.

import * as SecureStore from "expo-secure-store";

function key(userId: string): string {
  return `pollis.lastGroup.${userId.replace(/[^A-Za-z0-9._-]/g, "_")}`;
}

export async function readLastGroupId(userId: string): Promise<string | null> {
  try {
    return await SecureStore.getItemAsync(key(userId));
  } catch {
    return null;
  }
}

export function writeLastGroupId(userId: string, groupId: string): void {
  SecureStore.setItemAsync(key(userId), groupId).catch(() => {});
}
