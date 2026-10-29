// The group last selected on the Groups tab, device-local and user-scoped.
// Best-effort: a failed read or write falls back to the first group.

import * as SecureStore from "expo-secure-store";

function key(userId: string): string {
  // SecureStore keys allow only [A-Za-z0-9._-].
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
