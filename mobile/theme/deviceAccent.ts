import * as SecureStore from "expo-secure-store";
import type { AccentBackend } from "./accentStore";

// expo-secure-store (#1447): its synchronous `getItem` is read at module load. AFTER_FIRST_UNLOCK: a colour is not a secret.
export const deviceAccentBackend: AccentBackend = {
  getItem: (key) => SecureStore.getItem(key),
  setItem: (key, value) =>
    SecureStore.setItemAsync(key, value, {
      keychainAccessible: SecureStore.AFTER_FIRST_UNLOCK,
    }),
};
