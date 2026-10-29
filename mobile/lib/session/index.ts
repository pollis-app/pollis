// The one place a mobile session ends (#1256): sign-out, account deletion and a
// revoked device's forced sign-out all call `endSession()`.
// `sweepStalePlaintext()` is the startup half, for what a crashed run left.

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
      // Pop the whole stack: a plain replace leaves e.g. an open chat mounted
      // underneath sign-in.
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
