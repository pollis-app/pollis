// The background task that clears read notifications from the tray (#1508).
//
// When the user reads a conversation on any client, the DS sends this
// account's devices one silent, data-only push `{ d: [handles] }` — no title,
// no body, nothing the OS shows. expo-notifications runs this task for it in
// every app state (Android: foreground, background and killed, where it loads
// the JS bundle headless; iOS: foreground and background, and not after the
// user force-quits — the foreground catch-up in `useUnreadSync` covers that).
//
// It must be defined at module load, before the app renders, which is why the
// app entry (`mobile/index.ts`) imports this file first. It imports only
// expo-notifications and expo-task-manager: no native bridge, no store, no
// network — a killed, locked app has none of those, and it needs none, because
// the handles in the payload are matched against the tray directly.
//
// A build without the expo-task-manager native module (any binary from before
// this change) never runs an OTA update carrying this file: a new native
// module is a new runtime version. The try/catch is for the dev client and
// tests, not for that.

import * as Notifications from "expo-notifications";
import * as TaskManager from "expo-task-manager";
import { pickDismissals, readDismissPayload, type Presented } from "./dismiss";

export const DISMISS_TASK = "pollis-dismiss-read-notifications";

/** Clear every tray notification whose handle is in `handles`. */
export async function dismissHandles(handles: readonly string[]): Promise<number> {
  if (handles.length === 0) {
    return 0;
  }
  const presented = await Notifications.getPresentedNotificationsAsync();
  const tray: Presented[] = presented.map((n) => ({
    identifier: n.request.identifier,
    data: n.request.content.data,
  }));
  const ids = pickDismissals(tray, handles);
  await Promise.all(
    ids.map((id) =>
      Notifications.dismissNotificationAsync(id).catch(() => {
        // Already gone (the user swiped it, or a second dismissal raced
        // this one): nothing to do.
      }),
    ),
  );
  return ids.length;
}

try {
  TaskManager.defineTask<Notifications.NotificationTaskPayload>(
    DISMISS_TASK,
    async ({ data, error }) => {
      if (error) {
        return Notifications.BackgroundNotificationTaskResult.Failed;
      }
      // A tap on a notification action also lands here on Android; it carries
      // no `d`, so it is ignored like any ordinary message push.
      const handles = readDismissPayload(data);
      if (handles === null) {
        return Notifications.BackgroundNotificationTaskResult.NoData;
      }
      try {
        await dismissHandles(handles);
        return Notifications.BackgroundNotificationTaskResult.NewData;
      } catch {
        return Notifications.BackgroundNotificationTaskResult.Failed;
      }
    },
  );
  void Notifications.registerTaskAsync(DISMISS_TASK).catch(() => {
    // No native task support in this build: the foreground catch-up and the
    // same-device dismissal still clear the tray.
  });
} catch {
  // As above.
}
