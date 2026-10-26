import * as SecureStore from "expo-secure-store";
import type { AccentBackend } from "./accentStore";

// The accent's device-local store (#1447): expo-secure-store, which the
// language choice and the auto-lock timeout already use. Its `getItem` is
// synchronous, which is what lets theme/tokens.ts pick the accent up while
// the module loads, before the first frame. Writes go async: nothing waits
// on them. AFTER_FIRST_UNLOCK so a launch while the phone is locked (a
// background push wake) still reads it; a colour is not a secret.
export const deviceAccentBackend: AccentBackend = {
  getItem: (key) => SecureStore.getItem(key),
  setItem: (key, value) =>
    SecureStore.setItemAsync(key, value, {
      keychainAccessible: SecureStore.AFTER_FIRST_UNLOCK,
    }),
};
