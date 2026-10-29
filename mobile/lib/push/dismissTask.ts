// Background task that clears read notifications on the DS's silent
// `{ d: [handles] }` push (#1508; `.codesight/wiki/notifications.md`). Defined
// at module load from the app entry (`mobile/index.ts`), because Android may
// load the bundle headless just to run it. Imports no bridge, store or network:
// a killed, locked app has none, and the handles match the tray directly.

import * as Notifications from "expo-notifications";
import * as TaskManager from "expo-task-manager";
import { pickDismissals, readDismissPayload, type Presented } from "./dismiss";

export const DISMISS_TASK = "pollis-dismiss-read-notifications";

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
        // Already gone (swiped, or a racing dismissal).
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
      // Android also routes notification-action taps here; they carry no `d`.
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
    // No native task support (dev client, tests): the other two paths still clear the tray.
  });
} catch {
  // As above.
}
