// The one place a mobile session ends (#1256). Sign-out, account deletion
// and the forced sign-out of a revoked device all route through
// `endSession()`, so everything a session leaves behind on the device — UI
// state, the query cache, decrypted media, export archives, the emoji
// cache — is cleaned up in one spot rather than at each call site.
// `sweepStalePlaintext()` is the startup half: what a crashed run left.

import { router } from "expo-router";
import { appStore } from "../../stores/appStore";
import { queryClient } from "../queryClient";
import { clearMediaCache } from "../media/cache";
import { clearExportArchives } from "../exportDir";
import { clearEmojiCache } from "../emojiCache";
import { oncePerProcess, sweepPlaintext, tearDownSession } from "./teardown";

export function endSession(): Promise<void> {
  return tearDownSession({
    resetStore: appStore.logout,
    leaveSignedInScreens: () => {
      // Pop the whole stack first: a plain replace swaps only the top
      // screen and leaves e.g. an open chat mounted underneath sign-in.
      if (router.canDismiss()) {
        router.dismissAll();
      }
      router.replace("/(auth)/email");
    },
    clearQueryCache: async () => {
      await queryClient.cancelQueries();
      queryClient.clear();
    },
    clearMediaCache,
    clearExportArchives,
    clearEmojiCache,
  });
}

export const sweepStalePlaintext = oncePerProcess(
  sweepPlaintext({ clearMediaCache, clearExportArchives }),
);
