import * as SecureStore from "expo-secure-store";
import type { AccentBackend } from "./accentStore";

// expo-secure-store: its `getItem` is synchronous, so theme/tokens.ts can read
// the accent before the first frame (#1447). AFTER_FIRST_UNLOCK so a launch
// while the phone is locked (a background push wake) still reads it; a colour
// is not a secret.
export const deviceAccentBackend: AccentBackend = {
  getItem: (key) => SecureStore.getItem(key),
  setItem: (key, value) =>
    SecureStore.setItemAsync(key, value, {
      keychainAccessible: SecureStore.AFTER_FIRST_UNLOCK,
    }),
};
